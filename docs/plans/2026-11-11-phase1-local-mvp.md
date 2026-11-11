# 1단계 로컬 MVP 실행 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 순정 Chrome for Testing 위에서, 에이전트가 `pegboard` CLI로 세션을 만들고 에이전트 프로필 창의 자기 탭그룹 안에서만 브라우저를 제어하며, 그 동작이 로그로 남는다.

**Architecture:** Rust 단일 바이너리 `pegboard`가 데몬(`pegboard daemon`)과 CLI를 겸한다. 데몬은 브라우저를 띄우는 부모이고, 세션마다 범위를 좁힌 CDP 엔드포인트(프록시)를 열어 무수정 agent-browser를 거기에 붙인다. 탭그룹과 에이전트 프로필의 탭 생성은 작은 확장 프로그램(브리지)이 맡는다. 이 브리지만 3단계에서 버린다.

**Tech Stack:** Rust(stable), tokio, serde, clap, tokio-tungstenite, agent-browser 0.42.0(동봉, 무수정), Chrome for Testing, MV3 확장 프로그램.

**Spec:** [docs/architecture.md](../architecture.md)(결정 1~6, 6절, 7절, 8절, 11절), [docs/roadmap.md](../roadmap.md) 1단계, [docs/spike-findings.md](../spike-findings.md). 실행자는 세 문서를 함께 읽는다.

## Global Constraints

- 대상은 macOS arm64 한 가지다.
- agent-browser는 `0.42.0`으로 고정하고 고치지 않는다. 공개 CLI(`--json`, 전역 플래그)만 쓴다. 데몬 소켓 프로토콜에 직접 붙지 않는다.
- 브라우저는 창이 뜨는 모드로만 띄운다.
- 사용자가 평소 쓰는 Chrome 프로필을 건드리지 않는다. 브라우저의 사용자 데이터 디렉터리는 `<PEGBOARD_HOME>/chrome`이다.
- `PEGBOARD_HOME`의 기본값은 `~/Library/Application Support/Pegboard`. 시험은 반드시 임시 디렉터리로 덮어쓴다.
- 코어 API 소켓과 `PEGBOARD_HOME`은 소유자 전용 권한(디렉터리 `0700`, 소켓 `0600`).
- 세션 CDP 엔드포인트는 `127.0.0.1`에만 열고 세션별 무작위 토큰(32바이트)을 요구한다.
- CDP 메서드는 허용목록 방식이다. 목록에 없는 메서드는 거부한다.
- 사람 프로필의 타깃은 어떤 경로로도 세션에 보이지 않는다. 판정 기준은 타깃의 `browserContextId`다.
- `Target.createTarget`과 컨텍스트를 지정하지 않는 브라우저 수준 메서드는 브라우저에 그대로 넘기지 않는다.
- 정책과 명령 필터 판정은 `pegboard`가 한다. agent-browser의 `--action-policy`에 기대지 않는다.
- 이 저장소의 코드는 MIT. 동봉하는 agent-browser는 Apache-2.0이고 그 라이선스 파일을 함께 둔다.
- 소스 코드의 주석과 식별자는 영어, 사용자에게 보이는 CLI 메시지는 영어, 문서는 한국어.
- 실제 브라우저를 띄우는 시험은 `#[ignore]`를 붙이고 `cargo test -- --ignored --test-threads=1`로 돌린다.
- 커밋은 사용자가 요청할 때만 한다. 실행을 시작할 때 "작업마다 커밋"에 대한 승인을 한 번 받는다. 승인이 없으면 커밋 단계를 건너뛰고 원장에 기록한다.

### 설계와 다르게 가는 것 한 가지

architecture.md는 "TCP 디버깅 포트를 열지 않는다"고 정했다. **1~2단계에서는 `--remote-debugging-port=0`(루프백, 임의 포트)을 쓴다.** 0단계에서 검증한 구성이 이것이고, 파이프는 연결이 하나뿐이라 세션들을 한 연결에 겹쳐 싣는 다중화가 필요한데 그것은 검증하지 않았다. 프록시의 브라우저 쪽 연결을 `Upstream` 트레이트 뒤에 두어, 3단계에서 파이프 다중화 구현으로 바꿔 끼운다.

이 기간의 한계: 같은 사용자로 도는 로컬 프로세스는 `DevToolsActivePort` 파일을 읽어 브라우저 전체에 붙을 수 있다. 1~2단계의 격리가 막는 것은 "제공된 도구(CLI, 세션 엔드포인트)를 통해 조종당한 에이전트"이지, 셸을 가진 악의적 로컬 프로세스가 아니다. 이 브라우저 데이터 디렉터리에는 실제 계정을 넣지 않는 것을 README에 적는다.

## Review Focus

설계가 요구하지만 개별 작업의 기본 시험이 놓치기 쉬운 것들이다. 각 항목의 시험을 해당 작업에 넣었다.

1. **사람 창이 마지막으로 활성인 상태에서 탭을 만든다.** 탭은 에이전트 프로필에 생겨야 한다. CDP의 "기본 컨텍스트"는 실행마다 달랐다. (Task 5, Task 10)
2. **사람이 세션의 탭이나 에이전트 창을 손으로 닫는다.** 다음 명령은 조용히 다른 탭에 붙지 않고 `tab_gone` 오류를 내야 하고, 세션은 새 탭을 만들어 계속 쓸 수 있어야 한다. (Task 6)
3. **데몬이 죽고 소켓 파일만 남는다.** CLI는 멈춰 있지 않고 낡은 소켓임을 알아채 정리해야 한다. (Task 9)
4. **agent-browser 바이너리가 없거나 버전이 다르다.** 첫 명령에서 무엇이 잘못됐는지와 고치는 방법을 말해야 한다. (Task 7)
5. **세션 이름이 비었거나, 매우 길거나, 겹치거나, 한글·이모지다.** 탭그룹 제목으로 그대로 쓰이되 식별에는 쓰이지 않아야 한다. (Task 6)

---

## 파일 구조

```
Cargo.toml                         workspace
LICENSE                            MIT
THIRD_PARTY.md                     agent-browser(Apache-2.0) 고지
README.md
scripts/fetch-agent-browser.sh     고정 버전 받기, sha256 대조
vendor/agent-browser.lock          버전과 sha256
vendor/agent-browser/              받은 바이너리와 그 LICENSE (깃 제외)
extension/bridge/manifest.json     브리지 확장 프로그램 (3단계에서 버린다)
extension/bridge/sw.js
crates/pegboard-protocol/src/lib.rs    코어 API 타입
crates/pegboard-core/src/
  lib.rs
  paths.rs          PEGBOARD_HOME 아래 경로
  cdp.rs            CDP 클라이언트 (WebSocket)
  browser.rs        브라우저 실행, 프로필 창 열기
  bridge.rs         확장 프로그램 호출
  profiles.rs       에이전트 프로필 등록부
  proxy/filter.rs   통과 판정 (순수 함수)
  proxy/server.rs   세션 CDP 엔드포인트
  session.rs        세션 관리자
  argv.rs           agent-browser 명령 필터
  runner.rs         agent-browser 실행
  actionlog.rs      동작 로그와 가림
  api.rs            코어 API 서버 (유닉스 소켓)
  daemon.rs         조립
crates/pegboard-core/tests/e2e_*.rs    실제 브라우저 시험 (#[ignore])
crates/pegboard-cli/src/main.rs        `pegboard` 바이너리
docs/phase1-manual-checks.md           사람이 해야 하는 확인 목록
```

---

### Task 1: 작업공간, 프로토콜 타입, 동봉 준비

**Files:**
- Create: `Cargo.toml`, `LICENSE`, `THIRD_PARTY.md`, `.gitignore`, `scripts/fetch-agent-browser.sh`, `vendor/agent-browser.lock`
- Create: `crates/pegboard-protocol/Cargo.toml`, `crates/pegboard-protocol/src/lib.rs`
- Create: `crates/pegboard-core/Cargo.toml`, `crates/pegboard-core/src/lib.rs`, `crates/pegboard-core/src/paths.rs`

**Interfaces:**
- Produces (`pegboard_protocol`):

```rust
pub const JSONRPC: &str = "2.0";
#[derive(Serialize, Deserialize)] pub struct Request  { pub jsonrpc: String, pub id: u64, pub method: String, #[serde(default)] pub params: serde_json::Value }
#[derive(Serialize, Deserialize)] pub struct Response { pub jsonrpc: String, pub id: u64, pub result: Option<serde_json::Value>, pub error: Option<RpcError> }
#[derive(Serialize, Deserialize)] pub struct RpcError { pub code: String, pub message: String, pub data: Option<serde_json::Value> }
// code values: "not_running", "invalid_params", "unknown_method", "session_not_found",
// "profile_not_found", "profile_in_use", "argv_rejected", "tab_gone", "agent_browser_missing",
// "agent_browser_version", "browser_failed", "internal"

pub struct SessionId(pub String);                 // "s_" + 8 lowercase base32 chars
impl SessionId { pub fn generate() -> Self; pub fn parse(s: &str) -> Option<Self>; }
#[serde(rename_all = "camelCase")]
pub struct SessionInfo { pub id: SessionId, pub name: String, pub profile: String, pub tab_group_id: i64,
    pub targets: Vec<String>, pub state: SessionState, pub recording_id: String,
    pub created_at: String, pub last_activity_at: String }   // RFC 3339 UTC
pub enum SessionState { Active, Closed }          // serialized lowercase
pub struct SessionCreateParams { pub name: Option<String>, pub url: Option<String>, pub profile: Option<String> }
pub struct SessionCloseParams  { pub session_id: SessionId, pub keep_tabs: bool }
pub struct CommandExecParams   { pub session_id: SessionId, pub argv: Vec<String>, pub timeout_ms: Option<u64> }
pub struct CommandExecResult   { pub exit_code: i32, pub output: serde_json::Value, pub stderr: String }
pub struct ProfileInfo { pub name: String, pub kind: ProfileKind, pub sessions: Vec<SessionId> }
pub enum ProfileKind { Default, Named }           // serialized lowercase
pub struct BrowserStatus { pub running: bool, pub pid: Option<u32>, pub chrome_version: Option<String>, pub sessions: usize }
```

- Produces (`pegboard_core::paths`):

```rust
pub struct Paths { pub home: PathBuf, pub chrome_udd: PathBuf, pub recordings: PathBuf,
                   pub socket: PathBuf, pub state: PathBuf, pub ab_socket_dir: PathBuf }
pub fn paths() -> Paths;   // home = $PEGBOARD_HOME or ~/Library/Application Support/Pegboard
                           // ab_socket_dir = /tmp/pegboard-<uid>/ab (unix socket path limit)
pub fn ensure(paths: &Paths) -> std::io::Result<()>;  // create dirs with mode 0700
```

- [ ] **Step 1: 작업공간과 크레이트 뼈대 작성**

`Cargo.toml`은 `members = ["crates/*"]`, `resolver = "2"`. 작업공간 의존성: `serde = { features = ["derive"] }`, `serde_json`, `tokio = { features = ["full"] }`, `thiserror`, `anyhow`, `rand`, `time = { features = ["formatting", "serde"] }`. `.gitignore`에 `target/`, `vendor/agent-browser/`.

- [ ] **Step 2: 실패하는 시험 작성** (`crates/pegboard-protocol/src/lib.rs`의 `#[cfg(test)]`)

```rust
#[test] fn session_id_has_prefix_and_length() {
    let id = SessionId::generate();
    assert!(id.0.starts_with("s_")); assert_eq!(id.0.len(), 10);
    assert!(SessionId::parse(&id.0).is_some());
    assert!(SessionId::parse("../x").is_none()); assert!(SessionId::parse("s_UPPER123").is_none());
}
#[test] fn session_info_uses_camel_case() {
    let json = serde_json::to_value(sample_session_info()).unwrap();
    assert!(json.get("tabGroupId").is_some()); assert_eq!(json["state"], "active");
}
#[test] fn request_params_default_to_null() {
    let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"browser.status"}"#).unwrap();
    assert!(r.params.is_null());
}
```

`crates/pegboard-core/src/paths.rs`:

```rust
#[test] fn home_follows_env_and_dirs_are_private() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PEGBOARD_HOME", tmp.path());
    let p = paths(); ensure(&p).unwrap();
    assert_eq!(p.socket, tmp.path().join("pegboardd.sock"));
    let mode = std::fs::metadata(&p.home).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert!(p.ab_socket_dir.to_str().unwrap().len() < 40);
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p pegboard-protocol -p pegboard-core`
Expected: 컴파일 오류(타입 미정의).

- [ ] **Step 4: 위 Interfaces대로 구현**

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p pegboard-protocol -p pegboard-core`
Expected: 4개 통과.

- [ ] **Step 6: 라이선스와 동봉 스크립트**

`LICENSE`는 MIT 전문(저작권자: 저장소 소유자, 2026). `scripts/fetch-agent-browser.sh`는 npm 레지스트리에서 `agent-browser@0.42.0` 꾸러미를 받아 `bin/agent-browser-darwin-arm64`와 `LICENSE`를 `vendor/agent-browser/`에 풀고, `vendor/agent-browser.lock`의 sha256과 대조한다. 잠금 파일이 비어 있으면 받은 파일의 sha256을 적고 그 사실을 출력한다(첫 실행에 한함). `THIRD_PARTY.md`에 agent-browser의 이름, 버전, 라이선스, 출처를 적는다.

Run: `scripts/fetch-agent-browser.sh && vendor/agent-browser/agent-browser --version && scripts/fetch-agent-browser.sh`
Expected: `agent-browser 0.42.0`. 두 번째 실행은 `checksum ok`.

- [ ] **Step 7: 커밋**

```bash
git add Cargo.toml Cargo.lock LICENSE THIRD_PARTY.md .gitignore scripts vendor/agent-browser.lock crates
git commit -m "feat: workspace, core API types, pinned agent-browser fetch"
```

---

### Task 2: CDP 클라이언트와 브라우저 실행

**Files:**
- Create: `crates/pegboard-core/src/cdp.rs`, `crates/pegboard-core/src/browser.rs`
- Test: `crates/pegboard-core/tests/e2e_browser.rs`, `crates/pegboard-core/tests/common/mod.rs`

**Interfaces:**
- Consumes: `paths::Paths`
- Produces:

```rust
// cdp.rs
pub struct CdpConn;                                   // cheap to clone (Arc inside)
pub struct CdpEvent { pub method: String, pub params: serde_json::Value, pub session_id: Option<String> }
impl CdpConn {
    pub async fn connect(ws_url: &str) -> anyhow::Result<CdpConn>;
    pub async fn send(&self, method: &str, params: serde_json::Value, session_id: Option<&str>) -> anyhow::Result<serde_json::Value>;
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<CdpEvent>;
    pub async fn attach(&self, target_id: &str) -> anyhow::Result<String>;      // flatten: true
    pub async fn evaluate(&self, session_id: &str, expression: &str) -> anyhow::Result<serde_json::Value>; // awaitPromise, returnByValue
    pub async fn navigate(&self, session_id: &str, url: &str) -> anyhow::Result<()>; // waits Page.loadEventFired, 15 s
}
pub struct TargetInfo { pub target_id: String, pub kind: String, pub url: String, pub browser_context_id: Option<String>, pub opener_id: Option<String> }
pub async fn page_targets(conn: &CdpConn) -> anyhow::Result<Vec<TargetInfo>>;

// browser.rs
pub const HUMAN_PROFILE_DIR: &str = "Default";
pub struct BrowserConfig { pub chrome: PathBuf, pub extension_dir: PathBuf }
pub fn find_chrome() -> anyhow::Result<PathBuf>;      // $PEGBOARD_CHROME, else newest under ~/.agent-browser/browsers
pub struct Browser { pub pid: u32, pub ws_url: String, pub conn: CdpConn, pub human_context: String }
impl Browser {
    pub async fn launch(paths: &Paths, cfg: &BrowserConfig) -> anyhow::Result<Browser>;
    /// Opens (or focuses) a window of `profile_dir` and returns that profile's browserContextId.
    pub async fn open_profile_window(&self, paths: &Paths, cfg: &BrowserConfig, profile_dir: &str) -> anyhow::Result<String>;
    pub async fn shutdown(self) -> anyhow::Result<()>;   // Browser.close, then SIGKILL after 5 s
}
```

실행 인자: `--user-data-dir=<chrome_udd>`, `--profile-directory=Default`, `--remote-debugging-port=0`, `--no-first-run`, `--no-default-browser-check`, `--load-extension=<extension_dir>`, `--disable-features=DisableLoadExtensionCommandLineSwitch`. 포트는 `<chrome_udd>/DevToolsActivePort`에서 읽는다. 시작 주소를 인자로 주지 않는다(0단계에서 로드되지 않았다). `open_profile_window`는 같은 `--user-data-dir`와 `--profile-directory=<profile_dir>`, `--new-window about:blank`로 실행 파일을 한 번 더 부르고, 새로 생긴 페이지 타깃의 `browserContextId`를 돌려준다.

`tests/common/mod.rs`: `pub struct Env { pub paths: Paths, pub cfg: BrowserConfig, _tmp: TempDir }`, `pub fn env() -> Env`(임시 `PEGBOARD_HOME`, 저장소의 `extension/bridge`), `pub async fn fixture() -> Fixture { origin }`(`/counter`, `/form`, `/opener`, `/cookie/set`, `/cookie/get`을 주는 로컬 HTTP 서버. 내용은 `spikes/lib/fixture.mjs`와 같다).

- [ ] **Step 1: 실패하는 시험 작성** (`tests/e2e_browser.rs`)

```rust
#[tokio::test] #[ignore] async fn launches_and_reports_version() {
    let e = common::env(); let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let v = b.conn.send("Browser.getVersion", json!({}), None).await.unwrap();
    assert!(v["product"].as_str().unwrap().starts_with("Chrome/"));
    b.shutdown().await.unwrap();
}
#[tokio::test] #[ignore] async fn agent_profile_window_has_its_own_context() {
    let e = common::env(); let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    assert_ne!(agent, b.human_context);
    let again = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    assert_eq!(again, agent);                       // same profile, same context
    b.shutdown().await.unwrap();
}
#[tokio::test] #[ignore] async fn navigate_waits_for_load() {
    let e = common::env(); let f = common::fixture().await; let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let t = &page_targets(&b.conn).await.unwrap()[0]; let s = b.conn.attach(&t.target_id).await.unwrap();
    b.conn.navigate(&s, &format!("{}/counter", f.origin)).await.unwrap();
    assert_eq!(b.conn.evaluate(&s, "document.title").await.unwrap(), "counter");
    b.shutdown().await.unwrap();
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 3: `cdp.rs` 구현**

`tokio-tungstenite`로 연결. 읽기 태스크 하나가 응답을 `id`로 대기자에게, 이벤트를 `broadcast` 채널(용량 1024)에 보낸다. 요청 제한 시간 30초.

- [ ] **Step 4: `browser.rs` 구현**

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1`
Expected: 3개 통과. 화면에 창이 떴다 닫힌다.

- [ ] **Step 6: 커밋**

```bash
git add crates/pegboard-core
git commit -m "feat: CDP client and browser launcher with agent profile windows"
```

---

### Task 3: 브리지 확장 프로그램과 수명 검증

0단계 A5의 조건을 여기서 푼다. 서비스 워커가 45초 쉬면 내려가 호출할 수 없었다.

**Files:**
- Create: `extension/bridge/manifest.json`, `extension/bridge/sw.js`, `crates/pegboard-core/src/bridge.rs`
- Test: `crates/pegboard-core/tests/e2e_bridge.rs`

**Interfaces:**
- Consumes: `CdpConn`, `Browser`
- Produces:

```rust
pub struct Bridge;
pub struct GroupId(pub i64);
impl Bridge {
    /// Attaches to the bridge service worker that lives in `agent_context` and keeps that CDP session open.
    pub async fn attach(conn: &CdpConn, agent_context: &str) -> anyhow::Result<Bridge>;
    pub async fn create_group(&self, title: &str, color: &str) -> anyhow::Result<(GroupId, String)>; // (group, first tab target id)
    pub async fn create_tab(&self, group: &GroupId, url: &str) -> anyhow::Result<String>;            // target id
    pub async fn add_to_group(&self, group: &GroupId, target_id: &str) -> anyhow::Result<()>;
    pub async fn members(&self, group: &GroupId) -> anyhow::Result<Vec<String>>;                     // target ids
    pub async fn close_targets(&self, target_ids: &[String]) -> anyhow::Result<()>;
    pub async fn ungroup(&self, group: &GroupId) -> anyhow::Result<()>;
}
```

확장 프로그램: `permissions: ["tabs", "tabGroups", "debugger"]`. `sw.js`는 `self.peg` 객체에 `createGroup(title, color)`, `createTab(groupId, url)`, `addToGroup(groupId, targetId)`, `members(groupId)`, `closeTargets(targetIds)`, `ungroup(groupId)`를 둔다. 타깃 ID와 탭 ID의 대응은 `chrome.debugger.getTargets()`. `createGroup`과 `createTab`은 `chrome.tabs.create({ url: "about:blank", active: false })`로 탭을 만든다. 확장은 프로필마다 따로 돌므로 이 호출은 그 워커가 속한 프로필에 탭을 만든다. `createTab`은 탭을 만든 뒤 그룹에 넣고 주소로 이동시킨다.

**수명 처리**: `Bridge::attach`가 서비스 워커 타깃에 붙은 CDP 세션을 닫지 않고 유지한다. 호출이 "세션 없음"으로 실패하면 서비스 워커 타깃을 다시 찾아 붙고 한 번 재시도한다. 타깃이 목록에 없으면 에이전트 프로필의 아무 탭에서나 `chrome-extension://<id>/` 요청이 일어나게 해 깨우는 대신, **오류 `bridge_asleep`을 낸다.** 아래 Step 5의 결과에 따라 이 부분을 바꾼다.

- [ ] **Step 1: 실패하는 시험 작성** (`tests/e2e_bridge.rs`)

```rust
#[tokio::test] #[ignore] async fn group_and_tabs_land_in_agent_profile() {
    let (e, f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap();
    let (g, first) = br.create_group("조사", "blue").await.unwrap();
    let second = br.create_tab(&g, &format!("{}/counter", f.origin)).await.unwrap();
    let all = page_targets(&b.conn).await.unwrap();
    for id in [&first, &second] {
        assert_eq!(all.iter().find(|t| &t.target_id == id).unwrap().browser_context_id.as_deref(), Some(agent.as_str()));
    }
    let m = br.members(&g).await.unwrap(); assert!(m.contains(&first) && m.contains(&second));
}
#[tokio::test] #[ignore] async fn create_tab_ignores_which_window_was_last_active() {   // Review Focus 1
    let (e, f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap(); let (g, _) = br.create_group("a", "red").await.unwrap();
    common::activate_any_target_in(&b.conn, &b.human_context).await;   // make the human window the active one
    let t = br.create_tab(&g, "about:blank").await.unwrap();
    assert_eq!(common::context_of(&b.conn, &t).await, agent);
}
#[tokio::test] #[ignore] async fn bridge_still_answers_after_two_minutes_idle() {
    let (e, _f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap(); let (g, first) = br.create_group("idle", "grey").await.unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(br.members(&g).await.unwrap(), vec![first]);
}
#[tokio::test] #[ignore] async fn close_and_ungroup() {
    let (e, _f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap(); let (g, first) = br.create_group("x", "blue").await.unwrap();
    let t = br.create_tab(&g, "about:blank").await.unwrap();
    br.close_targets(&[t.clone()]).await.unwrap();
    assert_eq!(br.members(&g).await.unwrap(), vec![first]);
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p pegboard-core --test e2e_bridge -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 3: 확장 프로그램 작성**

- [ ] **Step 4: `bridge.rs` 구현**

호출은 `Runtime.evaluate`로 `(async () => JSON.stringify(await self.peg.<fn>(...)))()`. 인자는 `serde_json`으로 직렬화해 식에 넣는다(문자열 이어 붙이기로 따옴표를 처리하지 않는다).

- [ ] **Step 5: 통과 확인, 특히 수명 시험**

Run: `cargo test -p pegboard-core --test e2e_bridge -- --ignored --test-threads=1`
Expected: 4개 통과. `bridge_still_answers_after_two_minutes_idle`이 통과하면 "CDP 세션을 붙여 두면 워커가 내려가지 않는다"가 확인된 것이다.

이 시험이 실패하면: 확장이 `pegboardd`가 여는 루프백 WebSocket에 접속해 20초마다 한 줄을 주고받도록 바꾼다(`sw.js`에 접속 코드, `bridge.rs`에 수신기). 호출 경로는 그대로 CDP를 쓴다. 바꾼 뒤 같은 시험을 다시 통과시킨다. 어느 쪽으로 됐는지 `docs/spike-findings.md`의 A5 절에 [확인]으로 적는다.

- [ ] **Step 6: 커밋**

```bash
git add extension crates/pegboard-core docs/spike-findings.md
git commit -m "feat: bridge extension for tab groups and agent-profile tabs"
```

---

### Task 4: 프록시 통과 판정 (순수 함수)

**Files:**
- Create: `crates/pegboard-core/src/proxy/mod.rs`, `crates/pegboard-core/src/proxy/filter.rs`

**Interfaces:**
- Produces:

```rust
pub struct Scope { pub context: String, pub targets: HashSet<String>, pub cdp_sessions: HashSet<String> }

pub enum ClientDecision {
    Forward,                                         // pass to the browser unchanged
    Reply(serde_json::Value),                        // answer locally with this `result`
    Error { code: i64, message: String },            // answer locally with a CDP error
    CreateTab { url: String },                       // Target.createTarget: daemon creates it via the bridge
}
pub fn client_message(msg: &serde_json::Value, scope: &Scope) -> ClientDecision;

pub enum BrowserDecision { Forward(serde_json::Value), Drop, Adopt { target_id: String, forward: serde_json::Value } }
/// `method` is the request method when `msg` is a response to a forwarded request.
pub fn browser_message(msg: serde_json::Value, request_method: Option<&str>, scope: &Scope) -> BrowserDecision;

pub const ALLOWED_METHODS: &[&str];   // the 23 methods from docs/spike-findings.md (A2) plus the ones listed below
```

**판정 규칙** (위에서부터 먼저 맞는 것):

클라이언트에서 브라우저로:

| 조건 | 결정 |
|---|---|
| `Target.createTarget` | `CreateTab { url }` (`url` 없으면 `about:blank`). `browserContextId`, `newWindow`, `background` 등 다른 인자는 버린다 |
| `Browser.close`, `Browser.crash`, `Target.disposeBrowserContext`, `Target.createBrowserContext` | `Error { -32000, "not allowed in a pegboard session" }` |
| `Target.attachToTarget`, `Target.closeTarget`, `Target.activateTarget`, `Target.getTargetInfo`에서 `targetId`가 `scope.targets`에 없음 | `Error { -32000, "No target with given id found" }` |
| `sessionId`가 있는데 `scope.cdp_sessions`에 없음 | `Error { -32001, "Session with given id not found." }` |
| `sessionId`가 없고(브라우저 수준) 메서드가 `Storage.*`, `Browser.grantPermissions`, `Browser.resetPermissions`, `Browser.setDownloadBehavior`, `Network.*`, `Fetch.*` | `Error { -32000, "browser-level call is not allowed; use a page session" }` |
| 메서드가 `ALLOWED_METHODS`에 없음 | `Error { -32601, "'<method>' is not allowed in a pegboard session" }` |
| 그 밖 | `Forward` |

`ALLOWED_METHODS`에 0단계의 23종 외에 넣을 것: `Target.closeTarget`, `Target.activateTarget`, `Target.getTargetInfo`, `Target.detachFromTarget`, `Page.reload`, `Page.navigateToHistoryEntry`, `Page.getNavigationHistory`, `Input.dispatchKeyEvent`, `Input.dispatchTouchEvent`, `DOM.getDocument`, `DOM.querySelector`, `DOM.scrollIntoViewIfNeeded`, `DOM.getBoxModel`, `DOM.focus`, `DOM.resolveNode`, `DOM.describeNode`, `Runtime.getProperties`, `Runtime.releaseObject`, `Emulation.setDeviceMetricsOverride`, `Network.getCookies`, `Network.setCookie`, `Network.deleteCookies`. `Network.*`는 위 규칙 때문에 페이지 세션에서만 통과한다.

일부러 넣지 않는 것: `Page.bringToFront`(창을 앞으로 가져와 사람의 포커스를 빼앗는다).

Task 7에서 agent-browser를 실제로 돌려 거부된 메서드를 보고 목록을 넓힌다.

브라우저에서 클라이언트로:

| 조건 | 결정 |
|---|---|
| `Target.getTargets`의 응답 | `targetInfos`에서 `scope.targets`에 없는 것을 뺀 뒤 `Forward` |
| `Target.targetCreated`이고 `targetInfo.openerId`가 `scope.targets`에 있고 `browserContextId == scope.context` | `Adopt { target_id, forward }` |
| `Target.targetCreated`, `Target.targetInfoChanged`, `Target.targetDestroyed`, `Target.attachedToTarget`, `Target.detachedFromTarget`, `Target.targetCrashed`의 대상이 `scope.targets`에 없음 | `Drop` |
| `sessionId`가 있고 `scope.cdp_sessions`에 없음 | `Drop` |
| 그 밖 | `Forward` |

- [ ] **Step 1: 실패하는 시험 작성** (`filter.rs`의 `#[cfg(test)]`). 시험 이름과 단언:

```rust
fn scope() -> Scope  // context "AGENT", targets {"T1"}, cdp_sessions {"S1"}
#[test] fn create_target_is_never_forwarded()        // {"method":"Target.createTarget","params":{"url":"https://a.test","browserContextId":"HUMAN"}} => CreateTab{url:"https://a.test"}
#[test] fn create_target_without_url_uses_blank()     // => CreateTab{url:"about:blank"}
#[test] fn foreign_target_cannot_be_attached()        // attachToTarget targetId "H1" => Error code -32000
#[test] fn own_target_can_be_attached()               // targetId "T1" => Forward
#[test] fn browser_close_is_refused()                 // => Error
#[test] fn browser_level_cookies_are_refused()        // {"method":"Storage.getCookies"} (no sessionId) => Error, message contains "browser-level"
#[test] fn page_level_cookies_pass()                  // {"method":"Network.getCookies","sessionId":"S1"} => Forward
#[test] fn unknown_session_is_refused()               // sessionId "S9" => Error -32001
#[test] fn unlisted_method_is_refused()               // {"method":"SystemInfo.getInfo"} => Error -32601
#[test] fn bring_to_front_is_refused()                // {"method":"Page.bringToFront","sessionId":"S1"} => Error -32601
#[test] fn get_targets_response_hides_foreign_targets() // result.targetInfos [T1,H1] => Forward with only T1
#[test] fn popup_of_own_tab_is_adopted()              // targetCreated {targetId:"T2", openerId:"T1", browserContextId:"AGENT"} => Adopt{"T2"}
#[test] fn popup_in_other_context_is_dropped()        // same but browserContextId "HUMAN" => Drop
#[test] fn foreign_target_events_are_dropped()        // targetCreated H2 => Drop; targetDestroyed H1 => Drop
#[test] fn events_of_foreign_sessions_are_dropped()   // {"method":"Page.loadEventFired","sessionId":"S9"} => Drop
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p pegboard-core proxy::filter`
Expected: 컴파일 오류.

- [ ] **Step 3: `filter.rs` 구현**

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p pegboard-core proxy::filter`
Expected: 15개 통과.

- [ ] **Step 5: 커밋**

```bash
git add crates/pegboard-core/src/proxy
git commit -m "feat: session-scoped CDP filter decisions"
```

---

### Task 5: 세션 CDP 엔드포인트 (프록시 서버)

**Files:**
- Create: `crates/pegboard-core/src/proxy/server.rs`
- Test: `crates/pegboard-core/tests/e2e_proxy.rs`

**Interfaces:**
- Consumes: `proxy::filter`, `CdpConn`, `Bridge`, `GroupId`
- Produces:

```rust
#[async_trait] pub trait Upstream: Send + Sync {      // one logical browser connection per client
    async fn open(&self) -> anyhow::Result<Box<dyn UpstreamConn>>;
}
#[async_trait] pub trait UpstreamConn: Send { async fn send(&mut self, text: String) -> anyhow::Result<()>; async fn recv(&mut self) -> Option<String>; }
pub struct WsUpstream { pub browser_ws_url: String }  // phase 1: a fresh WebSocket per client

pub struct Endpoint { pub url: String, pub token: String }   // ws://127.0.0.1:<port>/s/<session id>
pub struct ProxyServer;
impl ProxyServer {
    pub async fn start(upstream: Arc<dyn Upstream>, bridge: Arc<Bridge>) -> anyhow::Result<ProxyServer>;  // one listener, 127.0.0.1:0
    /// Registers a session scope. Targets and CDP sessions are tracked inside the server from then on.
    pub fn register(&self, id: &SessionId, context: &str, group: GroupId, initial_targets: Vec<String>) -> Endpoint;
    pub fn unregister(&self, id: &SessionId);          // drops client connections of that session
    pub fn targets(&self, id: &SessionId) -> Vec<String>;
    pub fn port(&self) -> u16;
}
```

동작:
- 업그레이드 요청의 경로가 `/s/<등록된 세션 ID>`가 아니거나 `Authorization: Bearer <그 세션의 토큰>`이 아니면 `401`. 토큰 비교는 상수 시간. 평문 HTTP 요청은 `404`.
- 클라이언트 연결마다 `upstream.open()`으로 브라우저 연결을 하나 연다.
- 클라이언트 메시지: `filter::client_message`의 결정대로. `CreateTab`이면 `bridge.create_tab(group, url)`을 부르고, 돌아온 타깃의 `browserContextId`가 세션의 컨텍스트와 같은지 `Target.getTargetInfo`로 확인한다. 다르면 그 탭을 닫고 CDP 오류를 돌려준다. 같으면 `scope.targets`에 넣고 `{"targetId": ...}`로 응답한다.
- `Target.attachToTarget`의 응답에서 `sessionId`를 `scope.cdp_sessions`에 넣는다. `Target.attachedToTarget` 이벤트(자동 연결)의 `sessionId`도 대상이 세션 타깃이면 넣는다.
- 브라우저 메시지: `filter::browser_message`의 결정대로. `Adopt`면 타깃을 범위에 넣고 전달한다.
- `Target.targetDestroyed`로 세션 타깃이 사라지면 범위에서 뺀다.

- [ ] **Step 1: 실패하는 시험 작성** (`tests/e2e_proxy.rs`). `common::session_fixture()`는 브라우저를 띄우고, 사람 프로필에 `/cookie/set` 탭을 하나 열고, 에이전트 창과 브리지와 그룹을 만들어 프록시에 등록한 뒤 `(env, fixture, browser, proxy, endpoint, human_target, first_target)`을 돌려준다. `common::client(&endpoint)`는 토큰을 실은 `CdpConn`이다(`CdpConn::connect_with_headers` 추가).

```rust
#[tokio::test] #[ignore] async fn rejects_missing_or_wrong_token() {
    // connect without header => HTTP 401; with "Bearer wrong" => 401; path /s/s_unknown00 => 401
}
#[tokio::test] #[ignore] async fn lists_only_session_targets() {
    // Target.getTargets via client => exactly [first_target]; human_target absent
}
#[tokio::test] #[ignore] async fn cannot_attach_to_human_target() {
    // Target.attachToTarget {targetId: human_target} => Err containing "No target with given id"
}
#[tokio::test] #[ignore] async fn created_tab_lands_in_agent_profile_even_when_human_window_is_active() {  // Review Focus 1
    // activate human target directly on browser.conn, then client Target.createTarget {url: fixture/counter}
    // => returned targetId has browserContextId == agent context, is a member of the session group,
    //    and appears in client Target.getTargets
}
#[tokio::test] #[ignore] async fn browser_level_cookie_read_is_refused_and_page_level_is_scoped() {
    // client Storage.getCookies (no session) => Err containing "browser-level"
    // attach first_target, Network.getCookies => no cookie named "spike" (it lives in the human profile)
}
#[tokio::test] #[ignore] async fn popup_from_session_tab_becomes_visible() {
    // navigate first_target to /opener, click #open via Runtime.evaluate(userGesture)
    // => within 3 s client Target.getTargets has 2 targets, both in agent context
}
#[tokio::test] #[ignore] async fn two_sessions_do_not_see_each_other() {
    // register a second session with its own group; each client's getTargets lists only its own;
    // client A attachToTarget(B's target) => Err
}
#[tokio::test] #[ignore] async fn unregister_drops_clients() {
    // proxy.unregister(id) => client's next send fails within 2 s
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 3: `server.rs` 구현**

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1`
Expected: 8개 통과.

- [ ] **Step 5: 커밋**

```bash
git add crates/pegboard-core
git commit -m "feat: per-session CDP endpoint with tab creation through the bridge"
```

---

### Task 6: 프로필 등록부와 세션 관리자

**Files:**
- Create: `crates/pegboard-core/src/profiles.rs`, `crates/pegboard-core/src/session.rs`
- Test: `crates/pegboard-core/tests/e2e_session.rs`

**Interfaces:**
- Consumes: `Browser`, `Bridge`, `ProxyServer`, `paths`, 프로토콜 타입
- Produces:

```rust
// profiles.rs
pub const DEFAULT_PROFILE: &str = "default";
pub fn profile_dir(name: &str) -> String;       // "default" -> "PegboardAgent", "work" -> "PegboardAgent-work"
pub fn valid_profile_name(name: &str) -> bool;  // 1..=32 chars of [a-z0-9-], not starting with '-'
pub struct Profiles;
impl Profiles {
    pub fn load(paths: &Paths) -> anyhow::Result<Profiles>;                 // <home>/state.json; "default" always present
    pub fn list(&self) -> Vec<String>;
    pub fn create(&mut self, name: &str) -> Result<(), RpcError>;           // invalid_params on bad or duplicate name
    pub fn delete(&mut self, name: &str, in_use: bool) -> Result<(), RpcError>; // profile_in_use, profile_not_found; "default" cannot be deleted
}

// session.rs
pub struct ProfileRuntime { pub context: String, pub bridge: Arc<Bridge> }
pub struct SessionManager;
impl SessionManager {
    pub fn new(browser: Arc<Browser>, proxy: Arc<ProxyServer>, profiles: Profiles, paths: Paths, cfg: BrowserConfig) -> Self;
    pub async fn create(&self, p: SessionCreateParams) -> Result<SessionInfo, RpcError>;
    pub fn list(&self) -> Vec<SessionInfo>;
    pub fn info(&self, id: &SessionId) -> Result<SessionInfo, RpcError>;
    pub async fn close(&self, p: SessionCloseParams) -> Result<SessionInfo, RpcError>;
    pub fn endpoint(&self, id: &SessionId) -> Result<Endpoint, RpcError>;
    /// Opens a new tab in the session group. Used to recover after the last tab was closed by hand.
    pub async fn new_tab(&self, id: &SessionId, url: &str) -> Result<String, RpcError>;
    pub async fn open_profile(&self, name: &str) -> Result<(), RpcError>;
    pub fn touch(&self, id: &SessionId);          // last_activity_at = now
}
pub fn group_title(name: Option<&str>, id: &SessionId) -> String;
pub fn group_color(index: usize) -> &'static str;   // cycles grey, blue, red, yellow, green, pink, purple, cyan, orange
```

동작:
- `create`: 프로필 이름을 확인한다(없으면 `profile_not_found`). 그 프로필의 창이 없으면 `Browser::open_profile_window`로 열고 `Bridge::attach`해 `ProfileRuntime`을 만든다. `bridge.create_group(group_title(...), color)`로 그룹과 첫 탭을 만든다. `proxy.register`로 엔드포인트를 받는다. 세션 탭마다 데몬 자신의 CDP 세션을 붙여 `Emulation.setFocusEmulationEnabled { enabled: true }`와 `Page.setWebLifecycleState { state: "active" }`를 건다. `url`이 있으면 첫 탭을 그 주소로 보낸다. 녹화 ID(`r_` + 세션 ID의 8글자)를 정한다.
- 세션에 타깃이 추가될 때마다(프록시의 `CreateTab`, `Adopt`) 같은 두 호출을 건다. `ProxyServer`에 `on_target_added` 콜백을 추가해 연결한다.
- `close`: `keep_tabs`가 아니면 `bridge.close_targets(proxy.targets(id))`, 맞으면 `bridge.ungroup`. `proxy.unregister`. 상태를 `Closed`로.
- `group_title`: 이름이 없거나 공백뿐이면 세션 ID. 60자를 넘으면 59자로 자르고 `…`를 붙인다(글자 단위, 바이트 단위 아님). 이름은 식별에 쓰지 않으므로 겹쳐도 된다.

- [ ] **Step 1: 실패하는 단위 시험 작성**

```rust
// profiles.rs
#[test] fn profile_names_are_restricted() {
    for ok in ["default", "work", "a-1"] { assert!(valid_profile_name(ok)); }
    for bad in ["", "Work", "a b", "../x", "-a", &"a".repeat(33)] { assert!(!valid_profile_name(bad)); }
}
#[test] fn profile_dirs_never_collide_with_the_human_profile() {
    assert_eq!(profile_dir("default"), "PegboardAgent"); assert_eq!(profile_dir("work"), "PegboardAgent-work");
    assert_ne!(profile_dir("default"), crate::browser::HUMAN_PROFILE_DIR);
}
#[test] fn default_profile_cannot_be_deleted_and_duplicates_are_refused() { /* create "work" twice => second is invalid_params; delete "default" => invalid_params; delete "work" with in_use=true => profile_in_use */ }
// session.rs  (Review Focus 5)
#[test] fn group_title_handles_odd_names() {
    let id = SessionId("s_abcdefgh".into());
    assert_eq!(group_title(None, &id), "s_abcdefgh"); assert_eq!(group_title(Some("   "), &id), "s_abcdefgh");
    assert_eq!(group_title(Some("가격 조사 🔎"), &id), "가격 조사 🔎");
    let long = group_title(Some(&"가".repeat(200)), &id); assert_eq!(long.chars().count(), 60); assert!(long.ends_with('…'));
}
```

- [ ] **Step 2: 실패하는 e2e 시험 작성** (`tests/e2e_session.rs`)

```rust
#[tokio::test] #[ignore] async fn create_opens_group_in_agent_window() {
    // create {name:"조사", url: fixture/counter} => info.targets.len()==1, state Active;
    // that target's context != human context; bridge members == info.targets
}
#[tokio::test] #[ignore] async fn two_sessions_with_the_same_name_get_separate_groups() {   // Review Focus 5
    // create twice with name "dup" => different ids and tab_group_id
}
#[tokio::test] #[ignore] async fn session_tab_stays_responsive_when_hidden() {
    // create session on /counter, then activate another tab in the agent window so the session tab is hidden.
    // On the daemon's own CDP session: document.visibilityState == "visible", and
    // window.__frames grows by more than 50 within 2 s  (spike A1: 0 without focus emulation)
}
#[tokio::test] #[ignore] async fn named_profile_is_isolated_from_default() {
    // profiles.create("work"); session A (default) loads /cookie/set; session B (profile "work") loads /cookie/get
    // => B's page text has cookie "" and ls null; A and B targets have different contexts
}
#[tokio::test] #[ignore] async fn close_removes_tabs_and_keep_tabs_leaves_them() {
    // close {keep_tabs:false} => targets gone from page_targets; endpoint() => session_not_found
    // other session closed with keep_tabs:true => its target still exists
}
#[tokio::test] #[ignore] async fn manually_closed_tab_is_reported_and_recoverable() {    // Review Focus 2
    // close the session's only target directly via browser.conn Target.closeTarget
    // => within 2 s info(id).targets is empty and state is still Active
    // => new_tab(id, fixture/form) returns a target in the agent context and in the session group
}
#[tokio::test] #[ignore] async fn unknown_profile_is_an_error() { /* create {profile:"nope"} => code profile_not_found */ }
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p pegboard-core profiles session && cargo test -p pegboard-core --test e2e_session -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 4: `profiles.rs`, `session.rs` 구현, `ProxyServer`에 `on_target_added` 추가**

- [ ] **Step 5: 통과 확인**

Run: 위와 같은 명령.
Expected: 단위 4개, e2e 7개 통과.

- [ ] **Step 6: 커밋**

```bash
git add crates/pegboard-core
git commit -m "feat: agent profiles and session manager with focus emulation"
```

---

### Task 7: agent-browser 명령 필터와 실행기

**Files:**
- Create: `crates/pegboard-core/src/argv.rs`, `crates/pegboard-core/src/runner.rs`
- Test: `crates/pegboard-core/tests/e2e_runner.rs`

**Interfaces:**
- Consumes: `SessionManager::endpoint`, `paths`, 프로토콜 타입
- Produces:

```rust
// argv.rs
pub enum Checked { Run(Vec<String>), TabNew { url: String } }
/// Validates a user-supplied agent-browser command line (without `--session`, `--cdp`, `--json`).
pub fn check(argv: &[String]) -> Result<Checked, RpcError>;     // error code "argv_rejected"
pub const REJECTED_FLAGS: &[&str] = &["--cdp", "--cdp-headers", "--auto-connect", "--no-auto-connect", "--provider", "-p",
    "--profile", "--executable-path", "--args", "--extension", "--state", "--config", "--engine", "--session",
    "--namespace", "--restore", "--action-policy", "--confirm-actions", "--confirm-interactive", "--allowed-domains",
    "--headed", "--proxy", "--proxy-bypass", "--relaunch", "--idle-timeout", "--pin-tab", "--no-pin-tab", "--json"];
pub const REJECTED_COMMANDS: &[&str] = &["session", "close", "connect", "install", "upgrade", "record", "stream",
    "dashboard", "mcp", "chat", "profiles", "state", "auth", "plugin", "doctor", "skills", "confirm", "deny"];

// runner.rs
pub fn find_agent_browser() -> Result<PathBuf, RpcError>;       // $PEGBOARD_AGENT_BROWSER, else vendor/agent-browser/agent-browser next to the executable or repo root
pub const AGENT_BROWSER_VERSION: &str = "0.42.0";
pub struct Runner;
impl Runner {
    pub fn new(paths: Paths) -> Result<Runner, RpcError>;       // agent_browser_missing / agent_browser_version
    /// Called once right after session creation: attaches agent-browser and turns its stream server off.
    pub async fn attach(&self, id: &SessionId, endpoint: &Endpoint) -> Result<(), RpcError>;
    pub async fn exec(&self, id: &SessionId, endpoint: &Endpoint, argv: &[String], timeout_ms: u64) -> Result<CommandExecResult, RpcError>;
    pub async fn detach(&self, id: &SessionId) -> Result<(), RpcError>;   // `close` on the agent-browser session
}
```

동작:
- `check`: 첫 번째 플래그 아닌 낱말이 명령이다. 명령이 `REJECTED_COMMANDS`에 있으면 거부. 어느 위치든 `REJECTED_FLAGS`의 플래그가 있으면 거부(`--flag=value` 꼴 포함). 단, `--` 뒤의 낱말은 글자 그대로의 인자이므로 검사하지 않는다(`fill "#q" -- --json`은 허용). `tab new <url>`은 `TabNew`로 돌려준다(프록시의 탭 생성 경로를 거치지만, agent-browser가 보내는 인자를 믿지 않고 `pegboard`가 직접 만든다는 뜻을 분명히 하려고 구분한다). 빈 `argv`는 거부.
- `exec`: `agent-browser --session peg-<id> --cdp <endpoint.url> --cdp-headers '{"Authorization":"Bearer <token>"}' --json <argv>`. 환경: `AGENT_BROWSER_SOCKET_DIR=<paths.ab_socket_dir>`, `AGENT_BROWSER_NO_VERSION_CHECK=1`, `AGENT_BROWSER_CONTENT_BOUNDARIES=1`. 부모 환경의 `AGENT_BROWSER_*` 변수는 넘기지 않는다. 표준 출력의 마지막 줄을 JSON으로 읽어 `output`에 넣는다. 기본 제한 시간 60초.
- `attach`: `exec(["get", "url"])`로 붙인 다음 곧바로 `stream disable`을 실행한다(`check`를 거치지 않는 내부 호출). 둘 중 하나라도 실패하면 오류.
- agent-browser의 응답이 `tab_gone`류의 오류(코드 `tab_gone` 또는 대상 탭 없음)면 `RpcError { code: "tab_gone" }`로 바꾼다.
- `Runner::new`: 바이너리가 없으면 `agent_browser_missing`과 메시지 `run scripts/fetch-agent-browser.sh`. `--version`이 `0.42.0`이 아니면 `agent_browser_version`과 찾은 버전, 기대 버전.

- [ ] **Step 1: 실패하는 단위 시험 작성** (`argv.rs`)

```rust
#[test] fn plain_commands_pass() { for a in [vec!["snapshot","-i"], vec!["click","@e1"], vec!["open","https://a.test"], vec!["get","url"]] { assert!(matches!(check(&s(a)), Ok(Checked::Run(_)))); } }
#[test] fn connection_flags_are_rejected_anywhere() {
    for a in [vec!["--cdp","9222","snapshot"], vec!["snapshot","--cdp=9222"], vec!["open","x","--profile","Default"],
              vec!["-p","browserbase","open","x"], vec!["snapshot","--session","other"], vec!["eval","1","--action-policy","p.json"]] {
        assert_eq!(check(&s(a)).unwrap_err().code, "argv_rejected");
    }
}
#[test] fn pegboard_owned_commands_are_rejected() { for c in ["close","session","record","stream","connect","install","mcp","state","auth"] { assert!(check(&s(vec![c])).is_err()); } }
#[test] fn literal_text_after_double_dash_is_not_inspected() { assert!(check(&s(vec!["fill","#q","--","--cdp"])).is_ok()); }
#[test] fn tab_new_is_routed_to_pegboard() { assert!(matches!(check(&s(vec!["tab","new","https://a.test"])), Ok(Checked::TabNew{..}))); }
#[test] fn empty_argv_is_rejected() { assert!(check(&[]).is_err()); }
```

- [ ] **Step 2: 실패하는 e2e 시험 작성** (`tests/e2e_runner.rs`)

```rust
#[tokio::test] #[ignore] async fn drives_the_session_tab() {
    // create session on /counter; runner.attach; exec snapshot -i => output.data.snapshot contains "button";
    // exec click "#btn" => page #clicks == "1" (read via browser.conn)
}
#[tokio::test] #[ignore] async fn stream_server_is_off_after_attach() {
    // after attach: internal exec ["stream","status"] => data.enabled == false
}
#[tokio::test] #[ignore] async fn hidden_session_tab_click_is_fast() {
    // hide the session tab (activate another), exec click "#btn" => elapsed < 1500 ms  (spike A1: about 5 s without focus emulation)
}
#[tokio::test] #[ignore] async fn tab_list_shows_only_session_tabs() {
    // human profile has a tab; exec ["tab","list"] => every listed url belongs to the session
}
#[tokio::test] #[ignore] async fn command_after_tab_was_closed_by_hand_reports_tab_gone() {   // Review Focus 2
    // close the session target via browser.conn; exec snapshot => Err code "tab_gone"
}
#[test] fn missing_binary_is_a_clear_error() {                                              // Review Focus 4
    std::env::set_var("PEGBOARD_AGENT_BROWSER", "/nonexistent/agent-browser");
    let e = Runner::new(test_paths()).unwrap_err(); assert_eq!(e.code, "agent_browser_missing"); assert!(e.message.contains("fetch-agent-browser.sh"));
}
#[test] fn wrong_version_is_a_clear_error() {                                               // Review Focus 4
    // PEGBOARD_AGENT_BROWSER points at a temp shell script that prints "agent-browser 0.1.0"
    // => code "agent_browser_version", message contains "0.1.0" and "0.42.0"
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p pegboard-core argv runner && cargo test -p pegboard-core --test e2e_runner -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 4: `argv.rs`, `runner.rs` 구현**

- [ ] **Step 5: 통과 확인, 허용목록 넓히기**

Run: 위와 같은 명령.
Expected: 단위 8개, e2e 5개 통과.

통과하지 못하는 명령이 프록시의 `-32601`(허용되지 않은 메서드) 때문이면, 그 메서드가 페이지 세션에서만 쓰이고 세션 범위를 넘지 않는지 확인한 뒤 `ALLOWED_METHODS`에 넣는다. 넣은 메서드와 이유를 `docs/spike-findings.md`의 A2 절 목록에 덧붙인다. 범위를 넘는 메서드(브라우저 수준, 다른 프로필에 닿는 것)는 넣지 않고, 그 명령이 동작하지 않는다는 것을 README의 "아직 안 되는 것"에 적는다.

- [ ] **Step 6: 커밋**

```bash
git add crates/pegboard-core docs/spike-findings.md
git commit -m "feat: agent-browser runner with argv filter"
```

---

### Task 8: 동작 로그와 민감 값 가림

**Files:**
- Create: `crates/pegboard-core/src/actionlog.rs`
- Test: `crates/pegboard-core/tests/e2e_actionlog.rs`

**Interfaces:**
- Consumes: `paths`, `CdpConn`, 프로토콜 타입
- Produces:

```rust
pub const REDACTED: &str = "[redacted]";
#[serde(rename_all = "camelCase")]
pub struct ActionEntry { pub seq: u64, pub ts: String, pub session_id: String, pub target_id: Option<String>, pub url: Option<String>,
    pub command: String, pub args: Vec<String>, pub ok: bool, pub error: Option<String>, pub duration_ms: u64, pub redacted: bool }
pub struct Manifest { pub format: u32 /* 1 */, pub recording_id: String, pub session_id: String, pub session_name: String,
    pub profile: String, pub started_at: String, pub ended_at: Option<String>, pub chrome_version: String, pub agent_browser_version: String }
pub struct ActionLog;
impl ActionLog {
    pub fn open(paths: &Paths, manifest: Manifest) -> std::io::Result<ActionLog>;   // <recordings>/<recording id>/{manifest.json,actions.jsonl}, dir 0700, files 0600
    pub fn append(&mut self, entry: ActionEntry) -> std::io::Result<()>;            // one line, flushed
    pub fn finish(&mut self, ended_at: &str) -> std::io::Result<()>;                // rewrites manifest.json
}
pub fn read(paths: &Paths, recording_id: &str) -> std::io::Result<(Manifest, Vec<ActionEntry>)>;
pub fn list(paths: &Paths) -> std::io::Result<Vec<Manifest>>;

/// Which argv positions hold typed text for a command, if any.
pub fn text_positions(argv: &[String]) -> Vec<usize>;           // fill <sel> <text>, type <sel> <text>, keyboard type <text>, keyboard inserttext <text>
/// Replaces those positions with REDACTED.
pub fn redact(argv: &[String]) -> Vec<String>;
/// JS expression evaluated on the session tab right after a text command; true when the focused element is sensitive.
pub const SENSITIVE_FOCUS_JS: &str;
```

가림 규칙:
- 글자를 넣는 명령(`fill`, `type`, `keyboard type`, `keyboard inserttext`)이 끝난 직후, 데몬의 CDP 세션에서 `SENSITIVE_FOCUS_JS`를 실행한다. 이 식은 `document.activeElement`가 `input[type=password]`이거나 `autocomplete` 값이 `current-password`, `new-password`, `one-time-code`, `cc-number`, `cc-csc`, `cc-exp` 중 하나를 포함하면 `true`다. 그림자 DOM과 같은 출처 `iframe` 안의 `activeElement`까지 따라 들어간다.
- `true`면 `redact(argv)`를 기록하고 `redacted: true`.
- **검사 자체가 실패하면(예외, 시간 초과, 탭 없음) 가린다.**
- `eval`의 인자와 모든 명령의 출력은 로그에 넣지 않는다. `args`에는 명령줄만 남긴다. `eval`의 식은 200자까지만 남긴다.
- `--`는 `args`에 그대로 남긴다. 그 뒤의 글자도 위 규칙의 대상이다.

- [ ] **Step 1: 실패하는 단위 시험 작성**

```rust
#[test] fn text_positions_cover_typing_commands() {
    assert_eq!(text_positions(&s(vec!["fill","#pw","secret"])), vec![2]);
    assert_eq!(text_positions(&s(vec!["type","@e3","--clear","--","secret"])), vec![4]);
    assert_eq!(text_positions(&s(vec!["keyboard","type","secret"])), vec![2]);
    assert!(text_positions(&s(vec!["click","#pw"])).is_empty());
}
#[test] fn redact_replaces_only_the_text() { assert_eq!(redact(&s(vec!["fill","#pw","secret"])), s(vec!["fill","#pw",REDACTED])); }
#[test] fn log_roundtrip_and_permissions() {
    // open, append 2 entries, finish => read() returns them with seq 1,2 and ended_at set;
    // actions.jsonl mode & 0o777 == 0o600; directory mode == 0o700
}
#[test] fn eval_expression_is_truncated() { /* entry for eval with a 1000-char expression => args[1].chars().count() == 200 */ }
```

- [ ] **Step 2: 실패하는 e2e 시험 작성** (`tests/e2e_actionlog.rs`). 세션을 `/form`에 만들고 실행기로 명령을 돌린 뒤 로그 파일을 읽는다. 로그 기록은 Task 9의 `command.exec` 경로에서 하지만, 여기서는 `daemon::run_command(session, runner, log, argv)` 도우미를 만들어 그 경로를 시험한다(Task 9가 이 함수를 쓴다).

```rust
#[tokio::test] #[ignore] async fn password_text_never_reaches_the_log() {
    // run fill "#pw" "hunter2-secret"; run fill "#name" "alice"
    // => file text does not contain "hunter2-secret"; the #pw entry has redacted==true and args[2]==REDACTED;
    //    the #name entry has args[2]=="alice"
}
#[tokio::test] #[ignore] async fn failed_sensitivity_check_redacts() {
    // close the tab right after the fill completes (hook in run_command for tests), run completes
    // => entry.redacted == true
}
#[tokio::test] #[ignore] async fn every_command_is_logged_with_url_and_outcome() {
    // run open /counter, snapshot -i, click "#nope" (fails) => 3 entries, seq 1..3, third ok==false with error set,
    //    url of the second ends with "/counter"
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p pegboard-core actionlog && cargo test -p pegboard-core --test e2e_actionlog -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 4: `actionlog.rs`와 `daemon::run_command` 구현**

`run_command(sessions: &SessionManager, runner: &Runner, log: &mut ActionLog, id: &SessionId, argv: &[String], timeout_ms: Option<u64>) -> Result<CommandExecResult, RpcError>`: `argv::check` → 실행(`TabNew`면 `sessions.new_tab`) → 가림 검사 → `log.append` → `sessions.touch`. `argv_rejected`로 거부된 명령도 `ok: false`로 기록한다.

- [ ] **Step 5: 통과 확인**

Run: 위와 같은 명령.
Expected: 단위 4개, e2e 3개 통과.

- [ ] **Step 6: 커밋**

```bash
git add crates/pegboard-core
git commit -m "feat: action log with redaction of sensitive input"
```

---

### Task 9: 코어 API 서버, 데몬, CLI

**Files:**
- Create: `crates/pegboard-core/src/api.rs`, `crates/pegboard-core/src/daemon.rs`
- Create: `crates/pegboard-cli/Cargo.toml`, `crates/pegboard-cli/src/main.rs`
- Test: `crates/pegboard-cli/tests/e2e_cli.rs`

**Interfaces:**
- Consumes: 앞의 모든 작업
- Produces:

```rust
// api.rs
pub async fn serve(paths: &Paths, daemon: Arc<Daemon>) -> anyhow::Result<()>;   // unix socket, mode 0600, one JSON per line
pub struct Client;
impl Client {
    pub fn connect(paths: &Paths) -> Result<Client, RpcError>;                  // not_running when no live daemon
    pub fn call<P: Serialize, R: DeserializeOwned>(&mut self, method: &str, params: P) -> Result<R, RpcError>;
}
// daemon.rs
pub struct Daemon;
impl Daemon { pub async fn start(paths: Paths) -> anyhow::Result<Arc<Daemon>>; pub async fn handle(&self, req: Request) -> Response; pub async fn shutdown(&self); }
```

코어 API 메서드(1단계):

| 메서드 | params | result |
|---|---|---|
| `browser.status` | 없음 | `BrowserStatus` |
| `browser.stop` | 없음 | `{}`. 세션을 모두 닫고 브라우저와 데몬을 끝낸다 |
| `profile.list` | 없음 | `ProfileInfo[]` |
| `profile.create` | `{ name }` | `ProfileInfo` |
| `profile.open` | `{ name }` | `{}` |
| `profile.delete` | `{ name }` | `{}` |
| `session.create` | `SessionCreateParams` | `SessionInfo` |
| `session.list` | 없음 | `SessionInfo[]` |
| `session.info` | `{ sessionId }` | `SessionInfo` |
| `session.close` | `SessionCloseParams` | `SessionInfo` |
| `command.exec` | `CommandExecParams` | `CommandExecResult` |
| `recording.list` | 없음 | `Manifest[]` |
| `recording.get` | `{ recordingId }` | `{ manifest, actions }` |

데몬 수명: `pegboard browser start`가 `pegboard daemon`을 분리된 프로세스로 띄우고(표준 출력은 `<home>/daemon.log`), 소켓이 응답할 때까지 최대 20초 기다린다. 데몬은 브라우저를 띄우고 프록시와 API 서버를 연다. 브라우저 프로세스가 끝나면 데몬도 세션을 닫고 끝난다. 데몬이 `SIGTERM`을 받으면 세션을 닫고 브라우저를 끝낸 뒤 소켓 파일을 지운다. `browser.start`는 API 메서드가 아니다(데몬이 없을 때 하는 일이라서).

낡은 소켓: `Client::connect`는 소켓 파일이 있는데 접속이 거부되면 파일을 지우고 `not_running`을 돌려준다. `pegboard daemon`은 시작할 때 살아 있는 데몬이 있으면(접속 성공) `already running`으로 끝난다.

CLI (`clap`):

```
pegboard browser start | stop | status
pegboard profile list | create <name> | open <name> | delete <name>
pegboard session create [--name N] [--url U] [--profile P] | list | info <id> | close <id> [--keep-tabs]
pegboard recording list | show <recording id>
pegboard daemon                               (내부용, 도움말에 숨김)
pegboard [--session S] <agent-browser 명령...>   그 밖의 모든 것
```

- 세션은 `--session`이 먼저, 없으면 `PEGBOARD_SESSION`. 둘 다 없으면 `no session: pass --session or set PEGBOARD_SESSION`으로 종료 코드 2.
- `--json`을 주면 결과를 JSON 한 줄로, 아니면 사람이 읽는 꼴로 낸다. 동작 명령은 agent-browser의 `output`을 그대로 낸다.
- 종료 코드: 성공 0, 명령 실패 1, 사용법 오류와 `argv_rejected` 2, 데몬 없음 3.
- `session create`는 세션 ID를 표준 출력 첫 줄에 낸다(`export PEGBOARD_SESSION=$(pegboard session create)`가 되도록).
- `recording show`는 `seq  시각  명령  결과  소요` 표를 낸다.

- [ ] **Step 1: 실패하는 시험 작성** (`crates/pegboard-cli/tests/e2e_cli.rs`). `bin()`은 빌드된 `pegboard` 경로, `run(env, args) -> (code, stdout, stderr)`.

```rust
#[test] #[ignore] fn full_flow() {
    // browser start => 0;  browser status --json => running true
    // SID = session create --name "조사" --url <fixture>/counter  => 0, stdout line 1 matches ^s_[a-z2-7]{8}$
    // --session SID snapshot -i => 0, stdout contains "button"
    // --session SID click "#btn" => 0
    // --session SID get url => stdout contains "/counter"
    // session list --json => array of 1
    // recording list --json => 1 manifest; recording show <id> => lines for snapshot, click, get
    // session close SID => 0;  --session SID snapshot => code 1, stderr contains "session_not_found"
    // browser stop => 0;  browser status => code 3
}
#[test] #[ignore] fn rejected_flags_exit_2_and_are_logged() {
    // --session SID snapshot --cdp 9222 => code 2, stderr contains "argv_rejected"; recording has an entry with ok false
}
#[test] #[ignore] fn missing_session_is_a_usage_error() { /* snapshot with no --session and no env => code 2 */ }
#[test] fn commands_without_daemon_exit_3() { /* fresh PEGBOARD_HOME: session list => code 3, stderr contains "pegboard browser start" */ }
#[test] fn stale_socket_is_cleaned_up() {                                    // Review Focus 3
    // create <home>/pegboardd.sock as a bound-then-dropped unix socket file (no listener)
    // session list => code 3 within 2 s (does not hang), and the socket file no longer exists
}
#[test] #[ignore] fn daemon_exits_when_browser_is_closed() {
    // browser start; kill the Chrome pid from browser status; within 10 s `browser status` => code 3 and no pegboard daemon process remains
}
#[test] #[ignore] fn socket_is_owner_only() { /* mode of pegboardd.sock & 0o777 == 0o600 */ }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p pegboard-cli -- --test-threads=1 && cargo test -p pegboard-cli -- --ignored --test-threads=1`
Expected: 컴파일 오류.

- [ ] **Step 3: `api.rs`, `daemon.rs` 구현**

`Daemon::start`가 `Browser::launch` → `ProxyServer::start(WsUpstream)` → `Profiles::load` → `SessionManager::new` → `Runner::new` 순으로 조립한다. `command.exec`는 `daemon::run_command`를 부른다. 세션마다 `ActionLog` 하나. 한 세션의 명령은 세션별 `tokio::sync::Mutex`로 직렬 처리하고, 다른 세션의 명령은 동시에 돈다.

- [ ] **Step 4: `main.rs` 구현**

- [ ] **Step 5: 통과 확인**

Run: `cargo build -p pegboard-cli && cargo test -p pegboard-cli -- --test-threads=1 && cargo test -p pegboard-cli -- --ignored --test-threads=1`
Expected: 기본 2개, `--ignored` 5개 통과.

- [ ] **Step 6: 커밋**

```bash
git add crates
git commit -m "feat: daemon, core API over unix socket, pegboard CLI"
```

---

### Task 10: 격리 시험 묶음, 수동 확인, README

**Files:**
- Create: `crates/pegboard-cli/tests/e2e_isolation.rs`, `docs/phase1-manual-checks.md`, `README.md`
- Modify: `docs/roadmap.md`(1단계 완료 표시), `docs/spike-findings.md`(미실시 항목 결과)

**Interfaces:**
- Consumes: `pegboard` 바이너리

이 묶음은 CLI만으로 "에이전트가 할 수 있는 일"을 시도한다. 이후 모든 단계에서 유지한다.

- [ ] **Step 1: 실패하는 시험 작성** (`e2e_isolation.rs`). 준비: 브라우저 시작, 사람 프로필에 `/cookie/set`을 연 탭 하나(시험 도우미가 `DevToolsActivePort`로 직접 붙어 만든다), 세션 A와 B(둘 다 기본 프로필), 세션 C(프로필 `work`).

```rust
#[test] #[ignore] fn agent_cannot_list_or_switch_to_human_tabs() {
    // A: tab list --json => no url containing "/cookie/set"
    // A: tab <human target id> => non-zero exit; A: get url afterwards still its own
}
#[test] #[ignore] fn agent_cannot_read_human_cookies_or_storage() {
    // A: open <fixture>/cookie/get; get text "#out" => cookie "" and ls null
    // A: cookies (agent-browser `cookies` command) => no cookie named "spike"
    // A: eval "document.cookie" => ""
}
#[test] #[ignore] fn new_tabs_always_open_in_the_agent_profile() {            // Review Focus 1
    // bring the human window to front (helper: Target.activateTarget on the human tab), then
    // A: tab new <fixture>/counter ; A: open <fixture>/form
    // => helper lists page targets: every target not in the human profile at start is in an agent context;
    //    the human profile still has exactly its 1 original tab
}
#[test] #[ignore] fn sessions_in_one_profile_share_cookies_but_not_tabs() {
    // A: open /cookie/set ; B: open /cookie/get => B sees spike=1 (shared profile, by design)
    // B: tab list => none of A's targets
}
#[test] #[ignore] fn sessions_in_different_profiles_share_nothing() {
    // A: open /cookie/set ; C: open /cookie/get => C sees cookie "" and ls null
}
#[test] #[ignore] fn escape_flags_and_commands_are_refused() {
    // for each of: ["snapshot","--cdp","<real browser port>"], ["--auto-connect","snapshot"], ["connect","<port>"],
    //              ["open","x","--profile","Default"], ["close"], ["session","list"], ["stream","enable"]
    // => exit code 2
}
#[test] #[ignore] fn agent_does_not_steal_focus() {
    // helper records which target is the active one in the human window and document.hasFocus() there;
    // A: open, click, fill, tab new  => the human window's active tab is unchanged
}
#[test] #[ignore] fn stream_port_is_closed_for_every_session() {
    // for A, B, C: no listening TCP port owned by an agent-browser process except none
    // (lsof -nP -iTCP -sTCP:LISTEN filtered by the agent-browser daemon pids in ab_socket_dir => empty)
}
```

- [ ] **Step 2: 실행**

Run: `cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1`
Expected: 8개 통과. 실패하는 것이 있으면 시험을 느슨하게 고치지 않는다. 원인을 찾아 프록시나 필터를 고치고, 고칠 수 없는 한계면 README의 "알려진 한계"와 architecture.md 11절 "남는 위험"에 적은 뒤 시험에 `#[ignore = "known limit: ..."]` 사유를 붙인다.

- [ ] **Step 3: 수동 확인 목록 작성과 수행** (`docs/phase1-manual-checks.md`)

0단계에서 사람 손이 필요해 하지 못한 것들이다. 각 항목에 절차, 기대 결과, 실제 결과 칸을 둔다. 수행은 사용자가 한다. 실행자는 문서를 만들고 사용자에게 요청한다.

| # | 확인 | 절차 요약 |
|---|---|---|
| M1 | 화면 잠금 중 자동화 | 세션에서 10초 간격으로 `snapshot`과 `click`을 2분간 반복하는 셸 루프를 돌리고 화면을 잠근다. 풀고 나서 `recording show`로 실패와 소요 시간을 본다 |
| M2 | 다른 데스크톱 공간 | 에이전트 창을 다른 공간으로 옮기고 같은 루프 |
| M3 | 디스플레이 절전 | 같은 루프를 돌리며 화면을 끈다 |
| M4 | 브라우저 설정 누출 | 사람 프로필에서 홈 버튼 표시, 기본 검색 엔진, 다운로드 위치를 바꾸고 에이전트 창의 설정을 본다 |
| M5 | 다운로드 경로 | 세션에서 파일을 내려받고 어디에 저장되는지 본다 |
| M6 | 사람이 탭을 그룹 사이로 끌기 | 세션 A의 탭을 B의 그룹으로 끈 뒤 `session info`와 `tab list` |
| M7 | 에이전트 창을 닫기 | 창을 닫고 `session info`, 이어서 명령 실행 |

- [ ] **Step 4: README 작성**

구성: 무엇인가(한 문단), 상태(1단계, 순정 Chrome for Testing 위에서 동작, 설치형 브라우저 아님), 준비(Rust, `scripts/fetch-agent-browser.sh`, Chrome for Testing), 빠른 시작(Task 9 `full_flow`의 명령), 세션과 프로필, 아직 안 되는 것(MCP, 화면 녹화, 정책과 확인, 원격, 프로필 복사), 알려진 한계("설계와 다르게 가는 것 한 가지"의 내용과 "이 브라우저 데이터 디렉터리에 실제 계정을 넣지 말 것"), 시험 실행법, 라이선스.

- [ ] **Step 5: 전체 시험**

Run: `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && cargo test --workspace -- --ignored --test-threads=1`
Expected: 모두 통과.

- [ ] **Step 6: 문서 갱신**

`docs/roadmap.md` 1단계에 완료 날짜와 "동작하는 것" 각 줄의 확인 여부. `docs/spike-findings.md`의 "미실시" 표에 M1~M7의 결과(사용자가 수행한 것만). `docs/architecture.md` 12절의 A5, A12에 1단계에서 확인된 내용.

- [ ] **Step 7: 커밋**

```bash
git add crates README.md docs
git commit -m "test: isolation suite; docs: README and manual checks for phase 1"
```
