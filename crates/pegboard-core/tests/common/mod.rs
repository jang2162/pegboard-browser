#![allow(dead_code)]
use std::path::PathBuf;

use pegboard_core::browser::{find_chrome, Browser, BrowserConfig};
use pegboard_core::cdp::{page_targets, CdpConn};
use pegboard_core::paths::{paths_in, Paths};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Env {
    pub paths: Paths,
    pub cfg: BrowserConfig,
    _tmp: TempDir,
}

/// Fresh temporary pegboard home plus the repository's bridge extension.
pub fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let extension_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extension/bridge");
    Env {
        paths: paths_in(tmp.path()),
        cfg: BrowserConfig {
            chrome: find_chrome().unwrap(),
            extension_dir: extension_dir.canonicalize().unwrap(),
        },
        _tmp: tmp,
    }
}

pub struct Fixture {
    pub origin: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body>{body}</body></html>"
    )
}

/// Returns (extra headers, body) for a path, mirroring spikes/lib/fixture.mjs.
fn route(path: &str) -> Option<(&'static str, String)> {
    Some(match path {
        "/counter" => ("", page("counter", r#"<h1>counter</h1><button id="btn">Add</button> <span id="clicks">0</span>
<script>
window.__ticks = 0; window.__frames = 0;
document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1);
});
setInterval(() => { window.__ticks++; document.body.style.background = 'hsl(' + (window.__ticks * 7 % 360) + ',70%,85%)'; }, 100);
(function loop() { window.__frames++; requestAnimationFrame(loop); })();
</script>"#)),
        "/cookie/get" => ("", page("cookie-get", r#"<pre id="out"></pre><script>
document.getElementById('out').textContent = JSON.stringify({ cookie: document.cookie, ls: localStorage.getItem('spike') });
</script>"#)),
        "/cookie/set" => (
            "set-cookie: spike=1; Max-Age=86400; Path=/\r\n",
            page("cookie-set", "<p>set</p><script>localStorage.setItem('spike', '1');</script>"),
        ),
        "/opener" => ("", page("opener", r#"<button id="open" onclick="window.open('/counter')">Open</button>"#)),
        "/form" => ("", page("form", r#"<label>Name <input id="name"></label> <label>Password <input id="pw" type="password"></label>"#)),
        "/form-shadow" => ("", page("form-shadow", r#"<div id="host"></div><script>
const root = document.getElementById('host').attachShadow({ mode: 'open' });
root.innerHTML = '<input id="spw" type="password"> <input id="stext">';
</script>"#)),
        "/form-iframe" => ("", page("form-iframe", r#"<p>outer</p><iframe id="fr" src="/form"></iframe>"#)),
        "/form-submit" => ("", page("form-submit", r#"<form action="/counter" method="get"><input id="name" name="n"> <input id="pw" name="p" type="password"> <button type="submit">Go</button></form><script>
// Submits as soon as the password is long enough, like a form that submits on Enter.
const pw = document.getElementById('pw');
pw.addEventListener('input', () => { if (pw.value.length >= 8) pw.form.submit(); });
</script>"#)),
        "/form-otp" => ("", page("form-otp", r#"<input id="otp" autocomplete="one-time-code" oninput="this.blur()">"#)),
        "/form-blur" => ("", page("form-blur", r#"<input id="pw" type="password" oninput="this.blur()">"#)),
        "/frame" => ("", page("frame", r#"<p>outer</p><iframe id="fr" src="/form"></iframe>"#)),
        "/writer" => ("", page("writer", "<p>writer</p><script>setInterval(() => localStorage.setItem('busy', String(Date.now())), 100);</script>")),
        _ => return None,
    })
}

async fn serve(mut sock: tokio::net::TcpStream) {
    let mut buf = vec![0u8; 8192];
    let Ok(n) = sock.read(&mut buf).await else {
        return;
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|p| p.split('?').next())
        .unwrap_or("/");
    let resp = match route(path) {
        Some((headers, body)) => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ),
        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot found".to_string(),
    };
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

pub async fn fixture() -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(serve(sock));
        }
    });
    Fixture { origin, task }
}

/// Environment, fixture server and a launched browser. The browser is killed when dropped.
pub async fn launched() -> (Env, Fixture, Browser) {
    let e = env();
    let f = fixture().await;
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    (e, f, b)
}

/// Brings some page target of `context` to the front.
pub async fn activate_any_target_in(conn: &CdpConn, context: &str) {
    let t = page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(context))
        .expect("no page target in that context");
    conn.send(
        "Target.activateTarget",
        serde_json::json!({ "targetId": t.target_id }),
        None,
    )
    .await
    .unwrap();
}

/// Premise control for "the human window was the last active one": activates a human tab, then
/// sends a context-less `Target.createTarget` straight to the browser (it lands in the profile of
/// the last active window), closes that tab and returns the context it landed in. Equal to the
/// human context when the premise holds. Measured on Chrome 157 while another application has the
/// operating-system focus: it does not hold, so callers report that instead of faking it.
pub async fn establish_human_active(conn: &CdpConn, human_context: &str) -> String {
    activate_any_target_in(conn, human_context).await;
    let r = conn
        .send(
            "Target.createTarget",
            serde_json::json!({ "url": "about:blank" }),
            None,
        )
        .await
        .unwrap();
    let id = r["targetId"].as_str().unwrap().to_string();
    let context = context_of(conn, &id).await;
    let _ = conn
        .send(
            "Target.closeTarget",
            serde_json::json!({ "targetId": id }),
            None,
        )
        .await;
    context
}

/// The browserContextId of a page target.
pub async fn context_of(conn: &CdpConn, target_id: &str) -> String {
    page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.target_id == target_id)
        .and_then(|t| t.browser_context_id)
        .expect("target not found")
}

/// A browser with a human `/cookie/set` tab, an agent window, a bridge group and a proxy
/// session registered for that group. The browser is killed when this is dropped (also on
/// test failure); `shutdown` closes it gracefully.
/// Fields drop in declaration order: the proxy and the browser go before the temp dir.
pub struct SessionFixture {
    pub proxy: pegboard_core::proxy::server::ProxyServer,
    pub bridge: std::sync::Arc<pegboard_core::bridge::Bridge>,
    pub browser: Browser,
    pub fixture: Fixture,
    pub env: Env,
    pub session: pegboard_protocol::SessionId,
    pub endpoint: pegboard_core::proxy::server::Endpoint,
    pub agent_context: String,
    pub group: pegboard_core::bridge::GroupId,
    pub human_target: String,
    pub first_target: String,
}

impl SessionFixture {
    pub async fn shutdown(self) {
        let SessionFixture { proxy, browser, .. } = self;
        drop(proxy);
        browser.shutdown().await.unwrap();
    }
}

pub async fn session_fixture() -> SessionFixture {
    use pegboard_core::bridge::Bridge;
    use pegboard_core::proxy::server::{ProxyServer, WsUpstream};
    use std::sync::Arc;

    let (env, fixture, browser) = launched().await;
    let human_target = page_targets(&browser.conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(browser.human_context.as_str()))
        .expect("no human page")
        .target_id;
    let human_session = browser.conn.attach(&human_target).await.unwrap();
    browser
        .conn
        .navigate(&human_session, &format!("{}/cookie/set", fixture.origin))
        .await
        .unwrap();
    let agent_context = browser
        .open_profile_window(&env.paths, &env.cfg, "PegboardAgent")
        .await
        .unwrap();
    let bridge = Arc::new(Bridge::attach(&browser.conn, &agent_context).await.unwrap());
    let (group, first_target) = bridge.create_group("proxy", "blue").await.unwrap();
    let upstream = Arc::new(WsUpstream {
        browser_ws_url: browser.ws_url.clone(),
    });
    let proxy = ProxyServer::start(upstream).await.unwrap();
    let session = pegboard_protocol::SessionId::generate();
    let endpoint = proxy.register(
        &session,
        &agent_context,
        group.clone(),
        bridge.clone(),
        vec![first_target.clone()],
    );
    SessionFixture {
        env,
        fixture,
        browser,
        bridge,
        proxy,
        session,
        endpoint,
        agent_context,
        group,
        human_target,
        first_target,
    }
}

/// A CDP client of a session endpoint, sending its token.
pub async fn client(endpoint: &pegboard_core::proxy::server::Endpoint) -> CdpConn {
    let auth = format!("Bearer {}", endpoint.token);
    CdpConn::connect_with_headers(&endpoint.url, &[("Authorization", &auth)])
        .await
        .unwrap()
}
