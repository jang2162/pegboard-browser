//! The API server with a stub handler: no browser needed.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pegboard_core::api::{serve_with, Client, Handler, ServeOptions};
use pegboard_core::paths::paths_in;
use pegboard_protocol::{Request, Response, JSONRPC};
use serde_json::{json, Value};

struct Stub;

#[async_trait]
impl Handler for Stub {
    async fn handle(&self, req: Request) -> Response {
        Response {
            jsonrpc: JSONRPC.to_string(),
            id: req.id,
            result: Some(json!({ "pong": true })),
            error: None,
        }
    }
}

#[tokio::test]
async fn accept_survives_many_idle_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let opts = ServeOptions {
        max_connections: 8,
        idle_timeout: Duration::from_millis(500),
    };
    let server = {
        let paths = paths.clone();
        tokio::spawn(async move { serve_with(&paths, Arc::new(Stub), opts).await })
    };
    while !paths.socket.exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // More idle connections than the cap: the extra ones are closed at once.
    let idle: Vec<UnixStream> = (0..20)
        .map(|_| UnixStream::connect(&paths.socket).unwrap())
        .collect();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!server.is_finished(), "serve must keep running");
    // Every idle connection was closed by the server (end of stream, not a timeout).
    for mut c in idle {
        let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 1];
        assert_eq!(c.read(&mut buf).unwrap(), 0);
    }
    let p = paths.clone();
    let answer: Value = tokio::task::spawn_blocking(move || {
        Client::connect(&p).unwrap().call("ping", json!({})).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(answer["pong"], true);
    server.abort();
}
