//! The physical connection: text messages in and out over a websocket or a NUL-delimited pipe.

use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::cdp::QUEUED_MAX_MESSAGE;

/// One physical connection. `incoming` ends when the connection does (closed by the other
/// side, a failed read or write, a message over the limit); dropping `outgoing` closes it.
pub(crate) struct Transport {
    pub incoming: mpsc::UnboundedReceiver<String>,
    pub outgoing: mpsc::UnboundedSender<String>,
}

/// The browser's DevTools websocket, taking messages up to 256 MiB like a queued `CdpConn`.
pub(crate) async fn websocket(url: &str) -> Result<Transport> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(QUEUED_MAX_MESSAGE))
        .max_frame_size(Some(QUEUED_MAX_MESSAGE));
    let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    let (mut sink, mut stream) = ws.split();
    let (in_tx, incoming) = mpsc::unbounded_channel();
    let (outgoing, mut out_rx) = mpsc::unbounded_channel::<String>();
    // Each task drops its sender when it ends, which ends the other: the connection ends
    // as a whole.
    let (writer_done, mut writer_ended) = oneshot::channel::<()>();
    let (reader_done, mut reader_ended) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let _writer_done = writer_done;
        loop {
            tokio::select! {
                text = out_rx.recv() => match text {
                    Some(text) => {
                        if sink.send(Message::text(text)).await.is_err() {
                            return;
                        }
                    }
                    None => break,
                },
                _ = &mut reader_ended => break,
            }
        }
        // Sends our close (or the reply to the browser's, which tungstenite queued).
        let _ = sink.close().await;
    });
    tokio::spawn(async move {
        let _reader_done = reader_done;
        loop {
            tokio::select! {
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        if in_tx.send(text.as_str().to_owned()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                _ = &mut writer_ended => break,
            }
        }
    });
    Ok(Transport { incoming, outgoing })
}

/// A DevTools pipe: reads messages from `read`, writes them to `write`, each ended by a NUL.
/// A message over 256 MiB ends the connection, as on the websocket. Needs a tokio runtime.
pub(crate) fn pipe(read: OwnedFd, write: OwnedFd) -> Result<Transport> {
    let mut reader = pipe::Receiver::from_owned_fd(read).context("the read end is not a pipe")?;
    let mut writer = pipe::Sender::from_owned_fd(write).context("the write end is not a pipe")?;
    let (in_tx, incoming) = mpsc::unbounded_channel();
    let (outgoing, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (writer_done, mut writer_ended) = oneshot::channel::<()>();
    let (reader_done, mut reader_ended) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let _writer_done = writer_done;
        loop {
            let text = tokio::select! {
                text = out_rx.recv() => match text {
                    Some(text) => text,
                    None => return,
                },
                _ = &mut reader_ended => return,
            };
            let mut bytes = text.into_bytes();
            bytes.push(0);
            if writer.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    tokio::spawn(async move {
        let _reader_done = reader_done;
        let mut frames = NulFrames::new(QUEUED_MAX_MESSAGE);
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            let n = tokio::select! {
                n = reader.read(&mut chunk) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                },
                _ = &mut writer_ended => break,
            };
            let Some(messages) = frames.push(&chunk[..n]) else {
                break;
            };
            for message in messages {
                if in_tx.send(message).is_err() {
                    return;
                }
            }
        }
    });
    Ok(Transport { incoming, outgoing })
}

/// Splits a byte stream into NUL-ended messages.
struct NulFrames {
    buf: Vec<u8>,
    /// Bytes of `buf` already searched for a NUL.
    scanned: usize,
    max: usize,
}

impl NulFrames {
    fn new(max: usize) -> NulFrames {
        NulFrames {
            buf: Vec::new(),
            scanned: 0,
            max,
        }
    }

    /// The messages `bytes` completes; `None` once a message is longer than `max` (the
    /// connection must end). A message that is not UTF-8 is left out.
    fn push(&mut self, bytes: &[u8]) -> Option<Vec<String>> {
        self.buf.extend_from_slice(bytes);
        let mut messages = Vec::new();
        let mut start = 0;
        while let Some(at) = self.buf[self.scanned..].iter().position(|&b| b == 0) {
            let end = self.scanned + at;
            if end - start > self.max {
                return None;
            }
            match std::str::from_utf8(&self.buf[start..end]) {
                Ok(text) => messages.push(text.to_owned()),
                Err(_) => super::mux::dropped("a pipe message is not UTF-8", None),
            }
            start = end + 1;
            self.scanned = start;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        if self.buf.len() > self.max {
            return None;
        }
        Some(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdp::QUEUED_MAX_MESSAGE;
    use futures_util::{SinkExt, StreamExt};
    use std::io::{Read, Write};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    const WAIT: Duration = Duration::from_secs(3);

    async fn next(t: &mut Transport) -> Option<String> {
        tokio::time::timeout(WAIT, t.incoming.recv())
            .await
            .expect("no message within 3s")
    }

    /// A transport on two fresh pipes, and the browser's ends of them.
    fn pipes() -> (Transport, std::io::PipeWriter, std::io::PipeReader) {
        let (daemon_read, browser_write) = std::io::pipe().unwrap();
        let (browser_read, daemon_write) = std::io::pipe().unwrap();
        let t = pipe(daemon_read.into(), daemon_write.into()).unwrap();
        (t, browser_write, browser_read)
    }

    #[tokio::test]
    async fn pipe_frames_are_nul_delimited() {
        let (mut t, mut to_daemon, mut from_daemon) = pipes();

        // Two messages in one write come out as two.
        to_daemon.write_all(b"{\"a\":1}\0{\"b\":2}\0").unwrap();
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"a\":1}"));
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"b\":2}"));

        // A message split over several writes comes out whole, once its NUL arrives.
        to_daemon.write_all(b"{\"c\":").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(t.incoming.try_recv().is_err(), "half a message came out");
        to_daemon.write_all(b"3}").unwrap();
        to_daemon.write_all(b"\0{\"d\"").unwrap();
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"c\":3}"));
        to_daemon.write_all(b":4}\0").unwrap();
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"d\":4}"));

        // What the daemon sends is written with a NUL after each message.
        t.outgoing.send("{\"x\":1}".into()).unwrap();
        t.outgoing.send("{\"y\":2}".into()).unwrap();
        let want = b"{\"x\":1}\0{\"y\":2}\0";
        let mut got = vec![0u8; want.len()];
        tokio::task::spawn_blocking(move || {
            from_daemon.read_exact(&mut got).unwrap();
            got
        })
        .await
        .map(|got| assert_eq!(&got[..], &want[..]))
        .unwrap();

        // A message of exactly the limit passes.
        let mut big = vec![b'a'; QUEUED_MAX_MESSAGE];
        big.push(0);
        let writer = std::thread::spawn(move || {
            to_daemon.write_all(&big).unwrap();
            to_daemon
        });
        let got = tokio::time::timeout(Duration::from_secs(60), t.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.len(), QUEUED_MAX_MESSAGE);
        drop(got);
        let mut to_daemon = writer.join().unwrap();

        // One byte more and the connection ends; the browser's writes then fail.
        let writer = std::thread::spawn(move || {
            let over = vec![b'a'; QUEUED_MAX_MESSAGE + 1];
            let first = to_daemon.write_all(&over);
            let later = first.and_then(|_| {
                std::thread::sleep(Duration::from_millis(200));
                to_daemon.write_all(b"\0{}\0")
            });
            later.is_err()
        });
        let end = tokio::time::timeout(Duration::from_secs(60), t.incoming.recv())
            .await
            .expect("the connection did not end");
        assert_eq!(end, None);
        assert!(writer.join().unwrap(), "the pipe stayed open");
    }

    #[tokio::test]
    async fn pipe_eof_ends_the_connection() {
        let (mut t, to_daemon, _from_daemon) = pipes();
        drop(to_daemon);
        assert_eq!(next(&mut t).await, None);
    }

    #[tokio::test]
    async fn websocket_carries_text_both_ways() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::text("{\"hello\":1}")).await.unwrap();
            let got = ws.next().await.unwrap().unwrap();
            assert_eq!(got.to_text().unwrap(), "{\"back\":2}");
            // Closing ends the daemon's side.
            ws.close(None).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let mut t = websocket(&format!("ws://{addr}")).await.unwrap();
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"hello\":1}"));
        t.outgoing.send("{\"back\":2}".into()).unwrap();
        assert_eq!(next(&mut t).await, None);
        server.await.unwrap();
    }
}
