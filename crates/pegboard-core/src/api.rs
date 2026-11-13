//! The core API: one JSON-RPC message per line over a unix socket in the pegboard home.
//!
//! The socket is mode 0600 inside the 0700 home, and a peer with another uid is dropped
//! (`peer_cred`, which is `getpeereid` on macOS).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use pegboard_protocol::{codes, Request, Response, RpcError, JSONRPC};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use crate::lock::DaemonLock;
use crate::paths::Paths;
use crate::service::Daemon;

/// Longest request line accepted.
const MAX_LINE: u64 = 4 * 1024 * 1024;

/// Removes the socket file when the server stops (also when its future is dropped).
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What `serve_with` needs from the thing it serves.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, req: Request) -> Response;
    /// Called after the answer to a successful `browser.stop` was written.
    fn request_stop(&self) {}
    /// Called when the answer to `req` could not be written (the caller went away). The
    /// handler decides which methods need cleaning up after.
    async fn delivery_failed(&self, _req: &Request, _resp: &Response) {}
}

#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// Connections served at once; a further one is accepted and closed at once.
    pub max_connections: usize,
    /// A connection that sends no complete request line within this time is closed. It
    /// applies before each request, not while one is being handled.
    pub idle_timeout: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            max_connections: 64,
            idle_timeout: Duration::from_secs(10),
        }
    }
}

/// Serves the API until the future is dropped; its socket file is removed then. The caller
/// holds the daemon lock (`lock::DaemonLock`), so a socket file found here is stale.
pub async fn serve(paths: &Paths, daemon: Arc<Daemon>) -> anyhow::Result<()> {
    serve_with(paths, daemon, ServeOptions::default()).await
}

pub async fn serve_with(
    paths: &Paths,
    handler: Arc<dyn Handler>,
    opts: ServeOptions,
) -> anyhow::Result<()> {
    match std::fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("removing the stale socket"),
    }
    let listener = UnixListener::bind(&paths.socket)
        .with_context(|| format!("binding {}", paths.socket.display()))?;
    let _file = SocketFile(paths.socket.clone());
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))
        .context("restricting the socket to its owner")?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let slots = Arc::new(Semaphore::new(opts.max_connections));
    loop {
        // An accept error (e.g. out of descriptors) must not end the server.
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("pegboard: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {}
            _ => continue,
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            // Over the cap: one error line, then the stream is dropped (closed).
            tokio::spawn(refuse_busy(stream));
            continue;
        };
        let handler = handler.clone();
        tokio::spawn(async move {
            connection(stream, handler, opts.idle_timeout).await;
            drop(permit);
        });
    }
}

async fn refuse_busy(stream: UnixStream) {
    let (_, mut write) = stream.into_split();
    let r = error_response(0, codes::INTERNAL, "pegboard is busy; try again");
    let _ = tokio::time::timeout(Duration::from_secs(1), write_line(&mut write, &r)).await;
}

fn error_response(id: u64, code: &str, message: impl Into<String>) -> Response {
    Response {
        jsonrpc: JSONRPC.to_string(),
        id,
        result: None,
        error: Some(RpcError {
            code: code.to_string(),
            message: message.into(),
            data: None,
        }),
    }
}

async fn connection(stream: UnixStream, daemon: Arc<dyn Handler>, idle_timeout: Duration) {
    let (read, mut write) = stream.into_split();
    let mut read = tokio::io::BufReader::new(read);
    loop {
        let mut line = Vec::new();
        // Reads at most MAX_LINE + 1 bytes: one more means the line is too long.
        let mut limited = (&mut read).take(MAX_LINE + 1);
        let next = limited.read_until(b'\n', &mut line);
        match tokio::time::timeout(idle_timeout, next).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
            Ok(Ok(_)) => {}
        }
        if line.len() as u64 > MAX_LINE {
            let r = error_response(0, codes::INVALID_PARAMS, "request line too long");
            let _ = write_line(&mut write, &r).await;
            return;
        }
        let (response, stop, request) = match serde_json::from_slice::<Request>(&line) {
            Ok(req) => {
                let (id, stop) = (req.id, req.method == "browser.stop");
                let kept = req.clone();
                let d = daemon.clone();
                // A task of its own: a panic becomes an error, and the work finishes even
                // if this connection goes away.
                let r = tokio::spawn(async move { d.handle(req).await })
                    .await
                    .unwrap_or_else(|_| error_response(id, codes::INTERNAL, "request failed"));
                let stop = stop && r.error.is_none();
                (r, stop, Some(kept))
            }
            Err(e) => (
                error_response(0, codes::INVALID_PARAMS, format!("not a request: {e}")),
                false,
                None,
            ),
        };
        // A caller that has gone shows as an end of stream; that counts as an answer that
        // could not be delivered, like a failed write.
        let gone = matches!(
            futures_util::FutureExt::now_or_never(read.fill_buf()),
            Some(Ok(buf)) if buf.is_empty()
        );
        if gone || write_line(&mut write, &response).await.is_err() {
            if let Some(request) = request {
                daemon.delivery_failed(&request, &response).await;
            }
            return;
        }
        if stop {
            // After the answer is written, so the caller sees it.
            daemon.request_stop();
        }
    }
}

async fn write_line(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    r: &Response,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(r).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;
    write.flush().await
}

/// A blocking client of the core API.
pub struct Client {
    paths: Paths,
    reader: BufReader<StdStream>,
    next_id: u64,
    /// Set when the connection can no longer be trusted to answer the next request (a
    /// timeout, a wrong answer, a failed write): the next call connects anew.
    broken: bool,
    /// Replaces the per-method time limit (tests).
    limit: Option<Duration>,
}

fn not_running() -> RpcError {
    RpcError {
        code: codes::NOT_RUNNING.to_string(),
        message: "pegboard is not running; run `pegboard browser start`".to_string(),
        data: None,
    }
}

fn io_error(e: std::io::Error) -> RpcError {
    RpcError {
        code: codes::INTERNAL.to_string(),
        message: format!("talking to the daemon failed: {e}"),
        data: None,
    }
}

impl Client {
    /// `not_running` when no live daemon answers on the socket. A socket file that refuses
    /// connections is removed when the daemon lock is free (the daemon is dead), and left
    /// alone when it is held (a daemon is starting).
    pub fn connect(paths: &Paths) -> Result<Client, RpcError> {
        match StdStream::connect(&paths.socket) {
            Ok(stream) => Ok(Client {
                paths: paths.clone(),
                reader: BufReader::new(stream),
                next_id: 1,
                broken: false,
                limit: None,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                // Stale only if no daemon holds the lock: a daemon that is starting has
                // its socket bound before it listens, and must keep it.
                if let Ok(Some(_free)) = DaemonLock::try_acquire(paths) {
                    let _ = std::fs::remove_file(&paths.socket);
                }
                Err(not_running())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_running()),
            Err(e) => Err(io_error(e)),
        }
    }

    /// Replaces the time limit of every call (for tests).
    #[doc(hidden)]
    pub fn set_call_limit(&mut self, limit: Duration) {
        self.limit = Some(limit);
    }

    pub fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<R, RpcError> {
        if self.broken {
            // The old connection may still deliver an answer to an earlier request.
            let stream = StdStream::connect(&self.paths.socket).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    not_running()
                }
                _ => io_error(e),
            })?;
            self.reader = BufReader::new(stream);
            self.next_id = 1;
            self.broken = false;
        }
        self.exchange(method, params)
    }

    fn exchange<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<R, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request {
            jsonrpc: JSONRPC.to_string(),
            id,
            method: method.to_string(),
            params: serde_json::to_value(params).map_err(|e| RpcError {
                code: codes::INVALID_PARAMS.to_string(),
                message: e.to_string(),
                data: None,
            })?,
        };
        let mut line = serde_json::to_vec(&request).map_err(|e| io_error(e.into()))?;
        line.push(b'\n');
        // A command may take its own time limit; everything else is quick.
        let limit = match method {
            "command.exec" => {
                let ms = request.params["timeoutMs"].as_u64().unwrap_or(60_000);
                Duration::from_millis(ms) + Duration::from_secs(15)
            }
            // A page load and the attach of agent-browser.
            "session.create" => Duration::from_secs(180),
            // Closes every session in turn.
            "browser.stop" => Duration::from_secs(300),
            _ => Duration::from_secs(60),
        };
        let limit = self.limit.unwrap_or(limit);
        // Any failure from here on may leave an answer to this request on the way.
        self.broken = true;
        let stream = self.reader.get_mut();
        stream.set_read_timeout(Some(limit)).map_err(io_error)?;
        stream.write_all(&line).map_err(io_error)?;
        let mut answer = String::new();
        let read = self.reader.read_line(&mut answer);
        if matches!(&read, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
        {
            return Err(RpcError {
                code: codes::TIMEOUT.to_string(),
                message: format!(
                    "the daemon did not answer {method} within {} s; the command may still be running",
                    limit.as_secs()
                ),
                data: None,
            });
        }
        if read.map_err(io_error)? == 0 {
            return Err(RpcError {
                code: codes::INTERNAL.to_string(),
                message: "the daemon closed the connection".to_string(),
                data: None,
            });
        }
        let response: Response = serde_json::from_str(&answer).map_err(|e| io_error(e.into()))?;
        // Id 0 is what the server uses for an error about the connection itself (busy,
        // a line too long, not a request); the server closes after it.
        if response.id != id && !(response.id == 0 && response.error.is_some()) {
            return Err(RpcError {
                code: codes::INTERNAL.to_string(),
                message: "the daemon answered a different request; the connection was dropped"
                    .to_string(),
                data: None,
            });
        }
        // The answer to this very request is read: the connection is clean again (unless
        // the server speaks for the connection itself).
        self.broken = response.id != id;
        if let Some(e) = response.error {
            return Err(e);
        }
        serde_json::from_value(response.result.unwrap_or(serde_json::Value::Null))
            .map_err(|e| io_error(e.into()))
    }
}
