# 3a단계: Chromium 포크 뼈대 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 맥미니에서 빌드한 Pegboard.app을 켜면 `pegboardd`가 자식으로 떠서 파이프로 CDP를 받고, 확장 프로그램 대신 C++ `Pegboard` 도메인으로 탭그룹과 에이전트 프로필을 다루며, 1~2단계의 시험 전체가 디버깅 포트 없이 통과한다.

**Architecture:** `pegboardd`의 모든 브라우저 연결(주 연결, 세션 프록시, 다운로드 감시, 방어 장치, 녹화기)을 물리 연결 하나(`link::BrowserLink`, 웹소켓 또는 파이프) 위의 가상 연결로 바꾼다. 가상 연결 하나는 `Target.attachToBrowserTarget`으로 연 브라우저 세션 하나다. 브리지는 trait로 뽑아 확장 판(순정 모드)과 도메인 판(포크)을 둔다. "사람 컨텍스트 하나"는 "에이전트 컨텍스트 집합에 없으면 사람"으로 뒤집는다. 포크 쪽은 Chromium 트리 밖의 `browser/`(버전, 패치 묶음, 새 소스, 스크립트)로 관리하고, 패치 넷(브랜딩, 파이프 fd, 부트스트랩, `Pegboard` 도메인)을 얹는다.

**Tech Stack:** Rust(기존 워크스페이스), tokio, serde_json, tokio-tungstenite, `libc`(파이프 fd). Chromium Extended Stable `156.0.8078.109`, C++, GN/ninja, depot_tools. 순정 모드 시험은 Chrome for Testing 157, agent-browser 0.42.0.

**Spec:** [2026-11-21-phase3a-fork-skeleton-design.md](./2026-11-21-phase3a-fork-skeleton-design.md). 앞 문서: [architecture.md](../architecture.md) 4절 결정 3과 5절, [spike-findings.md](../spike-findings.md) A6·A10, [2b 계획](./2026-11-17-phase2b-recording.md).

## Global Constraints

- 설계 2절의 실패 조건이 하나라도 생기면 그 작업은 끝난 것이 아니다: 에이전트나 브라우저 안의 확장이 `Pegboard` 도메인을 부른다, 가상 연결 사이에 이벤트·응답이 샌다, 포크 모드에서 TCP 포트가 DevTools를 받는다, 사람 프로필에 탭을 만들거나 그 탭을 그룹에 넣는다, `pegboardd`가 죽으면 브라우저도 죽는다.
- Chromium 버전 `156.0.8078.109`(작업 7 착수 때 최신 Extended Stable이 다르면 그것으로 하고 설계 문서 F5를 고친다).
- 이름 `Pegboard`, 번들 ID `app.pegboard.browser`, 사용자 데이터 `~/Library/Application Support/Pegboard`.
- 에이전트 프로필 = 디렉터리 이름이 정확히 `PegboardAgent`이거나 `PegboardAgent-`로 시작하는 프로필(`profiles::profile_dir`). 그 밖의 모든 프로필은 사람 것. **모르는 컨텍스트는 사람 것으로 본다.**
- 데몬 재시작(F8): 데몬이 끝나면 브라우저가 다시 띄운다, **1분 안에 3번까지**. 데몬이 종료 코드 **4**(다른 데몬이 잠금을 쥐고 있다)로 끝나면 다시 띄우지 않는다. 브라우저 종료 때 데몬을 **5초** 기다린 뒤 `SIGTERM`.
- 파이프: 데몬 쪽에서 **fd 3을 읽고 fd 4에 쓴다**. 메시지는 NUL(`\0`)로 끝나는 JSON. 한 메시지 최대 **256MiB**(`cdp::QUEUED_MAX_MESSAGE`와 같다).
- `Pegboard` 도메인은 파이프 클라이언트의 브라우저 세션 중 `Pegboard.claim`을 처음 부른 하나에만 있다. 다른 세션과 신뢰되지 않은 클라이언트(`chrome.debugger`)에는 없는 메서드(`-32601`)다.
- 순정 모드는 그대로 동작해야 한다(F4): 1~2단계 시험 전체가 순정 Chrome으로 통과한다.
- 1~2단계의 보장을 약하게 만들지 않는다: `argv::check`, 동작 기록의 가림 규칙, 프록시 필터의 기본 거부, 요청 가드, 클립보드 방어, 다운로드 귀속, 녹화의 사람 탭 제외와 지연 기준(명령 하나에 녹화가 더하는 시간 중앙값 60ms 이하), 사람 프로필에 아무것도 걸지 않는 것.
- 애매하면 닫는 쪽으로: 주인을 모르는 CDP 메시지는 아무 가상 연결에도 주지 않는다, 모르는 컨텍스트는 사람 것, 에이전트 프로필인지 모르는 프로필 디렉터리는 도메인이 거부한다.
- **실제 브라우저를 띄우는 것과 Chromium 빌드는 구현 에이전트가 이 기기에서 하지 않는다.** Rust 작업(1~6)의 구현 에이전트는 코드, 브라우저 없는 시험(`cargo test --workspace`), `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`까지 하고, 보고서에 (1) 돌려야 할 명령을 그대로, (2) 시험마다 기대 결과, (3) 새 브라우저 시험마다 "이 한 줄을 이렇게 바꾸면 실패한다"는 변형 하나를 적는다. 컨트롤러가 맥미니에서 돌리고 변형까지 확인한다.
- **C++ 작업(7~9)은 맥미니의 Chromium 트리에서 한다.** 컨트롤러가 맥미니에 작업자를 띄운다(Orca). 작업자는 트리에서 고치고 빌드하고 `browser/scripts/export.sh`로 패치를 뽑아 이 저장소에 커밋한다. 빌드는 백그라운드로 돌리고 끝을 기다린다. 맥미니 디스크 여유가 **100GB** 미만이면 작업 7을 시작하지 않고 보고한다.
- 실제 브라우저 시험은 임시 `PEGBOARD_HOME`만 쓴다. 새 시험용 환경 변수는 `testenv::var`로 읽는다(`PEGBOARD_TEST_HOOKS`가 있을 때만).
- 문서와 코드에 홈 디렉터리 경로, 사용자 이름, 기기 이름을 쓰지 않는다. 문서는 한국어. 커밋 메시지는 영어, 끝에 `Co-Authored-By: <구현한 모델 이름> <noreply@anthropic.com>`. 푸시와 main 병합은 컨트롤러가 한다.

## Review Focus

설계가 암시하지만 시험이 놓치기 쉬운 것. 각 줄의 시험은 괄호 안 작업에 넣었다.

1. **한 가상 연결이 다른 가상 연결의 세션을 쓴다.** 세션 프록시는 에이전트가 보낸 `sessionId`를 거르지만, 중계 계층이 "보낸 쪽이 그 세션의 주인인가"를 보지 않으면 프록시의 버그 하나가 곧 격리 붕괴다. 주인이 아닌 `sessionId`로 보낸 메시지는 브라우저로 가지 않고 그 자리에서 오류로 돌아온다(작업 2 `a_virtual_connection_cannot_use_another_ones_session`).
2. **자동 연결로 생긴 자식 세션의 주인이 틀린다.** 브라우저 수준 `setAutoAttach`(flatten)로 붙은 자식, 그 자식에서 다시 붙은 손자(iframe, 워커), 같은 타깃에 두 가상 연결이 각자 붙은 경우. 주인은 `Target.attachedToTarget`을 실어 온 세션의 주인이다(작업 2 `auto_attached_children_belong_to_the_attaching_session`, `nested_children_follow_their_parent`, 작업 1 실측).
3. **물리 연결이 끊긴다** (브라우저가 죽음, 파이프 EOF). 모든 가상 연결이 끝나고 걸려 있던 요청이 실패해야 한다. 하나라도 응답을 영원히 기다리면 데몬이 멈춘다(작업 2 `link_end_ends_every_virtual_connection`).
4. **에이전트 프로필이 내려갔다 다시 올라와 컨텍스트 ID가 바뀐다.** 옛 ID는 집합에서 빠지고 새 ID가 들어가야 한다. 옛 ID가 남으면 무해하지만, 새 ID가 늦게 들어가는 사이에 다운로드 정책이 안 걸린 탭이 생긴다(작업 4 `a_reloaded_agent_profile_gets_its_new_context`, 작업 8 `profile_events_follow_unload_and_reload`).
5. **이미 데몬이 도는데 포크가 데몬을 또 띄운다** (순정 모드 데몬, 또는 두 번째 브라우저 인스턴스). 새 데몬은 종료 코드 4로 끝나고 브라우저는 다시 띄우지 않으며 기존 데몬은 영향이 없다(작업 6 `hosted_daemon_exits_4_when_the_lock_is_held`, 작업 9 `a_held_lock_is_not_retried`).

## 파일 구조

| 파일 | 할 일 | 작업 |
|---|---|---|
| `crates/pegboard-core/tests/e2e_link.rs` | 웹소켓 하나 위 브라우저 세션 둘의 독립성 실측. 이후 중계 계층 실측으로 남는다 | 1, 3 |
| `crates/pegboard-core/src/link/mod.rs` | `BrowserLink`, `VirtualConn` 공개 API | 2 |
| `crates/pegboard-core/src/link/mux.rs` | ID 다시 매기기, 세션 주인 표, 라우팅 | 2 |
| `crates/pegboard-core/src/link/transport.rs` | 웹소켓·파이프 물리 연결 | 2 |
| `crates/pegboard-core/src/cdp.rs` | `CdpConn::over`, `over_queued` (가상 연결 위) | 3 |
| `crates/pegboard-core/src/proxy/server.rs` | `LinkUpstream` (`WsUpstream` 대체) | 3 |
| `downloads.rs`, `guard.rs`, `recording/recorder.rs`, `session.rs`, `service.rs` | `ws_url` 대신 `BrowserLink`, 사람 판정 집합화 | 3, 4 |
| `crates/pegboard-core/src/contexts.rs` | `AgentContexts` | 4 |
| `crates/pegboard-core/src/bridge.rs` → `bridge/mod.rs`, `bridge/extension.rs`, `bridge/domain.rs` | `TabBridge` trait와 두 구현 | 5 |
| `crates/pegboard-core/src/browser.rs` | `BrowserConfig` 종류, `Browser::hosted`, 프로필 창 | 5, 6 |
| `crates/pegboard-core/src/settings.rs` | `config.toml`의 `[browser] kind` | 6 |
| `crates/pegboard-cli/src/main.rs` | `daemon --cdp-pipe`, `browser start/stop` 포크 모드 | 6 |
| `browser/` (설계 4.1의 구조 그대로) | 버전, 패치, 새 소스, 스크립트 | 7~9 |
| `crates/pegboard-core/tests/e2e_domain.rs` | `Pegboard` 도메인 실측 | 8 |
| `crates/pegboard-core/tests/e2e_bootstrap.rs` | 앱 시작·데몬 실행·재시작·종료 | 9 |

---

### Task 1: 전제 확인 — 웹소켓 하나 위 브라우저 세션 둘 (만들기 전에)

**목적:** 설계 F7의 전제를 순정 Chrome으로 확인한다. 틀리면 작업 2 이후를 멈추고 설계를 다시 본다.

**Files:**
- Create: `crates/pegboard-core/tests/e2e_link.rs`
- Modify: `docs/spike-findings.md` (끝에 "3a 작업 1" 절)

**Interfaces:**
- Consumes: `common::launched()`(브라우저 띄우기), `Browser::ws_url`
- Produces: 없음(실측). 결과표가 작업 2의 라우팅 규칙을 확정한다.

- [ ] **Step 1: 시험을 쓴다.** 모두 `#[ignore]`, 웹소켓 하나를 직접 열어 JSON을 손으로 보내는 작은 도우미(`RawLink`: `send(json) -> id`, `next_for(session) -> Value`, 시간 제한 3초)를 파일 안에 둔다. 브라우저 세션 S1, S2를 `Target.attachToBrowserTarget`으로 연다.

```rust
#[tokio::test] #[ignore] async fn discover_targets_on_one_session_stays_there()
// S1만 setDiscoverTargets(true). 새 탭을 만들면 S1에 targetCreated, S2와 루트(sessionId 없음)에는 없다.
#[tokio::test] #[ignore] async fn auto_attach_on_one_session_stays_there()
// S1만 setAutoAttach{autoAttach:true, waitForDebuggerOnStart:false, flatten:true}. 새 탭의 attachedToTarget은 S1에만, 그 자식 세션의 이벤트(Runtime.enable 후 consoleAPICalled)는 자식 sessionId로만 온다.
#[tokio::test] #[ignore] async fn page_session_events_go_only_to_that_session()
// S1, S2가 같은 탭에 각자 attachToTarget(flatten). 각자 Runtime.enable. 한쪽만 enable했을 때 다른 쪽 세션 ID로는 consoleAPICalled가 오지 않는다.
#[tokio::test] #[ignore] async fn download_behavior_fetch_and_screencast_are_per_session()
// Browser.setDownloadBehavior는 컨텍스트별로 S1, S2에서 다른 값이 서로 덮지 않는지; Fetch.enable(S1의 페이지 세션)이 S2의 같은 탭 세션 요청을 멈추지 않는지; Page.startScreencast가 세션별 스트림인지.
#[tokio::test] #[ignore] async fn detaching_a_browser_session_detaches_its_children()
// S1에서 붙인 페이지 세션 P1. S1을 루트에서 detachFromTarget. P1로 보낸 명령이 "session not found"로 실패하는지, 아니면 살아 있는지 기록(어느 쪽이든 PASS로 두고 출력).
#[tokio::test] #[ignore] async fn proxy_allowlist_behaves_the_same_on_a_browser_session()
// proxy::filter의 허용 메서드 중 브라우저 수준 것(Target.*, Browser.getVersion 등)을 S1에서 보낸 결과가 루트에서 보낸 것과 같은 모양인지.
```

- [ ] **Step 2: `cargo test -p pegboard-core --test e2e_link --no-run`으로 컴파일만 확인한다.**
- [ ] **Step 3: 컨트롤러가 맥미니에서 돌린다.** `cargo test -p pegboard-core --test e2e_link -- --ignored --nocapture --test-threads=1`. 기대: 앞 넷과 마지막이 PASS. `detaching_...`은 결과를 출력한다.
- [ ] **Step 4: 결과를 `docs/spike-findings.md`에 표로 적는다** (항목, 관찰, 작업 2에 주는 결론). 하나라도 기대와 다르면 컨트롤러는 작업 2로 가지 않고 사용자에게 설계 재검토를 가져간다.
- [ ] **Step 5: 커밋** `test: browser sessions over one connection behave like separate connections`

---

### Task 2: 중계 계층 (`link`)

**Files:**
- Create: `crates/pegboard-core/src/link/{mod.rs,mux.rs,transport.rs}`
- Modify: `crates/pegboard-core/src/lib.rs` (`pub mod link;`), `Cargo.toml`(필요하면 `libc`는 이미 있음)
- Test: 같은 파일의 `#[cfg(test)]` (가짜 브라우저)

**Interfaces:**
- Produces:

```rust
// link/mod.rs
#[derive(Clone)] pub struct BrowserLink { /* Arc */ }
impl BrowserLink {
    pub async fn websocket(ws_url: &str) -> anyhow::Result<BrowserLink>;
    /// 데몬 쪽 끝: `read`에서 읽고 `write`에 쓴다(포크 모드: fd 3, 4).
    pub fn pipe(read: std::os::fd::OwnedFd, write: std::os::fd::OwnedFd) -> anyhow::Result<BrowserLink>;
    /// 새 가상 연결 = 새 브라우저 세션(Target.attachToBrowserTarget).
    pub async fn open(&self) -> anyhow::Result<VirtualConn>;
    /// 물리 연결이 끝나면 풀린다.
    pub async fn closed(&self);
    pub fn is_closed(&self) -> bool;
}
pub struct VirtualConn { /* ... */ }
impl VirtualConn {
    pub fn split(self) -> (VirtualSender, VirtualReceiver);
}
#[derive(Clone)] pub struct VirtualSender;   impl VirtualSender { pub fn send(&self, text: String) -> anyhow::Result<()>; }
pub struct VirtualReceiver;                  impl VirtualReceiver { pub async fn recv(&mut self) -> Option<String>; }
// VirtualSender를 모두 놓으면 가상 연결이 닫힌다(브라우저 세션과 자식 세션을 뗀다).
```

라우팅 규칙(설계 4.4, 작업 1 결과로 확정):
- 가상 연결이 보낸 메시지의 `id`는 물리 연결 전체에서 유일하게 다시 매기고, 응답에서 원래 값으로 되돌려 그 가상 연결에만 준다.
- `sessionId`가 없으면 그 가상 연결의 브라우저 세션 ID를 붙인다. 있으면 그 가상 연결이 주인인 세션이어야 하고, 아니면 브라우저로 보내지 않고 `{"id":<원래>,"error":{"code":-32001,"message":"Session with given id not found."}}`를 돌려준다.
- 주인 표: 브라우저 세션은 연 가상 연결 것. `Target.attachedToTarget` 이벤트의 `params.sessionId`는 그 이벤트를 실어 온 `sessionId`의 주인 것. `Target.attachToTarget` 응답의 `sessionId`는 요청한 가상 연결 것. `Target.detachedFromTarget`에서 지운다.
- 주인을 모르는 메시지와 `sessionId` 없는 이벤트는 아무에게도 주지 않고 `tracing::debug`로 남긴다.
- 가상 연결이 닫히면 그 연결 소유의 자식 세션과 브라우저 세션을 루트에서 `Target.detachFromTarget`으로 떼고 표에서 지운다(작업 1에서 자식이 저절로 떨어지는 것으로 나와도 표는 지운다).
- 물리 연결이 끝나면 모든 `VirtualReceiver::recv`가 `None`을 돌려주고, 이후 `send`는 오류.
- 파이프 전송: NUL 구분, 한 메시지 256MiB 넘으면 그 물리 연결을 끝낸다(웹소켓 쪽 한도와 같은 동작).

- [ ] **Step 1: 실패하는 시험을 쓴다** (가짜 브라우저 = 메모리 채널 위에서 `attachToBrowserTarget`에 `B1`, `B2`…를 주고 받은 메시지를 기록하는 작은 구조체).

```rust
#[tokio::test] async fn ids_are_rewritten_and_restored()
// 두 가상 연결이 같은 id 1로 보낸다 → 가짜 브라우저가 본 id는 서로 다르다 → 각자 id 1의 응답만 받는다.
#[tokio::test] async fn a_message_without_session_goes_to_its_browser_session()
#[tokio::test] async fn a_virtual_connection_cannot_use_another_ones_session()
// V2가 V1의 브라우저 세션 B1로 보낸다 → 가짜 브라우저는 아무것도 받지 않는다, V2는 code -32001을 받는다.
#[tokio::test] async fn auto_attached_children_belong_to_the_attaching_session()
// 가짜 브라우저가 sessionId=B1로 attachedToTarget{sessionId:"C1"}를 보낸 뒤 sessionId=C1 이벤트 → V1만 받는다, V2는 아무것도 안 받는다.
#[tokio::test] async fn nested_children_follow_their_parent()
// C1 위로 attachedToTarget{sessionId:"G1"} → G1 이벤트는 V1으로.
#[tokio::test] async fn unknown_session_messages_go_nowhere()
#[tokio::test] async fn closing_a_virtual_connection_detaches_its_sessions()
// V1 drop → 가짜 브라우저가 C1, B1의 Target.detachFromTarget을 받는다. 이후 C1 이벤트는 아무에게도 안 간다.
#[tokio::test] async fn link_end_ends_every_virtual_connection()
// 가짜 브라우저 끊기 → V1, V2 recv가 None, 걸려 있던 요청은 3초 안에 오류.
#[tokio::test] async fn pipe_frames_are_nul_delimited()
// os_pipe 두 쌍으로 PipeTransport: 두 메시지를 한 번에 쓰고 나눠 읽기, 쪼개진 쓰기를 이어 읽기, 256MiB 초과면 끝.
```

- [ ] **Step 2: `cargo test -p pegboard-core link::` → 컴파일 실패 또는 FAIL 확인.**
- [ ] **Step 3: `link` 모듈을 구현한다.** 물리 연결마다 읽기·쓰기 작업 하나씩, 주인 표는 `Mutex<HashMap<String, VconnId>>`, 대기 요청은 `HashMap<u64, (VconnId, Value /*원래 id*/)>`. 메시지는 `serde_json::Value`로 한 번만 파싱한다.
- [ ] **Step 4: `cargo test -p pegboard-core link::` 통과, `cargo clippy`, `cargo fmt --check`.**
- [ ] **Step 5: 커밋** `feat: one browser connection carries many virtual ones`

---

### Task 3: 모든 브라우저 연결을 중계 계층 위로 (순정 모드)

**Files:**
- Modify: `cdp.rs`, `proxy/server.rs`, `browser.rs`, `downloads.rs`, `guard.rs`, `recording/recorder.rs`, `session.rs`, `service.rs`, `tests/common/mod.rs`
- Test: `tests/e2e_link.rs`(추가), 기존 e2e 전체

**Interfaces:**
- Consumes: 작업 2의 `BrowserLink`, `VirtualConn`
- Produces:

```rust
impl CdpConn {
    pub async fn over(link: &BrowserLink) -> anyhow::Result<CdpConn>;
    pub async fn over_queued(link: &BrowserLink) -> anyhow::Result<(CdpConn, mpsc::UnboundedReceiver<CdpEvent>)>;
}
pub struct LinkUpstream { pub link: BrowserLink }   // impl proxy::server::Upstream
// Browser: `pub link: BrowserLink` 추가. `ws_url`은 순정 모드에서만 Some: `pub ws_url: Option<String>`.
// Downloads::new, Guard 시작, RecorderOptions는 ws_url: String 대신 link: BrowserLink.
```

- `CdpConn::open`의 읽기·쓰기 루프를 "문자열을 보내고 받는 통로" 위로 옮겨, 웹소켓(`connect`, 시험용으로 남김)과 가상 연결(`over`)이 같은 코드를 쓴다.
- `Browser::launch`(순정)는 `DevToolsActivePort`에서 얻은 주소로 `BrowserLink::websocket` 하나를 열고, 그 위에 주 연결 `conn = CdpConn::over(&link)`를 둔다.
- `WsUpstream`은 지운다. 프록시는 클라이언트 연결마다 `link.open()`.

- [ ] **Step 1: 시험을 더한다** (`e2e_link.rs`, `#[ignore]`):

```rust
#[tokio::test] #[ignore] async fn the_daemon_opens_one_browser_connection()
// 데몬(service::Daemon::start)으로 세션 둘 + 녹화 + 가드가 도는 상태에서, 브라우저의 DevTools 웹소켓 연결 수가 1이다.
// 잰다: `lsof -a -p <chrome pid> -iTCP:<port> -sTCP:ESTABLISHED` 줄 수, 또는 데몬의 test 훅 `test.linkStats` → {physical:1, virtual:n}.
#[tokio::test] #[ignore] async fn sessions_stay_isolated_over_one_connection()
// 세션 A의 프록시 클라이언트가 Target.setDiscoverTargets 후 세션 B가 탭을 연다 → A는 B의 타깃 이벤트를 받지 않는다.
```

- [ ] **Step 2: 호출하는 쪽을 모두 옮긴다.** `grep -rn "ws_url" crates/pegboard-core/src`가 `browser.rs`의 순정 모드 실행부와 `cdp.rs`의 `connect`만 남기면 끝.
- [ ] **Step 3: `cargo test --workspace`, clippy, fmt 통과.**
- [ ] **Step 4: 컨트롤러가 맥미니에서 순정 모드 e2e 전체를 돌린다.** `cargo test -p pegboard-core -- --ignored --test-threads=1`, 그리고 `e2e_recording`의 `recording_overhead_meets_the_targets`. 기대: 모두 PASS, 녹화 지연 중앙값 60ms 이하.
- [ ] **Step 5: 커밋** `refactor: every browser connection rides the link`

---

### Task 4: 사람 판정을 집합으로 (F10)

**Files:**
- Create: `crates/pegboard-core/src/contexts.rs`
- Modify: `browser.rs`(`human_context` 제거), `downloads.rs`, `guard.rs`, `recording/{recorder.rs,screen.rs}`, `session.rs`, `service.rs`, `tests/e2e_bridge.rs`, `tests/common/mod.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Default)] pub struct AgentContexts { /* Arc<RwLock<..>> */ }
impl AgentContexts {
    pub fn set(&self, profile_dir: &str, context: &str);   // 같은 프로필의 옛 컨텍스트는 지운다
    pub fn forget_context(&self, context: &str);
    pub fn is_agent(&self, context: &str) -> bool;          // 빈 문자열, 모르는 값은 false
    pub fn context_of(&self, profile_dir: &str) -> Option<String>;
    pub fn profile_of(&self, context: &str) -> Option<String>;
}
pub fn is_agent_profile_dir(dir: &str) -> bool;   // "PegboardAgent" 또는 "PegboardAgent-" 접두
// recording::screen::may_record(target_context, session_context, agents: &AgentContexts) -> bool
```

- `downloads.rs`, `guard.rs`, 녹화의 `== human_context` 비교를 모두 `!agents.is_agent(ctx)`로 바꾼다. 순정 모드는 `open_profile_window`가 `set`을 부른다.
- 시험 도우미가 쓰던 "사람 컨텍스트"(`common::activate_any_target_in(&b.conn, &b.human_context)` 등)는 `common::human_context(&conn)`(사람 프로필 `Default`의 페이지에서 찾음)로 바꾼다.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn an_unknown_context_counts_as_human()       // is_agent("X") == false, is_agent("") == false
#[test] fn a_reloaded_agent_profile_gets_its_new_context()
// set("PegboardAgent","C1"); set("PegboardAgent","C2") → is_agent("C1")==false, is_agent("C2")==true
#[test] fn agent_profile_dirs_are_recognized()
// "PegboardAgent", "PegboardAgent-work" → true; "Default", "Profile 1", "PegboardAgentX", "pegboardagent" → false
#[test] fn may_record_needs_an_agent_context()         // screen.rs: 세션 컨텍스트와 같아도 is_agent가 false면 false
```
`downloads.rs`와 `guard.rs`의 기존 단위 시험 중 `human_context`를 쓰는 것은 `AgentContexts`로 옮기고, 사람 프로필이 둘인 경우(사람 컨텍스트 `H1`, `H2`)를 하나씩 더한다: `policy_is_not_applied_to_a_second_human_profile`, `guard_treats_a_second_human_profile_as_foreign`.

- [ ] **Step 2: FAIL 확인 → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt.**
- [ ] **Step 5: 컨트롤러가 맥미니에서 순정 모드 e2e 전체.** 기대: PASS.
- [ ] **Step 6: 커밋** `refactor: anything that is not an agent profile is human`

---

### Task 5: 브리지 trait와 도메인 판 (가짜 브라우저로)

**Files:**
- Create: `crates/pegboard-core/src/bridge/{mod.rs,extension.rs,domain.rs}` (`bridge.rs`를 옮김)
- Modify: `session.rs`, `browser.rs`(`open_profile_window`), `tests/e2e_bridge.rs`

**Interfaces:**
- Produces:

```rust
#[async_trait] pub trait TabBridge: Send + Sync {
    async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)>;
    async fn create_tab(&self, group: &GroupId, url: &str, title: &str, color: &str) -> Result<(String, Option<GroupId>)>;
    async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()>;
    async fn members(&self, group: &GroupId) -> Result<Vec<String>>;
    async fn close_targets(&self, target_ids: &[String]) -> Result<()>;
    async fn ungroup_targets(&self, target_ids: &[String]) -> Result<()>;
    async fn ungroup(&self, group: &GroupId) -> Result<()>;
    async fn follow_activity(self: Arc<Self>, on: ActivityFn);
}
pub struct ExtensionBridge;   // 지금 Bridge 그대로 (attach, detach_for_test 포함)
pub struct DomainBridge { conn: CdpConn, agent_context: String }
impl DomainBridge { pub fn new(control: CdpConn, agent_context: &str) -> DomainBridge; }
```

`DomainBridge`가 보내는 것(작업 8의 C++이 이 모양을 구현한다):

| Rust 메서드 | CDP | 응답 |
|---|---|---|
| `create_group(title,color)` | `Pegboard.createGroup {browserContextId, title, color}` | `{groupId: integer, targetId}` (빈 탭 하나를 만들어 그룹을 연다) |
| `create_tab(g,url,title,color)` | `Pegboard.createTab {browserContextId, groupId, url, title, color}` | `{targetId, newGroupId?}` (그룹이 사라졌으면 새로 만들고 `newGroupId`) |
| `add_to_group` | `Pegboard.addToGroup {groupId, targetId}` | `{}` |
| `members` | `Pegboard.getGroupMembers {groupId}` | `{targetIds: string[]}` |
| `close_targets` | `Pegboard.closeTargets {targetIds}` | `{}` |
| `ungroup_targets` | `Pegboard.ungroupTargets {targetIds}` | `{}` |
| `ungroup` | `Pegboard.ungroup {groupId}` | `{}` |
| `follow_activity` | 이벤트 `Pegboard.tabActivated {browserContextId, targetId, timestamp /*ms since epoch*/}` 중 자기 컨텍스트 것 | — |
| (Browser) `open_profile_window(dir)` | `Pegboard.openProfileWindow {profileDirectory}` | `{browserContextId}` |
| (Browser) 프로필 표 | `Pegboard.getProfiles` → `{profiles:[{profileDirectory, browserContextId, isAgent}]}`, 이벤트 `Pegboard.profileLoaded`/`profileUnloaded {profileDirectory, browserContextId}` | — |
| (Browser) 주인 | `Pegboard.claim` | `{}` |

URL 허용은 `about:blank`, 절대 `http:`/`https:`/`data:`만(지금 `allowedUrl`과 같다). `data:`는 도메인이 직접 연다(확장 판의 CDP 우회가 필요 없다). `close_targets` 앞뒤의 조용한 구간(`CLOSE_QUIET`, 2초)은 두 구현 모두 지킨다.

- [ ] **Step 1: 시험을 쓴다.** `domain.rs`의 단위 시험은 가짜 브라우저에 위 표의 요청이 그대로 가는지와 응답 해석: `create_tab_sends_context_and_group`, `a_vanished_group_comes_back_as_new_group_id`, `activity_from_other_contexts_is_ignored`, `activity_during_close_quiet_is_ignored`. `tests/e2e_bridge.rs`의 시험은 `fn bridge_under_test() -> Arc<dyn TabBridge>`로 바꿔 `PEGBOARD_TEST_BROWSER`가 `fork:`면 도메인 판, 아니면 확장 판을 쓴다(작업 8에서 포크로 돈다).
- [ ] **Step 2: FAIL → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt, 맥미니 순정 모드 e2e 전체 PASS.**
- [ ] **Step 5: 커밋** `refactor: the tab bridge is a trait; the domain bridge speaks Pegboard.*`

---

### Task 6: 데몬의 세 구성, `daemon --cdp-pipe`, 남은 상태 정리, CLI

**Files:**
- Create: `crates/pegboard-core/src/settings.rs`
- Modify: `browser.rs`, `service.rs`, `session.rs`, `crates/pegboard-cli/src/main.rs`, `tests/common/mod.rs`, `README.md`(명령 설명)

**Interfaces:**
- Produces:

```rust
pub enum BrowserConfig {
    Stock { chrome: PathBuf, extension_dir: PathBuf },
    /// 시험·개발용(F13): 데몬이 포크를 --remote-debugging-pipe --pegboard-no-daemon 으로 띄운다.
    ForkLaunched { binary: PathBuf },
}
pub enum DaemonStart { Launch(BrowserConfig), Hosted(BrowserLink) }
impl Daemon { pub async fn start(paths: Paths, how: DaemonStart) -> anyhow::Result<Arc<Daemon>>; }
impl Browser {
    pub async fn launch(paths: &Paths, cfg: &BrowserConfig) -> Result<Browser>;   // Stock | ForkLaunched
    pub async fn hosted(paths: &Paths, link: BrowserLink) -> Result<Browser>;      // 부모가 브라우저
}
pub enum BrowserKind { Fork, Stock }
pub struct BrowserSettings { pub kind: BrowserKind }
impl BrowserSettings { pub fn load(paths: &Paths) -> Result<BrowserSettings, String>; }
// config.toml [browser] kind = "fork" | "stock". 없으면: /Applications/Pegboard.app 또는 ~/Applications/Pegboard.app 가 있으면 Fork, 아니면 Stock.
pub const EXIT_LOCK_HELD: i32 = 4;
```

동작:
- `PEGBOARD_TEST_BROWSER=fork:<Pegboard.app 경로>`(testenv)면 `common::env()`와 데몬이 `ForkLaunched`를 쓴다. 시험 플래그(`test_host_rules`, `test_address_spaces`)는 두 실행 모두에 붙인다. `ForkLaunched`는 자식의 fd 3, 4에 파이프를 두고 `BrowserLink::pipe`.
- 포크(`ForkLaunched`, `Hosted`)에서 데몬은 시작 직후 `Pegboard.claim` → `Pegboard.getProfiles`로 `AgentContexts`를 채우고 `profileLoaded/Unloaded`로 갱신한다. 확장과 `wait_for_first_context`를 쓰지 않는다. 창이 하나도 없어도 시작한다.
- `pegboard daemon --cdp-pipe`: fd 3, 4로 `BrowserLink::pipe` → `DaemonStart::Hosted`. 데몬 잠금을 못 잡으면 `EXIT_LOCK_HELD`로 끝난다. 물리 연결이 끝나면(EOF) 세션을 정리하고 0으로 끝난다.
- **남은 상태 정리(F11)**: `Hosted` 시작 때, `AgentContexts`의 모든 컨텍스트의 페이지 타깃을 닫는다(`Target.closeTarget`). 이전 데몬의 녹화는 2b의 고아 처리 그대로.
- `pegboard browser start`: `Fork`면 `open -b app.pegboard.browser`를 부르고 데몬이 응답할 때까지 기다린다(`START_TIMEOUT` 그대로). 앱이 없으면 오류 "Pegboard.app not found; set [browser] kind = \"stock\"". `Stock`은 지금 그대로.
- `pegboard browser stop`: `Fork`면 새 API `browser.quit`(데몬이 제어 연결로 `Browser.close`)을 부른다. 터미널에서 실행했고 `--yes`가 없으면 "This closes every Pegboard window, including yours. Continue? [y/N]"를 묻는다. 터미널이 아니고 `--yes`도 없으면 사용 오류(종료 코드 2). MCP에는 넣지 않는다.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn browser_kind_defaults_to_fork_when_the_app_is_installed()   // 임시 디렉터리를 Applications로 두는 testenv 변수 PEGBOARD_TEST_APPLICATIONS
#[test] fn browser_kind_reads_config()                                  // kind = "stock" → Stock; kind = "x" → Err
#[test] fn hosted_daemon_exits_4_when_the_lock_is_held()                // CLI 통합 시험: 잠금을 쥔 채 `pegboard daemon --cdp-pipe`(fd 3,4는 빈 파이프) → 종료 코드 4
#[test] fn stop_without_a_terminal_needs_yes()                         // Fork 설정, stdin이 터미널이 아님 → 종료 코드 2, browser.quit 호출 없음
#[tokio::test] #[ignore] async fn hosted_start_closes_leftover_agent_tabs()
// 포크 시험 구성(맥미니, 작업 8 이후 PASS 기대): 에이전트 프로필 탭 둘을 연 브라우저에 데몬을 Hosted로 다시 붙이면 그 탭들이 닫히고 사람 탭은 그대로.
```

- [ ] **Step 2: FAIL → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt, 맥미니 순정 모드 e2e 전체 PASS.**
- [ ] **Step 5: 커밋** `feat: the daemon runs hosted by the browser over a pipe; browser start/stop for the fork`

---

### Task 7: `browser/` 구조, 브랜딩, 첫 빌드 (맥미니)

**Files:**
- Create: `browser/CHROMIUM_VERSION`, `browser/args.gn`, `browser/patches/series`, `browser/patches/branding.patch`, `browser/branding/*`, `browser/pegboard/BUILD.gn`(빈 그룹), `browser/scripts/{fetch.sh,apply.sh,export.sh,build.sh,package-dev.sh,budget.sh}`, `browser/README.md`(사용법 한 쪽)

**Interfaces:**
- Produces:
  - `browser/scripts/fetch.sh <src-dir>`: depot_tools 확인, `<src-dir>`에 Chromium 받기(없으면) 또는 `CHROMIUM_VERSION` 태그로 맞추기, `gclient sync -D`. 0단계에서 본 요청 제한(HTTP 429)을 피하려 `--jobs 4`와 재시도.
  - `apply.sh <src-dir>`: 깨끗한 태그에서 `pegboard-work` 브랜치를 만들고, `browser/pegboard/`를 `<src>/pegboard/`로 복사해 커밋 하나, 그다음 `series` 순서로 `git apply --3way` → 패치마다 커밋 하나(제목 = 패치 파일 이름).
  - `export.sh <src-dir>`: 태그 이후 커밋을 순서대로 `browser/patches/<제목>`으로 다시 쓰고(`git format-patch --no-numbered --zero-commit --no-signature`), `pegboard/` 커밋은 `browser/pegboard/`로 되돌려 복사, `series` 갱신.
  - `build.sh <src-dir>`: `out/Pegboard`에 `args.gn` 복사, `gn gen`, `autoninja -C out/Pegboard chrome`. 걸린 시간을 출력.
  - `package-dev.sh <src-dir> <out.app>`: `cargo build --release -p pegboard-cli`, `Pegboard.app`을 복사하고 `pegboard`와 agent-browser(`scripts/fetch-agent-browser.sh` 결과)를 `Contents/Helpers/`에 넣고 `codesign --force --deep -s -`.
  - `budget.sh <src-dir>`: 태그 대비 `pegboard/` 밖 파일의 추가·삭제 줄 수 합계.
- `args.gn`: `is_debug=false`, `symbol_level=0`, `is_component_build=false`, `target_cpu="arm64"`, `chrome_pgo_phase=0`, `enable_nacl=false`, `google_api_key=""`, `google_default_client_id=""`, `google_default_client_secret=""`.
- 브랜딩: Chromium 브랜드 파일(`chrome/app/theme/chromium/BRANDING`의 이름, 번들 ID `app.pegboard.browser`), 사용자 데이터 디렉터리 이름 `Pegboard`(`chrome/common/chrome_paths_mac.mm`의 Chromium 쪽 경로), 아이콘은 임시로 단색 사각형에 "P"(3b에서 다시).

- [ ] **Step 1: 맥미니 디스크 여유를 확인한다**(`df -h`). 100GB 미만이면 멈추고 보고.
- [ ] **Step 2: 스크립트를 쓰고 `shellcheck browser/scripts/*.sh` 통과.**
- [ ] **Step 3: `fetch.sh` → `apply.sh` → `build.sh`. 빈 상태부터 걸린 시간을 잰다.** 기대: `out/Pegboard/Chromium.app`(또는 브랜드 이름의 앱)이 생긴다.
- [ ] **Step 4: 확인.** 앱을 `--user-data-dir=<임시>`로 켜서 창 제목·메뉴 이름이 `Pegboard`, `defaults read <app>/Contents/Info CFBundleIdentifier`가 `app.pegboard.browser`, 기본 사용자 데이터가 `~/Library/Application Support/Pegboard`(임시 디렉터리 없이 한 번 켜서 생기는지 보고 지운다).
- [ ] **Step 5: 업스트림 파이프 확인.** 작업 1의 `e2e_link.rs`를 포크 바이너리에 `--remote-debugging-pipe`로 돌릴 수 있게 `RawLink`에 파이프 판을 더하고, 같은 시험이 PASS하는지 본다(포크에서도 F7의 전제가 맞는지).
- [ ] **Step 6: `export.sh`로 패치를 뽑고, 깨끗한 태그에 `apply.sh`가 다시 적용되는지 확인. `budget.sh` 값을 기록.**
- [ ] **Step 7: 커밋** `build: browser/ tree, branding patch, scripts; first fork build`. 빌드 시간·디스크는 `docs/spike-findings.md` "3a 작업 7"에.

---

### Task 8: `Pegboard` CDP 도메인 (C++, 맥미니)

**Files:**
- Create: `browser/pegboard/devtools/{pegboard_protocol.pdl,pegboard_handler.h,pegboard_handler.cc,BUILD.gn}`, `browser/patches/pegboard-cdp-domain.patch`, `crates/pegboard-core/tests/e2e_domain.rs`
- Modify: `browser/patches/series`

**Interfaces:**
- Consumes: 작업 5의 표(메서드·매개변수·응답 모양 그대로), 작업 6의 `Pegboard.claim` 흐름
- Produces: 위 표의 도메인. 업스트림 연결: `chrome/browser/devtools/BUILD.gn`(`cros_protocol.pdl`을 덧붙이는 `concatenate_protocols` 방식으로 `//pegboard/devtools/pegboard_protocol.pdl` 추가), `inspector_protocol_config.json`(도메인 항목), `chrome_devtools_session.cc`(핸들러 등록, `IsDomainAvailableToUntrustedClient`가 false).

규칙:
- `claim`: 이 클라이언트(파이프)의 세션 중 아직 주인이 없으면 이 세션이 주인. 주인이 있으면 `-32000 "already claimed"`. 주인 세션이 떨어지면 주인 없음으로 돌아간다. 주인이 아닌 세션에서 `claim` 외의 `Pegboard.*`는 `-32601`.
- 모든 메서드는 `browserContextId`가 에이전트 프로필(디렉터리 이름 규칙, Global Constraints) 것이 아니면, 또는 `targetId`·`groupId`가 에이전트 프로필의 탭·그룹이 아니면 `-32000`으로 거부한다.
- `openProfileWindow`는 에이전트 프로필 디렉터리 이름만 받고, 없으면 `ProfileManager`로 만들어 불러온 뒤 창을 연다. 첫 실행 화면을 띄우지 않는다.
- `tabActivated`, `profileLoaded/Unloaded`는 주인 세션에만 보낸다.

- [ ] **Step 1: 시험을 쓴다** (`e2e_domain.rs`, `#[ignore]`, `PEGBOARD_TEST_BROWSER=fork:...`일 때만 돌고 아니면 건너뛴다).

```rust
#[tokio::test] #[ignore] async fn domain_is_only_on_the_claiming_session()
// 가상 연결 V1 claim → OK; V2 claim → already claimed; V2의 Pegboard.getProfiles → -32601. V1을 닫으면 V2 claim → OK.
#[tokio::test] #[ignore] async fn domain_refuses_non_agent_contexts()
// 사람 프로필 Default와 사람 프로필 하나 더(Profile 1)의 컨텍스트로 createGroup/createTab → 거부. 사람 탭 targetId로 addToGroup/closeTargets → 거부, 탭은 그대로.
#[tokio::test] #[ignore] async fn open_profile_window_creates_and_reports_context()
// 없는 PegboardAgent-t1 → 컨텍스트가 돌아오고 getProfiles에 isAgent:true로 보인다. "Profile 1" → 거부.
#[tokio::test] #[ignore] async fn profile_events_follow_unload_and_reload()
// 에이전트 프로필 창을 모두 닫으면 profileUnloaded, 다시 열면 profileLoaded(새 컨텍스트 ID).
#[tokio::test] #[ignore] async fn tab_activated_reports_switches()
#[tokio::test] #[ignore] async fn extension_debugger_cannot_see_domain()
// 시험용 확장(spikes/ext-activity 같은 최소 확장, chrome.debugger)을 사람 프로필에 --load-extension으로 올려 탭에 붙고 Pegboard.getProfiles → 없는 메서드.
#[tokio::test] #[ignore] async fn proxy_refuses_pegboard_methods()
// 세션 프록시 클라이언트로 Pegboard.claim, Pegboard.createTab → 프록시가 거부(기존 허용목록).
```

- [ ] **Step 2: 컴파일 확인(`cargo test --no-run`).**
- [ ] **Step 3: 맥미니 트리에서 도메인을 구현하고 빌드한다.** 첫 헤더 수정 뒤 증분 빌드 시간을 잰다(A10의 남은 항목).
- [ ] **Step 4: 맥미니에서 `e2e_domain` 전체, 그리고 포크 구성으로 `e2e_bridge`, `e2e_session`, `e2e_proxy`, 작업 6의 `hosted_start_closes_leftover_agent_tabs`.** 기대: PASS.
- [ ] **Step 5: `export.sh`, `budget.sh` 기록, 커밋** `feat(browser): Pegboard CDP domain for tab groups and agent profiles`

---

### Task 9: 파이프 fd와 부트스트랩 (C++, 맥미니)

**Files:**
- Create: `browser/pegboard/bootstrap/{daemon_launcher.h,daemon_launcher.cc,BUILD.gn}`, `browser/patches/devtools-pipe-fds.patch`, `browser/patches/pegboard-bootstrap.patch`, `crates/pegboard-core/tests/e2e_bootstrap.rs`

**Interfaces:**
- Produces:
  - `devtools-pipe-fds.patch`: `content::DevToolsAgentHost`에 공개 함수 하나 — `StartRemoteDebuggingPipeHandlerWithFds(int read_fd, int write_fd, base::OnceClosure on_disconnect)`(이름은 업스트림 관례에 맞게 다듬어도 된다), 끝난 뒤 다시 부를 수 있다. 기존 `--remote-debugging-pipe`(fd 3, 4)는 그대로.
  - `pegboard-bootstrap.patch`: 브라우저 시작 뒤 한 곳에서 `pegboard::DaemonLauncher::Start()`를 부르는 연결.
  - `DaemonLauncher` 동작(설계 4.2): 건너뛰는 조건(`--remote-debugging-pipe`, `--pegboard-no-daemon`, 요청만 넘기고 끝나는 프로세스), 파이프 두 쌍, `Contents/Helpers/pegboard daemon --cdp-pipe`를 자식 fd 3(읽기)·4(쓰기)로 실행, 데몬 종료 감지, F8 재시작(1분 3번, 종료 코드 4면 안 함), 브라우저 종료 때 파이프를 닫고 5초 뒤 `SIGTERM`. 데몬의 stdout/stderr는 `<PEGBOARD_HOME>/daemon.log`로(데몬이 직접 연다).

- [ ] **Step 1: 시험을 쓴다** (`e2e_bootstrap.rs`, `#[ignore]`, `PEGBOARD_TEST_APP=<Pegboard.app>`, 임시 `--user-data-dir`과 `PEGBOARD_HOME`).

```rust
#[tokio::test] #[ignore] async fn app_start_spawns_the_daemon()
// open -n -a <app> --args --user-data-dir=<tmp> → 10초 안에 데몬 소켓 응답, `pegboard session create`와 `open https://example.test` 성공.
#[tokio::test] #[ignore] async fn no_tcp_listener()
// 위 상태에서 브라우저 메인 프로세스와 그 자식 전부에 `lsof -nP -iTCP -sTCP:LISTEN -a -p <pid>` 결과 없음.
#[tokio::test] #[ignore] async fn starts_without_any_window()
// --no-startup-window 로 켜도 데몬이 뜨고 세션을 만들 수 있다.
#[tokio::test] #[ignore] async fn daemon_exits_when_the_browser_quits()
// 세션이 있는 채 브라우저 종료 → 데몬 프로세스가 10초 안에 사라지고, 세션 녹화의 ended_at이 채워져 있다.
#[tokio::test] #[ignore] async fn a_killed_daemon_is_restarted_and_cleans_up()
// 세션 탭이 있는 채 데몬 SIGKILL → 새 데몬이 뜨고, 남은 에이전트 탭이 닫히고, 사람 탭은 그대로, 브라우저는 살아 있다.
#[tokio::test] #[ignore] async fn restarts_stop_after_three_in_a_minute()
// 1분 안에 4번 죽이면 4번째 뒤에는 데몬이 없고 브라우저는 살아 있다.
#[tokio::test] #[ignore] async fn a_held_lock_is_not_retried()
// 다른 PEGBOARD_HOME 잠금을 쥔 순정 데몬이 도는 상태에서 앱 시작 → 데몬은 종료 코드 4로 한 번만 실행(daemon.log 또는 PEGBOARD_TEST_LAUNCH_LOG 줄 수 1), 기존 데몬은 계속 응답.
```

- [ ] **Step 2: 컴파일 확인.** 
- [ ] **Step 3: 맥미니 트리에서 구현·빌드, `package-dev.sh`로 앱을 만든다.**
- [ ] **Step 4: `e2e_bootstrap` 전체 PASS.** 파이프 핸들러를 다시 시작할 수 없는 것으로 드러나면(설계 5절 위험 3) 멈추고 컨트롤러가 사용자에게 F8을 낮출지 묻는다.
- [ ] **Step 5: A6 확인: `budget.sh`로 `devtools-pipe-fds.patch`의 업스트림 줄 수를 적는다. 커밋** `feat(browser): the browser starts pegboardd and hands it the DevTools pipe`

---

### Task 10: 포크 모드 전체 확인, 측정, 문서

**Files:**
- Modify: `docs/spike-findings.md`, `docs/architecture.md`(결정 3의 "다중화 미검증", 5절의 [미확인] 중 조사·실측으로 확인한 것, 예상 패치 목록의 크기), `docs/roadmap.md`(3단계 할 일 1~4 완료 표시, 진행 현황), 설계 문서(만든 그대로 고치고 "처음 제안은 …"), `README.md`(포크 모드 사용법)

- [ ] **Step 1: 맥미니에서 포크 구성으로 e2e 전체**(`PEGBOARD_TEST_BROWSER=fork:<app> cargo test -p pegboard-core -- --ignored --test-threads=1`)와 이 기기 순정 구성 전체. 기대: 모두 PASS. 녹화 지연 중앙값 60ms 이하(포크·파이프 위에서).
- [ ] **Step 2: 기록한다.** 빈 상태 빌드 시간, 증분(한 줄, 헤더), 디스크, 업스트림 수정 줄 수 기준선(패치별), 파이프 위 녹화 지연.
- [ ] **Step 3: 문서를 고치고 커밋** `docs: phase 3a done (fork skeleton), measurements`

---

## 작업 순서와 의존

1 → 2 → 3 → 4 → 5 → 6 은 이 기기(Rust)와 맥미니 순정 시험으로 진행한다. 작업 1이 기대와 다르면 2로 가지 않는다.
7은 1 이후 언제든 맥미니에서 병행할 수 있다(빌드가 몇 시간 걸리므로 일찍 시작하는 것이 좋다).
8은 5, 6, 7 뒤. 9는 6, 8 뒤. 10은 마지막.

## 설계와의 차이, 이 계획이 정한 것

- 데몬의 "다른 데몬이 있음" 종료 코드를 **4**로 정했다(F8의 재시도 제외 조건을 브라우저가 알 방법).
- 순정 모드의 `ws_url`은 `Option`으로 남기고 시험용 `CdpConn::connect`도 남긴다(작업 1의 원시 시험과 기존 단위 시험이 쓴다).
- 브리지의 `create_group`이 빈 탭 하나를 만드는 지금 동작을 도메인도 따른다(그룹은 탭 없이 존재할 수 없다).
- `browser stop`의 확인 문구와 `--yes`, `browser.quit` API는 이 계획이 정했다.
- 아이콘은 임시. 3b에서 다시 만든다.
