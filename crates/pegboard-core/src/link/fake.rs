//! A browser on memory channels, for tests of the link and of its users.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::transport::{Incoming, Transport};
use super::BrowserLink;

const WAIT: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(200);

/// A browser on memory channels: answers `Target.attachToBrowserTarget` on the root with
/// `B1`, `B2`, … (after the root `Target.attachedToTarget` Chrome sends first) and hands
/// every other message it receives to the test.
pub(crate) struct FakeBrowser {
    inject: Option<mpsc::UnboundedSender<Incoming>>,
    seen: mpsc::UnboundedReceiver<Value>,
}

impl FakeBrowser {
    pub(crate) fn start() -> (BrowserLink, FakeBrowser) {
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Incoming>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (inject_tx, mut inject_rx) = mpsc::unbounded_channel::<Incoming>();
        let (seen_tx, seen_rx) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(async move {
            let mut sessions = 0;
            loop {
                tokio::select! {
                    msg = out_rx.recv() => {
                        let Some(msg) = msg else { break };
                        let v: Value = serde_json::from_str(&msg).unwrap();
                        let attach = v["method"] == "Target.attachToBrowserTarget"
                            && v.get("sessionId").is_none();
                        if attach {
                            sessions += 1;
                            let s = format!("B{sessions}");
                            let event = json!({"method": "Target.attachedToTarget",
                                "params": {"sessionId": s, "waitingForDebugger": false,
                                    "targetInfo": {"targetId": format!("T{s}"), "type": "browser"}}});
                            let reply = json!({"id": v["id"], "result": {"sessionId": s}});
                            let _ = in_tx.send(Incoming::Text(event.to_string()));
                            let _ = in_tx.send(Incoming::Text(reply.to_string()));
                        } else if seen_tx.send(v).is_err() {
                            break;
                        }
                    }
                    msg = inject_rx.recv() => {
                        let Some(msg) = msg else { break };
                        let _ = in_tx.send(msg);
                    }
                }
            }
        });
        let link = BrowserLink::start(Transport {
            incoming: in_rx,
            outgoing: out_tx,
        });
        let fake = FakeBrowser {
            inject: Some(inject_tx),
            seen: seen_rx,
        };
        (link, fake)
    }

    pub(crate) fn send(&self, v: Value) {
        self.send_incoming(Incoming::Text(v.to_string()));
    }

    /// Sends a message already written out (cheaper than `send` for large ones).
    pub(crate) fn send_text(&self, text: String) {
        self.send_incoming(Incoming::Text(text));
    }

    /// Sends `v` as the transport hands it on under a limit of `max` bytes.
    pub(crate) fn send_limited(&self, v: Value, max: usize) {
        self.send_incoming(Incoming::limited(v.to_string(), max));
    }

    pub(crate) fn send_incoming(&self, m: Incoming) {
        self.inject.as_ref().unwrap().send(m).unwrap();
    }

    pub(crate) async fn next(&mut self) -> Value {
        tokio::time::timeout(WAIT, self.seen.recv())
            .await
            .expect("the browser got nothing within 3s")
            .expect("the link closed")
    }

    pub(crate) async fn quiet(&mut self) {
        if let Ok(Some(v)) = tokio::time::timeout(QUIET, self.seen.recv()).await {
            panic!("the browser got {v}");
        }
    }

    /// Answers a request it saw, on the session it came on.
    pub(crate) fn answer(&self, request: &Value, result: Value) {
        let mut reply = json!({"id": request["id"], "result": result});
        if let Some(s) = request.get("sessionId") {
            reply["sessionId"] = s.clone();
        }
        self.send(reply);
    }

    /// The browser goes away: the physical connection ends.
    pub(crate) fn disconnect(&mut self) {
        self.inject = None;
    }
}
