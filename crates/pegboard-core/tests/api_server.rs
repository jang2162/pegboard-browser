//! The API server with a stub handler: no browser needed.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pegboard_core::api::{serve_with, Client, Handler, ServeOptions};
use pegboard_core::confirm::{
    confirm_method, ConfirmAsk, ConfirmBroker, ConfirmSubscription, CONFIRM_TIMEOUT,
};
use pegboard_core::paths::paths_in;
use pegboard_protocol::{
    codes, ActionClass, ConfirmBy, ConfirmOutcome, Request, Response, SessionId, JSONRPC,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};

/// Answers every request; `session.create` answers slowly with a session id, and the ids of
/// answers that could not be delivered are recorded.
/// With a broker, it answers the confirmation methods as the daemon does.
#[derive(Default)]
struct Stub {
    undelivered: Arc<Mutex<Vec<String>>>,
    broker: Option<Arc<ConfirmBroker>>,
}

#[async_trait]
impl Handler for Stub {
    async fn handle(&self, req: Request) -> Response {
        if let Some(outcome) = self
            .broker
            .as_ref()
            .and_then(|b| confirm_method(b, &req.method, &req.params))
        {
            let (result, error) = match outcome {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            return Response {
                jsonrpc: JSONRPC.to_string(),
                id: req.id,
                result,
                error,
            };
        }
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

    fn confirm_subscription(&self) -> Option<ConfirmSubscription> {
        self.broker.as_ref().map(|b| b.subscribe())
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

fn ask_state(session: &str) -> ConfirmAsk {
    ConfirmAsk {
        session_id: SessionId(session.into()),
        session_name: "demo".into(),
        profile: "default".into(),
        class: ActionClass::State,
        details: Vec::new(),
        offer_allow_for_session: true,
        place: None,
    }
}

/// A raw connection that subscribed: its answer to `confirm.subscribe` is read.
async fn subscribed(paths: &pegboard_core::paths::Paths) -> AsyncBufReader<tokio::net::UnixStream> {
    let stream = tokio::net::UnixStream::connect(&paths.socket)
        .await
        .unwrap();
    let mut conn = AsyncBufReader::new(stream);
    conn.get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"confirm.subscribe\"}\n")
        .await
        .unwrap();
    let answer = next_line(&mut conn).await;
    assert_eq!(answer["id"], 1, "{answer}");
    assert_eq!(answer["result"], json!({}), "{answer}");
    conn
}

async fn next_line(conn: &mut AsyncBufReader<tokio::net::UnixStream>) -> Value {
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(5), conn.read_line(&mut line))
        .await
        .expect("a line within 5 s")
        .unwrap();
    assert!(n > 0, "the connection ended");
    serde_json::from_str(&line).unwrap()
}

async fn rpc(
    paths: &pegboard_core::paths::Paths,
    method: &str,
    params: Value,
) -> Result<Value, pegboard_protocol::RpcError> {
    let p = paths.clone();
    let method = method.to_string();
    tokio::task::spawn_blocking(move || Client::connect(&p).unwrap().call(&method, params))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_streams_required_and_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let broker = ConfirmBroker::new(CONFIRM_TIMEOUT);
    let stub = Stub {
        broker: Some(broker.clone()),
        ..Stub::default()
    };
    let server = started(&paths, stub, ServeOptions::default()).await;
    let mut conn = subscribed(&paths).await;
    // A request on a subscribed connection is not answered: it only carries notices.
    conn.get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"confirm.list\"}\n")
        .await
        .unwrap();
    let asking = {
        let b = broker.clone();
        tokio::spawn(async move { b.ask(ask_state("s_aaaaaaaa")).await })
    };
    let required = next_line(&mut conn).await;
    assert_eq!(required["jsonrpc"], "2.0", "{required}");
    assert_eq!(required["method"], "confirm.required", "{required}");
    assert!(required.get("id").is_none(), "{required}");
    let params = &required["params"];
    let id = params["confirmId"].as_str().unwrap().to_string();
    assert_eq!(params["sessionId"], "s_aaaaaaaa");
    assert_eq!(params["class"], "state");
    assert_eq!(params["offerAllowForSession"], true);
    // A client that comes later sees it in the list.
    let list = rpc(&paths, "confirm.list", json!({})).await.unwrap();
    assert_eq!(list["pending"][0]["confirmId"], id.as_str(), "{list}");
    let answered = rpc(
        &paths,
        "confirm.respond",
        json!({ "confirmId": id, "answer": "approve" }),
    )
    .await
    .unwrap();
    assert_eq!(answered, json!({}));
    let resolved = next_line(&mut conn).await;
    assert_eq!(resolved["method"], "confirm.resolved", "{resolved}");
    assert_eq!(
        resolved["params"],
        json!({ "confirmId": id, "outcome": "approved", "by": "api" })
    );
    let d = asking.await.unwrap();
    assert_eq!(
        (d.outcome, d.by),
        (ConfirmOutcome::Approved, Some(ConfirmBy::Api))
    );
    // The subscriber hangs up: the subscription ends, and nobody is left to ask.
    drop(conn);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let d = broker.ask(ask_state("s_aaaaaaaa")).await;
        if d.outcome == ConfirmOutcome::NoResponder {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "still subscribed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscribed_connection_is_not_idle_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let broker = ConfirmBroker::new(CONFIRM_TIMEOUT);
    let stub = Stub {
        broker: Some(broker.clone()),
        ..Stub::default()
    };
    let opts = ServeOptions {
        idle_timeout: Duration::from_millis(300),
        ..ServeOptions::default()
    };
    let server = started(&paths, stub, opts).await;
    let mut conn = subscribed(&paths).await;
    // Many idle periods pass.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let asking = {
        let b = broker.clone();
        tokio::spawn(async move { b.ask(ask_state("s_aaaaaaaa")).await })
    };
    let required = next_line(&mut conn).await;
    assert_eq!(required["method"], "confirm.required", "{required}");
    broker.cancel_all();
    assert_eq!(asking.await.unwrap().outcome, ConfirmOutcome::Cancelled);
    let resolved = next_line(&mut conn).await;
    assert_eq!(resolved["params"]["outcome"], "cancelled", "{resolved}");
    // A connection that does not subscribe is still closed when idle.
    let mut idle = UnixStream::connect(&paths.socket).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut rest = Vec::new();
    idle.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn respond_unknown_is_confirm_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let broker = ConfirmBroker::new(CONFIRM_TIMEOUT);
    let stub = Stub {
        broker: Some(broker.clone()),
        ..Stub::default()
    };
    let server = started(&paths, stub, ServeOptions::default()).await;
    let e = rpc(
        &paths,
        "confirm.respond",
        json!({ "confirmId": "00112233445566778899aabbccddeeff", "answer": "approve" }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, codes::CONFIRM_NOT_FOUND);
    // An answer carries no session or class, and only the three answers exist.
    for bad in [
        json!({ "confirmId": "x", "answer": "approve", "sessionId": "s_aaaaaaaa" }),
        json!({ "confirmId": "x", "answer": "yes" }),
        json!({ "answer": "approve" }),
    ] {
        let e = rpc(&paths, "confirm.respond", bad.clone())
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS, "{bad}");
    }
    let list = rpc(&paths, "confirm.list", json!({})).await.unwrap();
    assert_eq!(list, json!({ "pending": [] }));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_without_a_broker_is_an_unknown_method() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let server = started(&paths, Stub::default(), ServeOptions::default()).await;
    let e = rpc(&paths, "confirm.subscribe", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::UNKNOWN_METHOD);
    server.abort();
}
