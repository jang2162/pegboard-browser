//! Client for the bridge extension's service worker (tab groups, tabs in the agent profile).

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::cdp::CdpConn;

const WORKER_WAIT: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupId(pub i64);

pub struct Bridge {
    conn: CdpConn,
    agent_context: String,
    /// Flat-mode session attached to the worker; kept open for the bridge's lifetime.
    session: Mutex<String>,
}

/// Finds the bridge worker of `context`, waiting up to `wait` for it to start.
async fn find_worker(conn: &CdpConn, context: &str, wait: Duration) -> Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let r = conn.send("Target.getTargets", json!({}), None).await?;
        let found = r["targetInfos"].as_array().into_iter().flatten().find(|t| {
            t["type"] == "service_worker"
                && t["browserContextId"].as_str() == Some(context)
                && t["url"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("chrome-extension://") && u.ends_with("/sw.js"))
        });
        if let Some(t) = found {
            return Ok(t["targetId"].as_str().map(String::from));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

impl Bridge {
    /// Attaches to the bridge service worker that lives in `agent_context` and keeps that CDP session open.
    pub async fn attach(conn: &CdpConn, agent_context: &str) -> Result<Bridge> {
        let deadline = tokio::time::Instant::now() + WORKER_WAIT;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let target = find_worker(conn, agent_context, left)
                .await?
                .ok_or_else(|| {
                    anyhow!("bridge service worker for the agent profile did not appear within 10s")
                })?;
            // A worker that is still starting (or being restarted) can accept a session
            // yet never answer, so require one answered ping before trusting it.
            if let Ok(session) = conn.attach(&target).await {
                let ping = conn.evaluate(&session, "typeof self.peg");
                if let Ok(Ok(v)) = tokio::time::timeout(PING_TIMEOUT, ping).await {
                    if v == "object" {
                        return Ok(Bridge {
                            conn: conn.clone(),
                            agent_context: agent_context.to_string(),
                            session: Mutex::new(session),
                        });
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("bridge service worker did not answer within 10s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Calls `self.peg.<func>(...args)` in the worker; retries once after re-attaching when the session is gone.
    async fn call(&self, func: &str, args: &[Value]) -> Result<Value> {
        // Arguments are serialized as JSON, never spliced in as raw text.
        let args = args
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let expr = format!("(async () => JSON.stringify(await self.peg.{func}({args})))()");
        let mut session = self.session.lock().await;
        let first = self.conn.evaluate(&session, &expr).await;
        let out = match first {
            Err(e) if is_session_gone(&e) => {
                let target = find_worker(&self.conn, &self.agent_context, Duration::ZERO)
                    .await?
                    .ok_or_else(|| {
                        anyhow!("bridge_asleep: the bridge service worker is not running")
                    })?;
                *session = self.conn.attach(&target).await?;
                self.conn.evaluate(&session, &expr).await?
            }
            other => other?,
        };
        let text = out
            .as_str()
            .ok_or_else(|| anyhow!("bridge call {func} returned no value"))?;
        serde_json::from_str(text).with_context(|| format!("parsing bridge reply of {func}"))
    }

    pub async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)> {
        let r = self
            .call("createGroup", &[json!(title), json!(color)])
            .await?;
        Ok((GroupId(group_id(&r)?), target_id(&r)?))
    }

    /// Makes a tab in `group`. When Chrome already deleted the group (it goes with its last
    /// tab) the worker starts a new one with `title` and `color` and the group id is returned
    /// with the target.
    pub async fn create_tab(
        &self,
        group: &GroupId,
        url: &str,
        title: &str,
        color: &str,
    ) -> Result<(String, Option<GroupId>)> {
        let r = self
            .call(
                "createTab",
                &[json!(group.0), json!(url), json!(title), json!(color)],
            )
            .await?;
        let moved = r["newGroupId"].as_i64().map(GroupId);
        Ok((target_id(&r)?, moved))
    }

    pub async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()> {
        self.call("addToGroup", &[json!(group.0), json!(target_id)])
            .await?;
        Ok(())
    }

    pub async fn members(&self, group: &GroupId) -> Result<Vec<String>> {
        let r = self.call("members", &[json!(group.0)]).await?;
        Ok(r["targetIds"]
            .as_array()
            .ok_or_else(|| anyhow!("members reply has no targetIds"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    pub async fn close_targets(&self, target_ids: &[String]) -> Result<()> {
        self.call("closeTargets", &[json!(target_ids)]).await?;
        Ok(())
    }

    pub async fn ungroup(&self, group: &GroupId) -> Result<()> {
        self.call("ungroup", &[json!(group.0)]).await?;
        Ok(())
    }
}

fn is_session_gone(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("Session with given id not found") || s.contains("No session with given id")
}

fn group_id(r: &Value) -> Result<i64> {
    r["groupId"]
        .as_i64()
        .ok_or_else(|| anyhow!("reply has no groupId"))
}

fn target_id(r: &Value) -> Result<String> {
    match r["targetId"].as_str() {
        Some(s) => Ok(s.to_string()),
        None => bail!("reply has no targetId"),
    }
}
