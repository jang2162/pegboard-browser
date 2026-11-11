#![allow(dead_code)]
use std::path::PathBuf;

use pegboard_core::browser::{find_chrome, BrowserConfig};
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
        "/writer" => ("", page("writer", "<p>writer</p><script>setInterval(() => localStorage.setItem('busy', String(Date.now())), 100);</script>")),
        _ => return None,
    })
}

async fn serve(mut sock: tokio::net::TcpStream) {
    let mut buf = vec![0u8; 8192];
    let Ok(n) = sock.read(&mut buf).await else { return };
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
