//! The API server with a stub handler: no browser needed.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pegboard_core::api::{serve_with, Client, Handler, ServeOptions};
use pegboard_core::paths::paths_in;
use pegboard_protocol::{Request, Response, JSONRPC};
use serde_json::{json, Value};

/// Answers every request; `session.create` answers slowly with a session id, and the ids of
/// answers that could not be delivered are recorded.
#[derive(Default)]
struct Stub {
    undelivered: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Handler for Stub {
    async fn handle(&self, req: Request) -> Response {
        let result = if req.method == "session.create" {
            tokio::time::sleep(Duration::from_millis(300)).await;
            json!({ "id": "s_aaaaaaaa" })
        } else {
            json!({ "pong": true })
        };
        Response {
            jsonrpc: JSONRPC.to_string(),
            id: req.id,
            result: Some(result),
            error: None,
        }
    }

    async fn delivery_failed(&self, req: &Request, resp: &Response) {
        let id = resp
            .result
            .as_ref()
            .and_then(|r| r["id"].as_str())
            .unwrap_or("");
        self.undelivered
            .lock()
            .unwrap()
            .push(format!("{}:{id}", req.method));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_survives_many_idle_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let opts = ServeOptions {
        max_connections: 8,
        idle_timeout: Duration::from_millis(500),
    };
    let server = {
        let paths = paths.clone();
        tokio::spawn(async move { serve_with(&paths, Arc::new(Stub::default()), opts).await })
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
        // A refused connection gets one error line first; then the stream ends.
        let mut rest = Vec::new();
        c.read_to_end(&mut rest).unwrap();
        assert!(rest.iter().filter(|b| **b == b'\n').count() <= 1);
    }
    let p = paths.clone();
    let answer: Value = tokio::task::spawn_blocking(move || {
        Client::connect(&p)
            .unwrap()
            .call("ping", json!({}))
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(answer["pong"], true);
    server.abort();
}

async fn started(
    paths: &pegboard_core::paths::Paths,
    stub: Stub,
    opts: ServeOptions,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let paths2 = paths.clone();
    let server = tokio::spawn(async move { serve_with(&paths2, Arc::new(stub), opts).await });
    while !paths.socket.exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn over_cap_connection_gets_busy_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let opts = ServeOptions {
        max_connections: 2,
        idle_timeout: Duration::from_secs(30),
    };
    let server = started(&paths, Stub::default(), opts).await;
    let _held: Vec<UnixStream> = (0..2)
        .map(|_| UnixStream::connect(&paths.socket).unwrap())
        .collect();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut extra = UnixStream::connect(&paths.socket).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut text = String::new();
    extra.read_to_string(&mut text).unwrap();
    let response: Response = serde_json::from_str(text.trim()).unwrap();
    let error = response.error.expect("an error line");
    assert_eq!(error.code, "internal");
    assert!(
        error.message.contains("pegboard is busy; try again"),
        "{error:?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn undelivered_session_create_is_discarded() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let stub = Stub::default();
    let undelivered = stub.undelivered.clone();
    let server = started(&paths, stub, ServeOptions::default()).await;
    // A client that asks for a session and goes away before the answer.
    let mut gone = UnixStream::connect(&paths.socket).unwrap();
    gone.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session.create\",\"params\":{}}\n")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(gone);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        *undelivered.lock().unwrap(),
        vec!["session.create:s_aaaaaaaa".to_string()]
    );
    // A client that reads its answer is not reported.
    let p = paths.clone();
    tokio::task::spawn_blocking(move || {
        Client::connect(&p)
            .unwrap()
            .call::<_, Value>("session.create", json!({}))
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(undelivered.lock().unwrap().len(), 1);
    server.abort();
}

/// A raw server: answers each request line with `{"n": <method>}` after the delay the
/// method names (`slow` waits 600 ms), and with the id `id_of(request id)`.
fn raw_server(
    paths: &pegboard_core::paths::Paths,
    id_of: fn(u64) -> u64,
) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead, BufReader};
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            std::thread::spawn(move || {
                let mut write = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let method = req["method"].as_str().unwrap().to_string();
                    if method == "slow" {
                        std::thread::sleep(Duration::from_millis(600));
                    }
                    let id = id_of(req["id"].as_u64().unwrap());
                    let answer = json!({"jsonrpc": JSONRPC, "id": id, "result": {"n": method}});
                    if writeln!(write, "{answer}").is_err() {
                        return;
                    }
                }
            });
        }
    })
}

#[test]
fn a_late_response_is_not_taken_for_the_next_call() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    std::fs::create_dir_all(paths.socket.parent().unwrap()).unwrap();
    let _server = raw_server(&paths, |id| id);
    let mut client = Client::connect(&paths).unwrap();
    client.set_call_limit(Duration::from_millis(200));
    let e = client.call::<_, Value>("slow", json!({})).unwrap_err();
    assert_eq!(e.code, pegboard_protocol::codes::TIMEOUT);
    // The late answer to `slow` arrives while the next call waits.
    std::thread::sleep(Duration::from_millis(500));
    client.set_call_limit(Duration::from_secs(5));
    let r: Value = client.call("fast", json!({})).unwrap();
    assert_eq!(r["n"], "fast", "the next call got its own answer");
}

#[test]
fn an_answer_with_another_id_is_an_internal_error_and_drops_the_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    std::fs::create_dir_all(paths.socket.parent().unwrap()).unwrap();
    // The server answers one id behind.
    let _server = raw_server(&paths, |id| id + 100);
    let mut client = Client::connect(&paths).unwrap();
    let e = client.call::<_, Value>("a", json!({})).unwrap_err();
    assert_eq!(e.code, pegboard_protocol::codes::INTERNAL);
    // The next call connects anew (its id starts again) and is judged by the same rule.
    let e = client.call::<_, Value>("b", json!({})).unwrap_err();
    assert_eq!(e.code, pegboard_protocol::codes::INTERNAL);
}
