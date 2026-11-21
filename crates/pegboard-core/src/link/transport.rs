//! The physical connection: text messages in and out over a websocket or a NUL-delimited pipe.
//!
//! A message over the limit does not end the connection (an agent can make the browser send
//! one, e.g. a huge string evaluated by value): it is reduced to its first and last bytes
//! (`Incoming::Oversized`), from which the multiplexer finds the one virtual connection it
//! belongs to and ends only that one.

use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Bytes kept of each end of an oversized message: enough for a leading `{"id":N` and a
/// trailing `,"sessionId":"…"}`, which is where Chrome puts them.
pub(crate) const KEPT: usize = 256;

/// A message from the browser.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    Text(String),
    /// A message over the limit, given up: only its first and last `KEPT` bytes.
    Oversized {
        head: Vec<u8>,
        tail: Vec<u8>,
    },
}

impl Incoming {
    /// `text` as it is, or reduced to its ends when it is longer than `max`.
    pub(crate) fn limited(text: String, max: usize) -> Incoming {
        if text.len() <= max {
            return Incoming::Text(text);
        }
        let bytes = text.as_bytes();
        Incoming::Oversized {
            head: bytes[..KEPT.min(bytes.len())].to_vec(),
            tail: bytes[bytes.len().saturating_sub(KEPT)..].to_vec(),
        }
    }
}

/// One physical connection. `incoming` ends when the connection does (closed by the other
/// side, a failed read or write); dropping `outgoing` closes it.
pub(crate) struct Transport {
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
    pub outgoing: mpsc::UnboundedSender<String>,
}

/// The browser's DevTools websocket. A message over `max` is handed on as `Oversized`. The
/// websocket itself takes a message of any size: tungstenite cannot skip one it refuses (the
/// connection would end), so the whole message is read before it is reduced.
pub(crate) async fn websocket(url: &str, max: usize) -> Result<Transport> {
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
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
                        let message = Incoming::limited(text.as_str().to_owned(), max);
                        drop(text);
                        if in_tx.send(message).is_err() {
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
/// A message over `max` is skipped up to its NUL, keeping only its ends (`Oversized`); the
/// connection goes on. Needs a tokio runtime.
pub(crate) fn pipe(read: OwnedFd, write: OwnedFd, max: usize) -> Result<Transport> {
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
        let mut frames = NulFrames::new(max);
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            let n = tokio::select! {
                n = reader.read(&mut chunk) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                },
                _ = &mut writer_ended => break,
            };
            for message in frames.push(&chunk[..n]) {
                if in_tx.send(message).is_err() {
                    return;
                }
            }
        }
    });
    Ok(Transport { incoming, outgoing })
}

/// Splits a byte stream into NUL-ended messages. Memory stays within `max` plus what one read
/// brings: a message over `max` is not kept, only its ends.
struct NulFrames {
    /// The message so far (no NUL in it), while it is within `max`.
    buf: Vec<u8>,
    max: usize,
    /// The ends of the message being skipped, once it went over `max`.
    skipping: Option<(Vec<u8>, Vec<u8>)>,
}

impl NulFrames {
    fn new(max: usize) -> NulFrames {
        NulFrames {
            buf: Vec::new(),
            max,
            skipping: None,
        }
    }

    /// The messages `bytes` completes. A message that is not UTF-8 is left out.
    fn push(&mut self, mut bytes: &[u8]) -> Vec<Incoming> {
        let mut messages = Vec::new();
        loop {
            let nul = bytes.iter().position(|&b| b == 0);
            let part = &bytes[..nul.unwrap_or(bytes.len())];
            match &mut self.skipping {
                Some((_, tail)) => keep_tail(tail, part),
                None if self.buf.len() + part.len() > self.max => {
                    let buf = std::mem::take(&mut self.buf);
                    let head = buf.iter().chain(part).take(KEPT).copied().collect();
                    let mut tail = Vec::new();
                    keep_tail(&mut tail, &buf);
                    keep_tail(&mut tail, part);
                    self.skipping = Some((head, tail));
                }
                None => self.buf.extend_from_slice(part),
            }
            let Some(at) = nul else {
                return messages;
            };
            match self.skipping.take() {
                Some((head, tail)) => messages.push(Incoming::Oversized { head, tail }),
                None => match String::from_utf8(std::mem::take(&mut self.buf)) {
                    Ok(text) => messages.push(Incoming::Text(text)),
                    Err(_) => super::mux::dropped("a pipe message is not UTF-8", None),
                },
            }
            bytes = &bytes[at + 1..];
        }
    }
}

/// Appends `part` to `tail`, keeping its last `KEPT` bytes.
fn keep_tail(tail: &mut Vec<u8>, part: &[u8]) {
    tail.extend_from_slice(&part[part.len().saturating_sub(KEPT)..]);
    let extra = tail.len().saturating_sub(KEPT);
    tail.drain(..extra);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::io::{Read, Write};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    const WAIT: Duration = Duration::from_secs(3);

    /// A small limit, so no test allocates the real 256 MiB.
    const MAX: usize = 1024;

    async fn next_incoming(t: &mut Transport) -> Option<Incoming> {
        tokio::time::timeout(WAIT, t.incoming.recv())
            .await
            .expect("no message within 3s")
    }

    async fn next(t: &mut Transport) -> Option<String> {
        match next_incoming(t).await? {
            Incoming::Text(text) => Some(text),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A transport on two fresh pipes, and the browser's ends of them.
    fn pipes() -> (Transport, std::io::PipeWriter, std::io::PipeReader) {
        let (daemon_read, browser_write) = std::io::pipe().unwrap();
        let (browser_read, daemon_write) = std::io::pipe().unwrap();
        let t = pipe(daemon_read.into(), daemon_write.into(), MAX).unwrap();
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
        let mut exact = vec![b'a'; MAX];
        exact.push(0);
        to_daemon.write_all(&exact).unwrap();
        assert_eq!(next(&mut t).await.map(|m| m.len()), Some(MAX));
    }

    #[tokio::test]
    async fn an_oversized_pipe_frame_is_skipped() {
        let (mut t, mut to_daemon, _from_daemon) = pipes();
        // Over the limit by one byte, written in pieces (its ends cross the pieces), then a
        // normal message right behind its NUL.
        let mut over = b"{\"id\":7,\"result\":{\"v\":\"".to_vec();
        over.resize(MAX + 1 - b"\"}},\"sessionId\":\"S1\"}".len(), b'x');
        over.extend_from_slice(b"\"}},\"sessionId\":\"S1\"}");
        assert_eq!(over.len(), MAX + 1);
        for piece in over.chunks(100) {
            to_daemon.write_all(piece).unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        to_daemon.write_all(b"\0{\"after\":1}\0").unwrap();
        let Some(Incoming::Oversized { head, tail }) = next_incoming(&mut t).await else {
            panic!("the oversized message did not come out reduced");
        };
        assert_eq!(head, over[..KEPT]);
        assert_eq!(tail, over[over.len() - KEPT..]);
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"after\":1}"));

        // Many times the limit in one write, between two normal messages: memory stays
        // small and the connection goes on.
        let mut many = b"{\"m\":1}\0".to_vec();
        many.extend(std::iter::repeat_n(b'y', MAX * 50));
        many.extend_from_slice(b"\0{\"n\":2}\0");
        // From a thread: more than a pipe buffer holds.
        let writer = std::thread::spawn(move || to_daemon.write_all(&many).unwrap());
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"m\":1}"));
        assert!(matches!(
            next_incoming(&mut t).await,
            Some(Incoming::Oversized { .. })
        ));
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"n\":2}"));
        writer.join().unwrap();
    }

    #[test]
    fn a_long_text_is_reduced_to_its_ends() {
        let text = format!("{{\"id\":3,{}}}", "z".repeat(2000));
        assert_eq!(
            Incoming::limited(text.clone(), text.len()),
            Incoming::Text(text.clone())
        );
        let Incoming::Oversized { head, tail } = Incoming::limited(text.clone(), MAX) else {
            panic!("not reduced");
        };
        assert_eq!(head, text.as_bytes()[..KEPT]);
        assert_eq!(tail, text.as_bytes()[text.len() - KEPT..]);
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
            // Over the limit: reduced, and the connection goes on.
            ws.send(Message::text(format!(
                "{{\"big\":\"{}\"}}",
                "q".repeat(MAX)
            )))
            .await
            .unwrap();
            let got = ws.next().await.unwrap().unwrap();
            assert_eq!(got.to_text().unwrap(), "{\"back\":2}");
            // Closing ends the daemon's side.
            ws.close(None).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let mut t = websocket(&format!("ws://{addr}"), MAX).await.unwrap();
        assert_eq!(next(&mut t).await.as_deref(), Some("{\"hello\":1}"));
        assert!(matches!(
            next_incoming(&mut t).await,
            Some(Incoming::Oversized { .. })
        ));
        t.outgoing.send("{\"back\":2}".into()).unwrap();
        assert_eq!(next(&mut t).await, None);
        server.await.unwrap();
    }
}
