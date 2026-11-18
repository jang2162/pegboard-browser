//! The replay page: a loopback HTTP server, started with the daemon, that shows one recording
//! to the person who ran `pegboard recording open`.
//!
//! Each `grant` makes a new one-time link `http://127.0.0.1:<port>/r/<token>/` for one
//! recording; the token ends after `TOKEN_TTL` or when the daemon stops (`revoke_all`). The
//! server answers only `GET` and `HEAD` with `Host: 127.0.0.1:<port>` exactly (a page of
//! another site reaching the port through a name of its own is refused), serves its built-in
//! page and that recording's data and frames, nothing else, and every unknown, expired or
//! revoked token, deleted recording or other path gets the same `404`. Its port is one of
//! pegboard's own ports, which agent sessions never reach (`address::OWN_PORT`).

pub mod model;

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::paths::Paths;
use crate::recording::screen::{valid_frame_name, valid_target_id};
use model::{is_real_dir, open_regular, real_recording_dir};

/// How long a link works.
pub const TOKEN_TTL: Duration = Duration::from_secs(30 * 60);

/// The page, built into the binary.
const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const APP_CSS: &str = include_str!("assets/app.css");

/// Sent with every answer.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Longest request head read.
const MAX_HEAD: usize = 8 * 1024;
/// Time a client has to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once; more wait.
const MAX_CONNECTIONS: usize = 32;
/// Longest a connection is served (a client that stops reading is cut off).
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest frame file served.
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

struct Grant {
    recording_id: String,
    /// The monotonic clock stops while the machine sleeps (macOS), so the wall clock is
    /// kept too: the grant ends when either has passed.
    expires: Instant,
    expires_wall: SystemTime,
}

impl Grant {
    fn live(&self, now: Instant, wall: SystemTime) -> bool {
        self.expires > now && self.expires_wall > wall
    }
}

struct Inner {
    port: u16,
    paths: Paths,
    ttl: Duration,
    /// Token -> what it opens.
    grants: Mutex<HashMap<String, Grant>>,
}

pub struct ReplayServer {
    inner: Arc<Inner>,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for ReplayServer {
    fn drop(&mut self) {
        self.revoke_all();
        self.accept.abort();
    }
}

/// The token lifetime: `PEGBOARD_TEST_REPLAY_TTL_SECS` (a test hook) or `TOKEN_TTL`.
fn ttl_from(var: Option<&str>) -> Duration {
    match var.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) if secs > 0 => Duration::from_secs(secs),
        _ => TOKEN_TTL,
    }
}

/// 32 random bytes from the OS generator, in lower-case base32 (52 characters).
fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .to_ascii_lowercase()
}

fn valid_token(t: &str) -> bool {
    t.len() == 52 && t.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

impl ReplayServer {
    /// Binds 127.0.0.1:0 once, at daemon start.
    pub async fn start(paths: Paths) -> anyhow::Result<Arc<ReplayServer>> {
        let ttl = ttl_from(crate::testenv::var("PEGBOARD_TEST_REPLAY_TTL_SECS").as_deref());
        ReplayServer::start_with_ttl(paths, ttl).await
    }

    #[doc(hidden)]
    pub async fn start_with_ttl(paths: Paths, ttl: Duration) -> anyhow::Result<Arc<ReplayServer>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let inner = Arc::new(Inner {
            port,
            paths,
            ttl,
            grants: Mutex::new(HashMap::new()),
        });
        let serving = inner.clone();
        let accept = tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            loop {
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    return;
                };
                match listener.accept().await {
                    Ok((sock, _)) => {
                        let inner = serving.clone();
                        tokio::spawn(async move {
                            let _ =
                                tokio::time::timeout(CONNECTION_TIMEOUT, serve(&inner, sock)).await;
                            drop(permit);
                        });
                    }
                    // E.g. out of file descriptors; do not spin.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        });
        Ok(Arc::new(ReplayServer { inner, accept }))
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// A new token for one recording: `http://127.0.0.1:<port>/r/<token>/` and when it
    /// expires. The caller has checked that the recording exists.
    pub fn grant(&self, recording_id: &str) -> (String, OffsetDateTime) {
        let token = new_token();
        let (now, wall) = (Instant::now(), SystemTime::now());
        let mut grants = self.inner.lock();
        grants.retain(|_, g| g.live(now, wall));
        grants.insert(
            token.clone(),
            Grant {
                recording_id: recording_id.to_string(),
                expires: now + self.inner.ttl,
                expires_wall: wall + self.inner.ttl,
            },
        );
        let url = format!("http://127.0.0.1:{}/r/{token}/", self.inner.port);
        (url, OffsetDateTime::now_utc() + self.inner.ttl)
    }

    /// Ends every link.
    pub fn revoke_all(&self) {
        self.inner.lock().clear();
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Grant>> {
        self.grants.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The recording a live token opens. Every token is compared in constant time.
    fn recording_of(&self, token: &str) -> Option<String> {
        let (now, wall) = (Instant::now(), SystemTime::now());
        let mut grants = self.lock();
        grants.retain(|_, g| g.live(now, wall));
        let mut found = None;
        for (t, g) in grants.iter() {
            if bool::from(t.as_bytes().ct_eq(token.as_bytes())) {
                found = Some(g.recording_id.clone());
            }
        }
        found
    }
}

/// What a path after `/r/<token>/` asks for.
#[derive(Debug, PartialEq)]
pub enum Route {
    Page,
    Asset(&'static str),
    Data,
    Frame { target: String, file: String },
    NotFound,
}

/// Pure: the path after `/r/<token>/`. Target ids are 32 upper-case hex characters, files
/// numbered `.jpg` as the writer names them; anything else (`..`, `%2e`, a slash too many, a
/// query) is `NotFound`.
pub fn route(rest: &str) -> Route {
    match rest {
        "" => return Route::Page,
        "app.js" => return Route::Asset("app.js"),
        "app.css" => return Route::Asset("app.css"),
        "data" => return Route::Data,
        _ => {}
    }
    match rest.strip_prefix("frames/") {
        Some(name) if valid_frame_name(name) => {
            let (target, file) = name.split_once('/').unwrap_or_default();
            Route::Frame {
                target: target.to_string(),
                file: file.to_string(),
            }
        }
        _ => Route::NotFound,
    }
}

/// An answer: status, content type and body.
struct Answer {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    extra: &'static str,
}

impl Answer {
    fn ok(content_type: &'static str, body: Vec<u8>) -> Answer {
        Answer {
            status: "200 OK",
            content_type,
            body,
            extra: "",
        }
    }

    fn text(status: &'static str, body: &str) -> Answer {
        Answer {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.as_bytes().to_vec(),
            extra: "",
        }
    }

    fn not_found() -> Answer {
        Answer::text("404 Not Found", "not found")
    }
}

async fn serve(inner: &Inner, mut sock: TcpStream) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut sock)).await {
        Ok(Some(head)) => head,
        _ => return,
    };
    let (answer, head_only) = match parse(&head) {
        None => (Answer::text("400 Bad Request", "bad request"), false),
        Some((method, path, hosts)) => {
            let head_only = method == "HEAD";
            (answer(inner, &method, &path, &hosts).await, head_only)
        }
    };
    let mut out = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Content-Security-Policy: {CSP}\r\nReferrer-Policy: no-referrer\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n{}Connection: close\r\n\r\n",
        answer.status,
        answer.content_type,
        answer.body.len(),
        answer.extra,
    )
    .into_bytes();
    if !head_only {
        out.extend_from_slice(&answer.body);
    }
    let _ = sock.write_all(&out).await;
    let _ = sock.shutdown().await;
}

/// The request head, up to its blank line.
async fn read_head(sock: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Some(buf);
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
    }
}

/// Method, path and every `Host` value of a request head.
fn parse(bytes: &[u8]) -> Option<(String, String, Vec<String>)> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    let hosts = req
        .headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("host"))
        .map(|h| String::from_utf8_lossy(h.value).into_owned())
        .collect();
    Some((req.method?.to_string(), req.path?.to_string(), hosts))
}

async fn answer(inner: &Inner, method: &str, path: &str, hosts: &[String]) -> Answer {
    let own = format!("127.0.0.1:{}", inner.port);
    if hosts.len() != 1 || hosts[0] != own {
        return Answer::text("403 Forbidden", "forbidden");
    }
    if method != "GET" && method != "HEAD" {
        return Answer {
            extra: "Allow: GET, HEAD\r\n",
            ..Answer::text("405 Method Not Allowed", "method not allowed")
        };
    }
    let Some((token, rest)) = path
        .strip_prefix("/r/")
        .and_then(|p| p.split_once('/'))
        .filter(|(t, _)| valid_token(t))
    else {
        return Answer::not_found();
    };
    let Some(recording_id) = inner.recording_of(token) else {
        return Answer::not_found();
    };
    // A deleted recording: its page is gone too.
    if real_recording_dir(&inner.paths, &recording_id).is_err() {
        return Answer::not_found();
    }
    match route(rest) {
        Route::Page => Answer::ok("text/html; charset=utf-8", INDEX_HTML.into()),
        Route::Asset("app.js") => Answer::ok("text/javascript; charset=utf-8", APP_JS.into()),
        Route::Asset("app.css") => Answer::ok("text/css; charset=utf-8", APP_CSS.into()),
        Route::Asset(_) | Route::NotFound => Answer::not_found(),
        Route::Data => {
            let paths = inner.paths.clone();
            let built = tokio::task::spawn_blocking(move || {
                let model = model::build(&paths, &recording_id)?;
                serde_json::to_vec(&model).map_err(std::io::Error::other)
            })
            .await;
            match built {
                Ok(Ok(json)) => Answer::ok("application/json", json),
                _ => Answer::not_found(),
            }
        }
        Route::Frame { target, file } => {
            let paths = inner.paths.clone();
            let read = tokio::task::spawn_blocking(move || {
                read_frame(&paths, &recording_id, &target, &file, MAX_FRAME_BYTES)
            })
            .await;
            match read {
                Ok(Ok(bytes)) => Answer::ok("image/jpeg", bytes),
                _ => Answer::not_found(),
            }
        }
    }
}

/// `screen/<target>/<file>` of the recording: every directory on the way a real one, the
/// file a regular file (never through a link) of at most `limit` bytes.
fn read_frame(
    paths: &Paths,
    recording_id: &str,
    target: &str,
    file: &str,
    limit: u64,
) -> std::io::Result<Vec<u8>> {
    let refused = || std::io::Error::new(std::io::ErrorKind::NotFound, "no such frame");
    if !valid_target_id(target) || !valid_frame_name(&format!("{target}/{file}")) {
        return Err(refused());
    }
    let dir = real_recording_dir(paths, recording_id)?;
    let screen = dir.join("screen");
    let tab = screen.join(target);
    if !is_real_dir(&screen) || !is_real_dir(&tab) {
        return Err(refused());
    }
    let f = open_regular(&tab.join(file))?;
    let mut bytes = Vec::new();
    f.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(refused());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::model::testkit::*;
    use super::*;
    use crate::paths::paths_in;
    use std::io::Write;
    use std::net::TcpStream as StdStream;

    const RA: &str = "r_aaaaaaaa";
    const RB: &str = "r_bbbbbbbb";

    /// A raw request; the whole answer as (status code, head, body).
    fn request(port: u16, raw: &str) -> (u16, String, Vec<u8>) {
        let mut s = StdStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let split = all
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a whole head");
        let head = String::from_utf8_lossy(&all[..split]).into_owned();
        let body = all[split + 4..].to_vec();
        let code = head[9..12].parse().unwrap();
        (code, head, body)
    }

    fn get(port: u16, path: &str) -> (u16, String, Vec<u8>) {
        request(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        )
    }

    /// The path of a link (`/r/<token>/`).
    fn path_of(url: &str) -> String {
        let after = url.strip_prefix("http://127.0.0.1:").unwrap();
        after[after.find('/').unwrap()..].to_string()
    }

    struct Setup {
        _tmp: tempfile::TempDir,
        paths: Paths,
        server: Arc<ReplayServer>,
    }

    async fn setup(ttl: Duration) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        // r_a: tab T1 with frames 1 and 2. r_b: tab T2 only, with its frame 1.
        plant(
            &paths,
            RA,
            &[
                command(1, 1, "open", true, None),
                command(2, 3, "click", false, Some("timeout: x")),
            ],
            &[frame(1, 1), frame(2, 2)],
        );
        let b = plant(
            &paths,
            RB,
            &[command(1, 1, "secretcommandofb", true, None)],
            &[],
        );
        let t2 = b.join("screen").join(T2);
        std::fs::create_dir_all(&t2).unwrap();
        std::fs::write(t2.join("000001.jpg"), "jpeg of b").unwrap();
        let server = ReplayServer::start_with_ttl(paths.clone(), ttl)
            .await
            .unwrap();
        Setup {
            _tmp: tmp,
            paths,
            server,
        }
    }

    #[test]
    fn routes() {
        let t = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(route(""), Route::Page);
        assert_eq!(route("app.js"), Route::Asset("app.js"));
        assert_eq!(route("app.css"), Route::Asset("app.css"));
        assert_eq!(route("data"), Route::Data);
        assert_eq!(
            route(&format!("frames/{t}/000001.jpg")),
            Route::Frame {
                target: t.into(),
                file: "000001.jpg".into()
            }
        );
        for bad in [
            "frames/../x".to_string(),
            "frames/%2e%2e/x".to_string(),
            format!("frames/{t}/1.jpg"),
            format!("frames/{}/000001.jpg", t.to_ascii_lowercase()),
            format!("frames/{t}/000001.jpg/x"),
            format!("frames/{t}/../000001.jpg"),
            format!("frames/{t}/..%2f..%2fmanifest.json"),
            format!("frames//{t}/000001.jpg"),
            format!("frames/{t}/000001.jpg?x=1"),
            "data?x=1".to_string(),
            "data/".to_string(),
            "app.js?v=1".to_string(),
            "index.html".to_string(),
            "/".to_string(),
            "manifest.json".to_string(),
        ] {
            assert_eq!(route(&bad), Route::NotFound, "{bad}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_token_opens_only_its_recording() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let (url, expires) = s.server.grant(RA);
        assert!(
            url.starts_with(&format!("http://127.0.0.1:{port}/r/")),
            "{url}"
        );
        assert!(url.ends_with('/'));
        assert!(expires > OffsetDateTime::now_utc() + Duration::from_secs(29 * 60));
        let base = path_of(&url);
        let (code, _, page) = get(port, &base);
        assert_eq!(code, 200);
        assert!(String::from_utf8_lossy(&page).contains("app.js"));
        assert_eq!(get(port, &format!("{base}app.js")).0, 200);
        assert_eq!(get(port, &format!("{base}app.css")).0, 200);
        let (code, _, data) = get(port, &format!("{base}data"));
        assert_eq!(code, 200);
        let data = String::from_utf8(data).unwrap();
        assert!(data.contains("\"recordingId\":\"r_aaaaaaaa\""), "{data}");
        assert!(!data.contains("secretcommandofb"));
        let (code, _, frame) = get(port, &format!("{base}frames/{T1}/000002.jpg"));
        assert_eq!(code, 200);
        assert_eq!(frame, b"jpeg r_aaaaaaaa 000002.jpg");
        // A frame that exists only in r_b, through r_a's token.
        assert_eq!(get(port, &format!("{base}frames/{T2}/000001.jpg")).0, 404);
        // A frame r_a does not have.
        assert_eq!(get(port, &format!("{base}frames/{T1}/000009.jpg")).0, 404);
        // r_b's own token opens r_b.
        let (url_b, _) = s.server.grant(RB);
        let base_b = path_of(&url_b);
        assert_ne!(base, base_b);
        assert_eq!(get(port, &format!("{base_b}frames/{T2}/000001.jpg")).0, 200);
        // A made-up token, a path without one, and the root.
        let made_up = format!("/r/{}/", "a".repeat(52));
        for p in [made_up.as_str(), "/", "/r/", "/r/data", "/favicon.ico"] {
            let (code, _, body) = get(port, p);
            assert_eq!(
                (code, body.as_slice()),
                (404, b"not found".as_slice()),
                "{p}"
            );
        }
        // The token without its trailing slash.
        assert_eq!(get(port, base.trim_end_matches('/')).0, 404);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deleted_recording_is_not_found() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        assert_eq!(get(port, &base).0, 200);
        std::fs::remove_dir_all(s.paths.recordings.join(RA)).unwrap();
        for p in [
            String::new(),
            "app.js".to_string(),
            "app.css".to_string(),
            "data".to_string(),
            format!("frames/{T1}/000001.jpg"),
        ] {
            let (code, _, body) = get(port, &format!("{base}{p}"));
            assert_eq!(
                (code, body.as_slice()),
                (404, b"not found".as_slice()),
                "{p}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_expired_token_is_refused() {
        let s = setup(Duration::from_secs(1)).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        assert_eq!(get(port, &format!("{base}data")).0, 200);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let (code, _, body) = get(port, &format!("{base}data"));
        assert_eq!((code, body.as_slice()), (404, b"not found".as_slice()));
        assert_eq!(get(port, &base).0, 404);
    }

    #[test]
    fn the_test_variable_sets_the_lifetime() {
        assert_eq!(ttl_from(None), TOKEN_TTL);
        assert_eq!(ttl_from(Some("3")), Duration::from_secs(3));
        assert_eq!(ttl_from(Some("0")), TOKEN_TTL);
        assert_eq!(ttl_from(Some("x")), TOKEN_TTL);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revoke_all_ends_every_token() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let a = path_of(&s.server.grant(RA).0);
        let b = path_of(&s.server.grant(RB).0);
        assert_eq!(get(port, &a).0, 200);
        assert_eq!(get(port, &b).0, 200);
        s.server.revoke_all();
        assert_eq!(get(port, &a).0, 404);
        assert_eq!(get(port, &format!("{b}data")).0, 404);
        // A new grant works again.
        let c = path_of(&s.server.grant(RA).0);
        assert_eq!(get(port, &c).0, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paths_outside_the_recording_are_refused() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        let tab = s.paths.recordings.join(RA).join("screen").join(T1);
        let outside = s._tmp.path().join("outside.jpg");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, tab.join("000003.jpg")).unwrap();
        // A frame directory that is a link to r_b's tab.
        std::os::unix::fs::symlink(
            s.paths.recordings.join(RB).join("screen").join(T2),
            s.paths.recordings.join(RA).join("screen").join(T2),
        )
        .unwrap();
        // A FIFO named like a frame (must not hang the server).
        let fifo = std::ffi::CString::new(tab.join("000004.jpg").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for p in [
            format!("frames/{T1}/000003.jpg"),
            format!("frames/{T2}/000001.jpg"),
            format!("frames/{T1}/000004.jpg"),
            format!("frames/{T1}/..%2f..%2fmanifest.json"),
            format!("frames/{T1}/../../manifest.json"),
            "frames/../manifest.json".to_string(),
            "../manifest.json".to_string(),
            "..%2fmanifest.json".to_string(),
            "manifest.json".to_string(),
            "actions.jsonl".to_string(),
        ] {
            let (code, _, body) = get(port, &format!("{base}{p}"));
            assert_eq!(
                (code, body.as_slice()),
                (404, b"not found".as_slice()),
                "{p}"
            );
        }
        // The whole recording directory replaced by a link.
        let ra = s.paths.recordings.join(RA);
        let moved = s._tmp.path().join("moved");
        std::fs::rename(&ra, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &ra).unwrap();
        assert_eq!(get(port, &format!("{base}data")).0, 404);
        assert_eq!(get(port, &format!("{base}frames/{T1}/000001.jpg")).0, 404);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_foreign_host_header_is_refused() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        for host in [
            format!("Host: evil.test:{port}\r\n"),
            format!("Host: localhost:{port}\r\n"),
            "Host: 127.0.0.1\r\n".to_string(),
            format!("Host: 127.0.0.1:{}\r\n", port.wrapping_add(1)),
            format!("Host: 127.0.0.1:{port}\r\nHost: evil.test:{port}\r\n"),
            String::new(),
        ] {
            let (code, _, body) = request(port, &format!("GET {base}data HTTP/1.1\r\n{host}\r\n"));
            assert_ne!(code, 200, "{host:?}");
            assert!(!String::from_utf8_lossy(&body).contains("recordingId"));
        }
        assert_eq!(get(port, &format!("{base}data")).0, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_get_and_head() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        for method in ["POST", "PUT", "DELETE", "OPTIONS", "PATCH"] {
            let (code, _, body) = request(
                port,
                &format!(
                    "{method} {base}data HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\n\r\n"
                ),
            );
            assert!(code == 405 || code == 404, "{method}: {code}");
            assert!(!String::from_utf8_lossy(&body).contains("recordingId"));
        }
        let (code, head, body) = request(
            port,
            &format!("HEAD {base}data HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        assert_eq!(code, 200);
        assert!(body.is_empty());
        assert!(head.contains("Content-Length: "));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn responses_carry_the_security_headers() {
        let s = setup(TOKEN_TTL).await;
        let port = s.server.port();
        let base = path_of(&s.server.grant(RA).0);
        let cases = [
            (base.clone(), 200, "text/html; charset=utf-8"),
            (
                format!("{base}app.js"),
                200,
                "text/javascript; charset=utf-8",
            ),
            (format!("{base}app.css"), 200, "text/css; charset=utf-8"),
            (format!("{base}data"), 200, "application/json"),
            (format!("{base}frames/{T1}/000001.jpg"), 200, "image/jpeg"),
            (format!("{base}nothing"), 404, "text/plain; charset=utf-8"),
            ("/".to_string(), 404, "text/plain; charset=utf-8"),
        ];
        for (path, want, content_type) in cases {
            let (code, head, _) = get(port, &path);
            assert_eq!(code, want, "{path}");
            for header in [
                format!("Content-Security-Policy: {CSP}"),
                "Referrer-Policy: no-referrer".to_string(),
                "Cache-Control: no-store".to_string(),
                "X-Content-Type-Options: nosniff".to_string(),
                "Cross-Origin-Resource-Policy: same-origin".to_string(),
                format!("Content-Type: {content_type}"),
            ] {
                assert!(head.contains(&header), "{path}: {header} in\n{head}");
            }
        }
        // Also on a refused host, a refused method and a request that is not HTTP.
        for (raw, want) in [
            (format!("GET {base} HTTP/1.1\r\nHost: x.test\r\n\r\n"), 403),
            (
                format!("POST {base} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
                405,
            ),
            ("NOT HTTP AT ALL\r\n\r\n".to_string(), 400),
        ] {
            let (code, head, _) = request(port, &raw);
            assert_eq!(code, want, "{raw:?}");
            for header in [
                format!("Content-Security-Policy: {CSP}"),
                "Referrer-Policy: no-referrer".to_string(),
                "Cache-Control: no-store".to_string(),
                "X-Content-Type-Options: nosniff".to_string(),
                "Cross-Origin-Resource-Policy: same-origin".to_string(),
                "Content-Type: text/plain; charset=utf-8".to_string(),
            ] {
                assert!(head.contains(&header), "{raw:?}: {header} in\n{head}");
            }
        }
    }

    #[test]
    fn a_grant_ends_by_either_clock() {
        let (now, wall) = (Instant::now(), SystemTime::now());
        let grant = |instant: Duration, wall_after: bool| Grant {
            recording_id: RA.into(),
            expires: now + instant,
            expires_wall: if wall_after {
                wall + Duration::from_secs(10)
            } else {
                wall - Duration::from_secs(1)
            },
        };
        assert!(grant(Duration::from_secs(10), true).live(now, wall));
        // The machine slept: the monotonic clock says 10 s are left, the wall clock says no.
        assert!(!grant(Duration::from_secs(10), false).live(now, wall));
        assert!(!grant(Duration::ZERO, true).live(now, wall));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_grants_are_dropped_when_another_is_made() {
        let s = setup(Duration::from_millis(200)).await;
        s.server.grant(RA);
        s.server.grant(RB);
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.server.grant(RA);
        assert_eq!(s.server.inner.lock().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_frame_over_the_limit_is_not_served() {
        let s = setup(TOKEN_TTL).await;
        let bytes = format!("jpeg {RA} 000001.jpg").len() as u64;
        let read = |limit| read_frame(&s.paths, RA, T1, "000001.jpg", limit);
        assert_eq!(read(bytes).unwrap().len() as u64, bytes);
        assert!(read(bytes - 1).is_err());
    }

    #[test]
    fn assets_never_parse_html() {
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "setTimeout(\"",
            "setTimeout('",
            "setInterval(\"",
            "setInterval('",
            "createContextualFragment",
            "DOMParser",
            "srcdoc",
            "javascript:",
        ] {
            assert!(!APP_JS.contains(forbidden), "app.js uses {forbidden}");
            assert!(
                !INDEX_HTML.contains(forbidden),
                "index.html uses {forbidden}"
            );
        }
        // The page judges nothing itself (model.rs `views` does): no copy of the rules.
        for rule in [
            "tsKey",
            "frameAt",
            "ts_key",
            "frame_at",
            "Date.parse(n.ts",
            "navigation",
        ] {
            assert!(!APP_JS.contains(rule), "app.js has {rule}");
        }
        let html = INDEX_HTML.to_ascii_lowercase();
        // Scripts only by src, styles only by link, no handlers, no outside URL.
        assert_eq!(html.matches("<script").count(), 1);
        assert!(html.contains("<script src=\"app.js\"></script>"));
        assert!(!html.contains("<style"));
        assert!(!html.contains(" style="));
        // No event handler attribute (` onload=`, ` onerror =`...).
        let handler = html.match_indices(" on").any(|(i, _)| {
            let rest = html[i + 3..].trim_start_matches(|c: char| c.is_ascii_alphabetic());
            rest.trim_start().starts_with('=')
        });
        assert!(!handler, "an event handler attribute");
        for text in [
            &html,
            &APP_JS.to_ascii_lowercase(),
            &APP_CSS.to_ascii_lowercase(),
        ] {
            assert!(!text.contains("http://") && !text.contains("https://"));
            assert!(!text.contains("//cdn") && !text.contains("@import"));
        }
    }

    #[test]
    fn tokens_are_long_and_random() {
        let a = new_token();
        let b = new_token();
        assert!(valid_token(&a) && valid_token(&b), "{a}");
        assert_ne!(a, b);
    }
}
