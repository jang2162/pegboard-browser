// Phase 2b measurement (plan task 1, S2 and S4): what a screen recorder beside pegboard's own CDP use
// costs a command, and whether it disturbs the 2a defences. Not a regression suite: every test is
// `#[ignore]` and prints its table; run with
//   cargo test -p pegboard-core --test probe_2b -- --ignored --test-threads=1 --nocapture
// The recorder is mimicked inside the test: a second browser-level CDP connection that attaches to the
// session's tabs, streams (`Page.startScreencast`), acks every frame at once and keeps at most one
// frame per 500 ms (the rest are dropped), and takes a direct capture right before and right after
// each command (awaited in turn: an upper bound of what the recorder adds).
//
// Environment: PROBE_2B_REPS (default 20) runs per command and setting; PROBE_2B_VARIANTS (default
// `base,light,q40,w800,after-only`) the settings measured.
#![allow(dead_code)]
#[path = "shared/clipboard.rs"]
mod clipboard;
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard::Clipboard;
use common::{eventually, ok, s, Harness};
use pegboard_core::actionlog::{ActionLog, Manifest};
use pegboard_core::cdp::{CdpConn, CdpEvent};
use pegboard_core::daemon::run_command;
use pegboard_protocol::{CommandExecResult, RpcError, SessionInfo};
use serde_json::{json, Value};

// ---- the recorder mimic ----

#[derive(Clone, Copy, Debug)]
struct Setting {
    name: &'static str,
    quality: u32,
    max_width: u32,
    /// Capture before the command too (otherwise only after).
    before: bool,
}

const BASE: Setting = Setting {
    name: "base",
    quality: 60,
    max_width: 1280,
    before: true,
};

fn setting(name: &str) -> Setting {
    match name {
        "base" => BASE,
        "light" => Setting {
            name: "light",
            quality: 40,
            max_width: 800,
            before: false,
        },
        "q40" => Setting {
            name: "q40",
            quality: 40,
            ..BASE
        },
        "w800" => Setting {
            name: "w800",
            max_width: 800,
            ..BASE
        },
        "after-only" => Setting {
            name: "after-only",
            before: false,
            ..BASE
        },
        other => panic!("unknown setting {other}"),
    }
}

#[derive(Default, Debug)]
struct Stats {
    received: u64,
    kept: u64,
    dropped: u64,
    kept_bytes: u64,
    last_kept: HashMap<String, Instant>,
}

struct Mimic {
    conn: CdpConn,
    setting: Setting,
    /// target -> the mimic's session on it
    sessions: Mutex<HashMap<String, String>>,
    streaming: Mutex<bool>,
    stats: Arc<Mutex<Stats>>,
    task: tokio::task::JoinHandle<()>,
}

impl Mimic {
    async fn start(ws_url: &str, setting: Setting) -> Mimic {
        let (conn, mut queue) = CdpConn::connect_queued(ws_url).await.unwrap();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let (c, st) = (conn.clone(), stats.clone());
        let task = tokio::spawn(async move {
            while let Some(CdpEvent {
                method,
                params,
                session_id,
            }) = queue.recv().await
            {
                if method != "Page.screencastFrame" {
                    continue;
                }
                let Some(sid) = session_id else { continue };
                // Ack at once: `send_now` writes the request before it returns, so the reply
                // future is dropped unawaited.
                drop(c.send_now(
                    "Page.screencastFrameAck",
                    json!({ "sessionId": params["sessionId"] }),
                    Some(&sid),
                ));
                let bytes = params["data"]
                    .as_str()
                    .map_or(0, |d| d.len() as u64 * 3 / 4);
                let mut s = st.lock().unwrap();
                s.received += 1;
                let now = Instant::now();
                let keep = s
                    .last_kept
                    .get(&sid)
                    .is_none_or(|t| now.duration_since(*t) >= Duration::from_millis(500));
                if keep {
                    s.kept += 1;
                    s.kept_bytes += bytes;
                    s.last_kept.insert(sid, now);
                } else {
                    s.dropped += 1;
                }
            }
        });
        Mimic {
            conn,
            setting,
            sessions: Mutex::new(HashMap::new()),
            streaming: Mutex::new(false),
            stats,
            task,
        }
    }

    fn screencast_params(&self) -> Value {
        json!({ "format": "jpeg", "quality": self.setting.quality, "maxWidth": self.setting.max_width })
    }

    async fn session_of(&self, target: &str) -> Option<String> {
        if let Some(s) = self.sessions.lock().unwrap().get(target) {
            return Some(s.clone());
        }
        let sid = self.conn.attach(target).await.ok()?;
        let _ = self.conn.send("Page.enable", json!({}), Some(&sid)).await;
        if *self.streaming.lock().unwrap() {
            let _ = self
                .conn
                .send("Page.startScreencast", self.screencast_params(), Some(&sid))
                .await;
        }
        self.sessions
            .lock()
            .unwrap()
            .insert(target.to_string(), sid.clone());
        Some(sid)
    }

    /// Attaches to every target not attached yet.
    async fn follow(&self, targets: &[String]) {
        for t in targets {
            self.session_of(t).await;
        }
    }

    async fn set_streaming(&self, on: bool) {
        *self.streaming.lock().unwrap() = on;
        let sessions: Vec<String> = self.sessions.lock().unwrap().values().cloned().collect();
        for sid in sessions {
            let (method, params) = if on {
                ("Page.startScreencast", self.screencast_params())
            } else {
                ("Page.stopScreencast", json!({}))
            };
            let _ = self.conn.send(method, params, Some(&sid)).await;
        }
    }

    /// One direct capture of `target` (attaching first if needed): its time and whether it worked.
    /// With `max_width` under 1280 the capture is clipped to the viewport and scaled down.
    async fn capture(&self, target: &str) -> (Duration, bool) {
        let t0 = Instant::now();
        let Some(sid) = self.session_of(target).await else {
            return (t0.elapsed(), false);
        };
        let mut params = json!({ "format": "jpeg", "quality": self.setting.quality });
        if self.setting.max_width < 1280 {
            if let Ok(m) = self
                .conn
                .send("Page.getLayoutMetrics", json!({}), Some(&sid))
                .await
            {
                let vv = &m["cssVisualViewport"];
                let css_w = vv["clientWidth"].as_f64().unwrap_or(1.0);
                let dpr = m["contentSize"]["width"].as_f64().unwrap_or(css_w)
                    / m["cssContentSize"]["width"]
                        .as_f64()
                        .unwrap_or(css_w)
                        .max(1.0);
                let scale = (self.setting.max_width as f64 / (css_w * dpr)).min(1.0);
                params["clip"] = json!({ "x": vv["pageX"], "y": vv["pageY"], "width": css_w, "height": vv["clientHeight"], "scale": scale });
            }
        }
        let r = self
            .conn
            .send("Page.captureScreenshot", params, Some(&sid))
            .await;
        let worked = r.is_ok_and(|v| v["data"].as_str().is_some_and(|d| d.len() > 100));
        (t0.elapsed(), worked)
    }

    async fn stop(self) {
        self.set_streaming(false).await;
        self.task.abort();
        self.conn.close();
    }
}

// ---- helpers ----

fn log_for(h: &Harness, info: &SessionInfo) -> ActionLog {
    let manifest = Manifest::new(
        &info.recording_id,
        &info.id.0,
        &info.name,
        &info.profile,
        &info.created_at,
        "test",
        "0.42.0",
    );
    ActionLog::open(&h.env.paths, manifest).unwrap()
}

async fn command(
    h: &Harness,
    log: &mut ActionLog,
    info: &SessionInfo,
    argv: &[&str],
) -> Result<CommandExecResult, RpcError> {
    run_command(
        &h.manager,
        &h.runner,
        &h.env.paths,
        log,
        &info.id,
        &s(argv),
        Some(30_000),
    )
    .await
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

fn max(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::NAN, f64::max)
}

/// The tab the session's agent-browser works on, or else the session's first tab.
fn current_target(h: &Harness, info: &SessionInfo) -> String {
    h.runner
        .bound_target(&info.id)
        .or_else(|| h.proxy.targets(&info.id).into_iter().next())
        .unwrap_or_else(|| info.targets[0].clone())
}

/// Brings agent-browser back to the session's first tab (after a `tab new` and its `tab close`).
async fn back_to_first(h: &Harness, log: &mut ActionLog, info: &SessionInfo) {
    let first = &info.targets[0];
    if h.runner.bound_target(&info.id).as_ref() == Some(first) {
        return;
    }
    if let Ok(r) = command(h, log, info, &["tab", "list"]).await {
        let id = r.output["data"]["tabs"]
            .as_array()
            .and_then(|tabs| tabs.iter().find(|t| t["targetId"] == first.as_str()))
            .and_then(|t| t["tabId"].as_str())
            .map(String::from);
        if let Some(id) = id {
            let _ = command(h, log, info, &["tab", &id]).await;
        }
    }
}

/// One timed run: (with the mimic on) a capture before, the command, a capture after.
/// Returns (total time, capture times).
async fn timed(
    h: &Harness,
    log: &mut ActionLog,
    info: &SessionInfo,
    argv: &[&str],
    mimic: Option<&Mimic>,
) -> (Duration, Vec<f64>, bool) {
    let mut caps = Vec::new();
    let t0 = Instant::now();
    if let Some(m) = mimic {
        if m.setting.before {
            let (d, _) = m.capture(&current_target(h, info)).await;
            caps.push(ms(d));
        }
    }
    let r = command(h, log, info, argv).await;
    if let Some(m) = mimic {
        let (d, _) = m.capture(&current_target(h, info)).await;
        caps.push(ms(d));
    }
    let worked = matches!(&r, Ok(r) if ok(r));
    (t0.elapsed(), caps, worked)
}

// ---- S2 ----

#[test]
#[ignore]
fn probe_command_latency_with_and_without_recording() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep) = h.session("/form").await;
        let mut log = log_for(&h, &info);
        let reps: usize = std::env::var("PROBE_2B_REPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let variants = std::env::var("PROBE_2B_VARIANTS")
            .unwrap_or_else(|_| "base,light,q40,w800,after-only".into());
        let form = h.url("/form");
        let counter = h.url("/counter");
        let commands: Vec<(&str, Vec<&str>)> = vec![
            ("open", vec!["open", &form]),
            ("snapshot -i", vec!["snapshot", "-i"]),
            ("click", vec!["click", "#name"]),
            ("fill", vec!["fill", "#name", "hello"]),
            ("type", vec!["type", "#name", " world"]),
            ("press Enter", vec!["press", "Enter"]),
            ("get url", vec!["get", "url"]),
            ("get text", vec!["get", "text", "label"]),
            ("eval", vec!["eval", "document.title"]),
            ("scroll down 200", vec!["scroll", "down", "200"]),
            ("wait 100", vec!["wait", "100"]),
            ("screenshot", vec!["screenshot"]),
            ("tab new", vec!["tab", "new", &counter]),
            ("tab list", vec!["tab", "list"]),
        ];
        println!("S2 reps {reps}, settings {variants}");
        let mut verdicts = Vec::new();
        for name in variants.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            let set = setting(name);
            let mimic = Mimic::start(&h.browser.ws_url, set).await;
            let mut added: HashMap<&str, Vec<f64>> = HashMap::new();
            let mut off_t: HashMap<&str, Vec<f64>> = HashMap::new();
            let mut on_t: HashMap<&str, Vec<f64>> = HashMap::new();
            let mut cap_t: HashMap<&str, Vec<f64>> = HashMap::new();
            let mut failures: HashMap<&str, usize> = HashMap::new();
            for rep in 0..reps {
                for (cname, argv) in &commands {
                    let order = if rep % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    };
                    let mut pair = [0.0f64; 2];
                    for on in order {
                        if on {
                            mimic.follow(&h.proxy.targets(&info.id)).await;
                            mimic.set_streaming(true).await;
                            // The stream's first frames come right after it starts.
                            tokio::time::sleep(Duration::from_millis(150)).await;
                        }
                        let (d, caps, worked) =
                            timed(&h, &mut log, &info, argv, on.then_some(&mimic)).await;
                        if on {
                            mimic.set_streaming(false).await;
                            cap_t.entry(cname).or_default().extend(caps);
                        }
                        if !worked {
                            *failures.entry(cname).or_default() += 1;
                        }
                        pair[on as usize] = ms(d);
                        if *cname == "tab new" {
                            let _ = command(&h, &mut log, &info, &["tab", "close"]).await;
                            back_to_first(&h, &mut log, &info).await;
                        }
                    }
                    off_t.entry(cname).or_default().push(pair[0]);
                    on_t.entry(cname).or_default().push(pair[1]);
                    added.entry(cname).or_default().push(pair[1] - pair[0]);
                }
            }
            let stats = mimic.stats.lock().unwrap().clone_counts();
            mimic.stop().await;
            println!(
                "\nS2 setting {} (quality {}, maxWidth {}, capture {}): stream frames received {}, kept {}, dropped {}, kept bytes {}",
                set.name,
                set.quality,
                set.max_width,
                if set.before { "before and after" } else { "after only" },
                stats.0,
                stats.1,
                stats.2,
                stats.3
            );
            println!("| command | off median ms | on median ms | added median ms | added max ms | capture median ms | capture max ms | failed runs |");
            println!("|---|---|---|---|---|---|---|---|");
            let mut pass = true;
            for (cname, _) in &commands {
                let a = &added[cname];
                let c = cap_t.get(cname).cloned().unwrap_or_default();
                let (am, ax) = (median(a), max(a));
                pass &= am <= 50.0 && ax <= 1000.0;
                println!(
                    "| {cname} | {:.0} | {:.0} | {am:.0} | {ax:.0} | {:.0} | {:.0} | {} |",
                    median(&off_t[cname]),
                    median(&on_t[cname]),
                    median(&c),
                    max(&c),
                    failures.get(cname).copied().unwrap_or(0)
                );
            }
            let v = format!(
                "S2 {}: {} (every command: added median <= 50 ms and max <= 1000 ms)",
                set.name,
                if pass { "PASS" } else { "FAIL" }
            );
            println!("{v}");
            verdicts.push(v);
            back_to_first(&h, &mut log, &info).await;
        }
        println!();
        for v in verdicts {
            println!("{v}");
        }
        h.finish().await;
    });
}

impl Stats {
    fn clone_counts(&self) -> (u64, u64, u64, u64) {
        (self.received, self.kept, self.dropped, self.kept_bytes)
    }
}

// ---- S4 ----

/// Evaluates on the agent's own (proxied) connection; an exception comes back as `EXC ...`.
async fn agent_eval(c: &CdpConn, sid: &str, expr: &str, gesture: bool) -> String {
    match c
        .send(
            "Runtime.evaluate",
            json!({ "expression": expr, "awaitPromise": true, "returnByValue": true, "userGesture": gesture }),
            Some(sid),
        )
        .await
    {
        Ok(r) if r.get("exceptionDetails").is_some() => format!("EXC {}", r["exceptionDetails"]),
        Ok(r) => match &r["result"]["value"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        Err(e) => format!("CDPERR {e:#}"),
    }
}

#[test]
#[ignore]
fn probe_recording_beside_the_2a_defences() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let (info, ep) = h.session("/form").await;
        let mut log = log_for(&h, &info);
        let mimic = Mimic::start(&h.browser.ws_url, BASE).await;
        mimic.set_streaming(true).await;
        mimic.follow(&h.proxy.targets(&info.id)).await;
        let first = info.targets[0].clone();
        let mut rows: Vec<(String, bool, String)> = Vec::new();
        let files = h.env.paths.session_files(&info.id);

        // Clipboard: a write and a read from the agent's own connection, with a user gesture.
        let agent = common::client(&ep).await;
        let asid = agent.attach(&first).await.unwrap();
        let marker = clip.mark();
        let w = agent_eval(
            &agent,
            &asid,
            "Promise.race([navigator.clipboard.writeText('WRITTEN-'+Date.now()).then(()=>'ok',e=>'ERR '+e.name), new Promise(r=>setTimeout(()=>r('PENDING'),3000))])",
            true,
        )
        .await;
        let x = agent_eval(
            &agent,
            &asid,
            "(()=>{const t=document.createElement('textarea');t.value='COPIED-'+Date.now();document.body.appendChild(t);t.select();return String(document.execCommand('copy'))})()",
            true,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let unchanged = clip.get() == marker;
        rows.push((
            "clipboard write is blocked (writeText, execCommand copy)".into(),
            unchanged,
            format!("writeText {w}, execCommand {x}"),
        ));
        let marker = clip.mark();
        let r = agent_eval(
            &agent,
            &asid,
            "Promise.race([navigator.clipboard.readText().then(t=>t,e=>'ERR '+e.name), new Promise(r=>setTimeout(()=>r('PENDING'),3000))])",
            true,
        )
        .await;
        rows.push((
            "clipboard read is blocked".into(),
            !r.contains(&marker),
            r.chars().take(80).collect(),
        ));
        let _ = agent
            .send(
                "Target.detachFromTarget",
                json!({ "sessionId": asid }),
                None,
            )
            .await;
        agent.close();

        // A request to a private address is refused by the guard.
        let denied = || {
            h.manager
                .guard_denied()
                .get(pegboard_core::address::PRIVATE)
                .copied()
                .unwrap_or(0)
        };
        let before = denied();
        let got = command(
            &h,
            &mut log,
            &info,
            &["eval", "Promise.race([fetch('http://10.255.255.1:81/x', {mode: 'no-cors'}).then(() => 'REACHED', e => 'FAIL:' + e.message), new Promise(r => setTimeout(() => r('TIMEOUT'), 5000))])"],
        )
        .await;
        let said = match &got {
            Ok(r) => r.output["data"].to_string(),
            Err(e) => format!("{}: {}", e.code, e.message),
        };
        rows.push((
            "private address request is blocked".into(),
            said.contains("FAIL:") && denied() > before,
            format!("{said}, guard refusals {before} -> {}", denied()),
        ));

        // A link download lands in the session's files directory.
        let link = common::DownloadProbe::new();
        let r = command(
            &h,
            &mut log,
            &info,
            &["open", &h.url(&format!("/linkto/dl/{}", link.token))],
        )
        .await;
        let opened = matches!(&r, Ok(r) if ok(r));
        let _ = command(&h, &mut log, &info, &["click", "#go"]).await;
        let saved = eventually(15, || async { files.join(&link.token).exists() }).await;
        rows.push((
            "link download lands in the session files directory".into(),
            opened && saved && link.in_real_downloads().is_empty(),
            format!(
                "opened {opened}, saved {saved}, in ~/Downloads {}",
                link.in_real_downloads().len()
            ),
        ));

        // A popup that only downloads (and closes) is attributed to the session.
        let pop = common::DownloadProbe::new();
        let conn = &h.browser.conn;
        let sid = conn.attach(&first).await.unwrap();
        conn.send(
            "Runtime.evaluate",
            json!({ "expression": format!("window.open('/dl/{}'); 1", pop.token), "userGesture": true }),
            Some(&sid),
        )
        .await
        .unwrap();
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": sid }), None)
            .await;
        mimic.follow(&h.proxy.targets(&info.id)).await;
        let saved = eventually(15, || async { files.join(&pop.token).exists() }).await;
        rows.push((
            "download of a popup that closes is attributed to the session".into(),
            saved && pop.in_real_downloads().is_empty(),
            format!(
                "saved {saved}, in ~/Downloads {}",
                pop.in_real_downloads().len()
            ),
        ));

        // dialog accept.
        let dialog = "data:text/html,<button id=\"c\" onclick=\"document.title=String(confirm('ok?'))\">c</button>";
        let _ = command(&h, &mut log, &info, &["open", dialog]).await;
        let _ = command(&h, &mut log, &info, &["click", "#c"]).await;
        let acc = command(&h, &mut log, &info, &["dialog", "accept"]).await;
        let title = command(&h, &mut log, &info, &["get", "title"]).await;
        let title_text = match &title {
            Ok(r) => r.output["data"].to_string(),
            Err(e) => e.message.clone(),
        };
        rows.push((
            "dialog accept works".into(),
            matches!(&acc, Ok(r) if ok(r)) && title_text.contains("true"),
            format!(
                "accept ok {}, title {title_text}",
                matches!(&acc, Ok(r) if ok(r))
            ),
        ));

        // A popup joins the session.
        let _ = command(&h, &mut log, &info, &["open", &h.url("/opener")]).await;
        let n = h.manager.info(&info.id).unwrap().targets.len();
        let _ = command(&h, &mut log, &info, &["click", "#open"]).await;
        let joined = eventually(5, || async {
            h.manager.info(&info.id).unwrap().targets.len() > n
        })
        .await;
        mimic.follow(&h.proxy.targets(&info.id)).await;
        rows.push((
            "popup joins the session".into(),
            joined,
            format!(
                "targets {n} -> {}",
                h.manager.info(&info.id).unwrap().targets.len()
            ),
        ));

        // The mimic really streamed meanwhile (else the rows above prove nothing about recording).
        let (received, kept, _, _) = mimic.stats.lock().unwrap().clone_counts();
        rows.push((
            "the recorder mimic received frames meanwhile".into(),
            received > 0,
            format!("received {received}, kept {kept}"),
        ));
        mimic.stop().await;

        for (what, pass, detail) in &rows {
            println!(
                "S4 {what}: {} ({detail})",
                if *pass { "PASS" } else { "FAIL" }
            );
        }
        h.finish().await;
        drop(clip);
        let failed: Vec<&String> = rows.iter().filter(|r| !r.1).map(|r| &r.0).collect();
        assert!(failed.is_empty(), "failed: {failed:?}");
    });
}
