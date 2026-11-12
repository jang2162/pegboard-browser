//! The core API: one JSON-RPC message per line over a unix socket in the pegboard home.
//!
//! The socket is mode 0600 inside the 0700 home, and a peer with another uid is dropped
//! (`peer_cred`, which is `getpeereid` on macOS).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdStream;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use pegboard_protocol::{codes, Request, Response, RpcError, JSONRPC};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

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

/// Serves the API until the future is dropped. Fails when a live daemon already owns the
/// socket; a socket file nobody listens on is replaced.
pub async fn serve(paths: &Paths, daemon: Arc<Daemon>) -> anyhow::Result<()> {
    if StdStream::connect(&paths.socket).is_ok() {
        bail!("already running (a daemon answers on {})", paths.socket.display());
    }
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
    loop {
        let (stream, _) = listener.accept().await?;
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {}
            _ => continue,
        }
        tokio::spawn(connection(stream, daemon.clone()));
    }
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

async fn connection(stream: UnixStream, daemon: Arc<Daemon>) {
    let (read, mut write) = stream.into_split();
    let mut read = tokio::io::BufReader::new(read);
    loop {
        let mut line = Vec::new();
        // Reads at most MAX_LINE + 1 bytes: one more means the line is too long.
        match (&mut read).take(MAX_LINE + 1).read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() as u64 > MAX_LINE {
            let r = error_response(0, codes::INVALID_PARAMS, "request line too long");
            let _ = write_line(&mut write, &r).await;
            return;
        }
        let (response, stop) = match serde_json::from_slice::<Request>(&line) {
            Ok(req) => {
                let (id, stop) = (req.id, req.method == "browser.stop");
                let d = daemon.clone();
                // A task of its own: a panic becomes an error, and the work finishes even
                // if this connection goes away.
                let r = tokio::spawn(async move { d.handle(req).await })
                    .await
                    .unwrap_or_else(|_| error_response(id, codes::INTERNAL, "request failed"));
                let stop = stop && r.error.is_none();
                (r, stop)
            }
            Err(e) => (
                error_response(0, codes::INVALID_PARAMS, format!("not a request: {e}")),
                false,
            ),
        };
        if write_line(&mut write, &response).await.is_err() {
            return;
        }
        if stop {
            // After the answer is written, so the caller sees it.
            daemon.request_stop();
        }
    }
}

async fn write_line(write: &mut tokio::net::unix::OwnedWriteHalf, r: &Response) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(r).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;
    write.flush().await
}

/// A blocking client of the core API.
pub struct Client {
    reader: BufReader<StdStream>,
    next_id: u64,
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
    /// connections is left over from a dead daemon and is removed.
    pub fn connect(paths: &Paths) -> Result<Client, RpcError> {
        match StdStream::connect(&paths.socket) {
            Ok(stream) => Ok(Client {
                reader: BufReader::new(stream),
                next_id: 1,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                let _ = std::fs::remove_file(&paths.socket);
                Err(not_running())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_running()),
            Err(e) => Err(io_error(e)),
        }
    }

    pub fn call<P: Serialize, R: DeserializeOwned>(
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
        self.reader.get_mut().write_all(&line).map_err(io_error)?;
        let mut answer = String::new();
        if self.reader.read_line(&mut answer).map_err(io_error)? == 0 {
            return Err(RpcError {
                code: codes::INTERNAL.to_string(),
                message: "the daemon closed the connection".to_string(),
                data: None,
            });
        }
        let response: Response = serde_json::from_str(&answer).map_err(|e| io_error(e.into()))?;
        if let Some(e) = response.error {
            return Err(e);
        }
        serde_json::from_value(response.result.unwrap_or(serde_json::Value::Null))
            .map_err(|e| io_error(e.into()))
    }
}
