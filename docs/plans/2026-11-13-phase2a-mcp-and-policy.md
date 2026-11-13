# 2a단계: MCP와 세션 정책 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 셸 없는 에이전트가 `pegboard mcp`로 세션을 만들어 브라우저를 조종하고, 그 세션이 사람이 정한 정책(동작 분류, 갈 수 있는 주소, 다운로드, 업로드, 클립보드) 안에서만 움직이게 한다.

**Architecture:** 세션마다 `SessionPolicy`가 붙고, `pegboardd`가 명령을 실행하기 전에 판정한다(`daemon::run_command` 안, `argv::check` 다음). 주소 규칙과 다운로드, 클립보드는 `pegboardd`가 자기 CDP 연결로 강제한다. `pegboard mcp`는 같은 바이너리의 stdio 서버로, 도구 호출을 CLI와 같은 argv로 바꿔 코어 API `command.exec`에 보낸다. 도구는 허용 명령 문법(`argv.rs`의 `GRAMMAR`)에서 생성한다.

**Tech Stack:** Rust(기존 워크스페이스: `pegboard-protocol`, `pegboard-core`, `pegboard-cli`), tokio, serde_json, clap. 브리지 확장(`extension/bridge/sw.js`). Chrome for Testing 157, agent-browser 0.42.0(동봉).

**Spec:** [2026-11-13-phase2a-mcp-and-policy-design.md](./2026-11-13-phase2a-mcp-and-policy-design.md). 앞 문서: [architecture.md](../architecture.md), [phase1-handoff/ledger.md](./phase1-handoff/ledger.md)(1단계의 판정. `Ruling:` 줄은 지금도 유효하다).

## Global Constraints

- 실패로 볼 것(설계 2절)이 하나라도 생기면 그 작업은 끝난 것이 아니다. 특히: MCP로 들어온 명령은 `daemon::run_command` 하나를 거친다. 세션 토큰과 프록시 토큰은 도구 결과, 오류 문구, 동작 기록, `daemon.log`에 나오지 않는다.
- 1단계의 보장을 약하게 만들지 않는다: `argv::check`의 허용 문법, 동작 기록의 가림 규칙(`actionlog.rs`, 1단계 ledger의 R1~R7, S1~S7), 프록시 필터의 기본 거부, `Target.createTarget`을 넘기지 않는 것, 사람 프로필에 정책을 걸지 않는 것.
- 애매하면 닫는 쪽으로 만든다. 정책을 걸 수 없으면 세션을 만들지 않거나 명령을 받지 않는다.
- 설계에 "제안"으로 적힌 값은 그대로 쓴다: 다운로드 한도 파일 200 MiB와 세션 합계 1 GiB, MCP 서버 하나의 세션 수 16, 유휴 만료 1시간, 도구 묶음 기본 `core,tabs`.
- **실제 브라우저를 띄우는 것은 구현 에이전트가 하지 않는다.** `#[ignore]` 시험과 `spikes/`의 실험은 컨트롤러가 두 번째 기기에서 돌린다. 구현 에이전트는 코드, 브라우저 없는 시험(`cargo test --workspace`), `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`까지 하고, 돌려야 할 실제 브라우저 시험의 이름과 기대 결과를 보고서에 적는다. 컨트롤러가 결과(통과, 실패 출력)를 돌려준다.
- 실제 브라우저 시험은 임시 `PEGBOARD_HOME`만 쓰고, 사용자의 `~/Downloads`, 실제 클립보드 내용, 실행 중인 다른 브라우저를 건드리지 않는다. 클립보드를 다루는 시험은 시작할 때 내용을 저장하고 끝날 때 되돌린다.
- 문서와 코드에 홈 디렉터리 경로, 사용자 이름, 기기 이름을 쓰지 않는다.
- 커밋 메시지는 영어, 본문 끝에 `Co-Authored-By: <구현한 모델 이름> <noreply@anthropic.com>`. 푸시와 main 병합은 컨트롤러가 한다.
- 문서(README, docs/)는 한국어로 쓴다.

## Review Focus

설계가 암시하지만 어느 작업의 시험도 건드리지 않기 쉬운 것 다섯 가지. 각 줄의 시험은 괄호 안 작업에 넣었다.

1. **정책이 다른 두 세션이 같은 프로필을 쓴다.** 좁은 세션이 넓은 세션 덕에 권한을 얻으면 안 된다(작업 3 `second_session_cannot_widen_a_pinned_profile`, 작업 7 `clipboard_setting_of_one_session_does_not_leak_to_another`).
2. **주소 규칙을 우회하는 표기.** `http://2130706433/`, `http://0x7f.1/`, `http://[::ffff:10.0.0.1]/`, 끝에 점이 붙은 `localhost.`, 대문자, 사설 주소로 풀리는 공개 이름, 리다이렉트로 사설 주소에 닿는 경우(작업 4 `address_table`, `redirect_to_private_address_is_blocked`).
3. **MCP 도구 인자가 문법 밖의 argv를 만든다.** 문자열 인자에 `--cdp`, 배열 자리에 중첩 배열, 숫자 자리에 문자열, 알 수 없는 키, `sessionId`에 남의 ID나 핸들 꼴(작업 10 `tool_arguments_cannot_build_argv_outside_the_grammar`).
4. **MCP 서버가 갑자기 죽는다**(stdin이 닫히지 않은 채 SIGKILL). 세션이 남는다. 유휴 만료가 치워야 한다(작업 8 `idle_session_is_closed_after_the_expiry`, 작업 9 `sessions_are_closed_when_stdin_closes`).
5. **다운로드가 누구 것인지 모른다.** 사람이 에이전트 창에서 직접 누른 다운로드, 닫힌 탭의 다운로드, 두 세션이 동시에 같은 이름의 파일을 받는 경우(작업 5 `unattributed_download_is_deleted`, `two_sessions_downloading_the_same_name_keep_their_own_file`).

## 파일 구조

| 파일 | 책임 | 작업 |
|---|---|---|
| `crates/pegboard-protocol/src/lib.rs` | `ActionClass`, `SessionPolicy`, 새 오류 코드, `SessionCreateParams.policy`, `SessionInfo.policy` | 3 |
| `crates/pegboard-core/src/argv.rs` | 각 형식의 `class`, 인자 이름, `class_of`, `forms()`(MCP용 공개 설명) | 3, 10 |
| `crates/pegboard-core/src/policy.rs` (새) | 동작 분류 판정, 프로필 단위 고정(`ProfilePins`) | 3 |
| `crates/pegboard-core/src/address.rs` (새) | 주소 판정(순수 함수) | 4 |
| `crates/pegboard-core/src/guard.rs` (새) | 세션 타깃에 요청 가로채기를 걸어 `address`로 판정 | 4 |
| `crates/pegboard-core/src/downloads.rs` (새) | 프로필별 임시 폴더, 세션 귀속, 한도, 정책 연결 재적용 | 5 |
| `crates/pegboard-core/src/browser.rs` | 1단계의 `deny_downloads`를 `downloads.rs`로 옮긴다 | 5 |
| `crates/pegboard-core/src/daemon.rs` | 정책 판정 호출 위치, `policy_denied`의 기록 | 3, 6, 7 |
| `crates/pegboard-core/src/session.rs` | 정책 보관, 닫을 때 자기 탭만, 유휴 만료 | 3, 8 |
| `crates/pegboard-core/src/service.rs`, `api.rs` | `session.create`의 정책, `files.list`, 입력 상한, `Client`의 응답 id 확인 | 3, 5, 8 |
| `crates/pegboard-core/src/proxy/filter.rs`, `server.rs` | 범위 밖이 된 타깃의 소멸 이벤트 전달, 가로채기를 끌 수 없게 | 4, 8 |
| `crates/pegboard-cli/src/main.rs` | `session create`의 정책 옵션, `files list`, `mcp` 하위 명령 | 3, 5, 9 |
| `crates/pegboard-cli/src/mcp/mod.rs` (새) | stdio 루프, 모드, 소유 세션 | 9 |
| `crates/pegboard-cli/src/mcp/tools.rs` (새) | 관리 도구와, 문법에서 생성한 동작 도구 | 9, 10 |
| `crates/pegboard-core/tests/common/mod.rs`, `crates/pegboard-cli/tests/common/mod.rs` | 시험 파일 하나가 브라우저 하나를 같이 쓰는 하네스 | 1 |
| `crates/pegboard-cli/tests/e2e_mcp.rs`, `e2e_mcp_isolation.rs` (새) | MCP 시험 | 9, 10, 11 |
| `crates/pegboard-cli/tests/e2e_policy.rs` (새) | 정책 시험(CLI) | 3~7 |
| `spikes/20-*.mjs` (새) | 작업 2의 실험 | 2 |

---

### Task 1: 시험이 브라우저를 덜 띄우게 한다

지금은 실제 브라우저 시험 하나마다 Chrome을 새로 띄운다(전체 한 번에 106번 이상). 격리 시험 묶음(`e2e_isolation.rs`, `e2e_isolated_world.rs`)과 수명주기 시험(브라우저를 끄거나 죽이는 시험)은 그대로 두고, 나머지는 시험 파일(시험 바이너리) 하나가 브라우저 하나를 같이 쓴다.

**Files:**
- Modify: `crates/pegboard-core/tests/common/mod.rs`, `crates/pegboard-cli/tests/common/mod.rs`
- Modify: `crates/pegboard-core/tests/e2e_runner.rs`, `e2e_actionlog.rs`, `e2e_proxy.rs`, `e2e_session.rs`, `e2e_bridge.rs`, `crates/pegboard-cli/tests/e2e_cli.rs`
- Modify: `crates/pegboard-core/src/browser.rs` (시험용 실행 횟수 기록 한 줄)

**Interfaces:**
- Produces (core 시험): `common::shared() -> &'static Shared`. `Shared`는 브라우저 하나, 프록시, `SessionManager`, `Runner`, 임시 홈을 쥔다. 시험은 `shared().session(path).await`로 자기 세션을 만들고 끝에 닫는다. 시험 바이너리가 끝날 때 브라우저를 끈다(프로세스 종료 훅. `--test-threads=1`을 전제로 한다).
- Produces (CLI 시험): `common::shared_home() -> &'static Home`. 한 번만 `browser start`를 하고, 바이너리가 끝날 때 `browser stop`.
- Produces: 환경 변수 `PEGBOARD_TEST_LAUNCH_LOG=<파일>`이 있으면 `Browser::launch`가 실행할 때마다 그 파일에 한 줄을 덧붙인다(시험 전용. 없으면 아무것도 하지 않는다).
- 같이 쓰면 안 되는 시험은 지금처럼 자기 브라우저를 띄운다. 어떤 시험이 그런지와 이유를 각 파일 머리의 주석 한 줄에 적는다: 브라우저를 끄거나 죽이는 시험, 사람 프로필에 쿠키를 심는 시험, 프로필을 만들고 지우는 시험 가운데 다른 시험의 프로필 목록 단언을 깨는 것.

- [ ] **Step 1: 기준을 잰다.** `PEGBOARD_TEST_LAUNCH_LOG`를 `browser.rs`에 넣고, 컨트롤러가 두 번째 기기에서 `cargo test --workspace -- --ignored --test-threads=1`을 돌려 파일별 실행 횟수와 소요 시간을 받는다. 보고서에 표로 적는다.
- [ ] **Step 2: 공유 하네스를 만들고 한 파일(`e2e_actionlog.rs`)만 옮긴다.** 시험끼리 상태가 새지 않게 한다: 시험마다 새 세션, 새 탭그룹, 끝에 세션 닫기. 쿠키와 저장소가 필요한 시험은 시험마다 다른 경로나 다른 프로필 이름을 쓴다.
- [ ] **Step 3: 옮긴 시험이 여전히 실패할 수 있음을 보인다.** 그 파일의 시험 세 개를 골라, 지키는 코드를 잠깐 망가뜨렸을 때 실패하는 것을 확인하고 되돌린다(어떤 줄을 어떻게 바꿨는지, 실패 출력). 컨트롤러가 돌린다.
- [ ] **Step 4: 나머지 파일을 옮긴다.** 파일마다 커밋 하나.
- [ ] **Step 5: 다시 잰다.** 기대: 전체 실행 횟수가 Step 1의 절반 이하. 못 미치면 어느 시험이 왜 자기 브라우저를 띄우는지 표로 적는다. 숫자를 맞추려고 시험을 합치거나 지우지 않는다.
- [ ] **Step 6: 시험 순서 의존을 확인한다.** 컨트롤러가 각 파일을 시험 이름 역순으로 한 번 더 돌린다(시험 필터로 하나씩 역순 실행하는 스크립트를 `scripts/test-reverse.sh`로 둔다). 실패하면 상태가 새는 것이다.
- [ ] **Step 7: 커밋.** `test: share one browser per test binary for non-isolation suites`

---

### Task 2: Chrome이 무엇을 허용하는지 잰다

작업 4, 5, 7의 방법은 Chrome의 실제 동작에 달려 있다. 0단계처럼 `spikes/`에 작은 Node 스크립트를 두고(`spikes/README`와 기존 `spikes/04-profiles.mjs`의 꼴을 따른다. 임시 user-data-dir, 프로필 두 개, 끝나면 브라우저 종료), 컨트롤러가 두 번째 기기에서 돌린다. 제품 코드는 바꾸지 않는다.

**Files:**
- Create: `spikes/20-request-guard.mjs`, `spikes/21-downloads.mjs`, `spikes/22-clipboard.mjs`
- Modify: `docs/spike-findings.md` (새 절 "2a단계 측정")

각 스크립트는 질문마다 `PASS`/`FAIL`/`관측값`을 한 줄씩 출력한다.

- [ ] **Step 1: `20-request-guard.mjs`.** 브라우저 수준 연결 하나(pegboardd 역)가 에이전트 프로필의 탭에 붙어 `Fetch.enable`로 요청을 가로챈다. 다른 연결(에이전트 역)이 같은 탭에 붙는다. 잴 것:
  - G1. 가로채기가 잡는 요청의 종류: 주 프레임 탐색, iframe, 이미지, 페이지 스크립트의 `fetch`, `Runtime.evaluate`로 낸 `fetch`, `XMLHttpRequest`, 웹소켓 핸드셰이크, `navigator.sendBeacon`, 전용 워커, 서비스 워커, 공유 워커가 낸 요청. 잡히지 않는 것의 목록.
  - G2. 요청 단계(`requestPaused`)에서 보이는 것: URL, 리다이렉트된 URL, 원격 주소가 있는지. 응답 단계(`requestStage: Response`)에서 원격 IP를 알 수 있는지(`Network.responseReceived.remoteIPAddress`와의 순서 포함). 본문이 페이지에 전달되기 전에 끊을 수 있는지.
  - G3. 공개 이름이 사설 주소로 풀릴 때(스크립트가 띄운 로컬 DNS 없이 재려면 `--host-resolver-rules`로 `test.example`을 `10.255.255.1`과 루프백에 매핑) 어느 단계에서 주소가 보이는지.
  - G4. 에이전트 역 연결이 `Fetch.disable`, `Fetch.enable`(자기 패턴), `Network.setBypassServiceWorker`, `Target.setAutoAttach`로 pegboardd 역의 가로채기를 끄거나 가릴 수 있는지. 두 연결이 모두 `Fetch.enable`을 했을 때 누가 먼저 판정하는지.
  - G5. 가로채기를 건 연결이 닫히면 가로채기가 풀리는지. 탭마다 걸어야 하는지, 새 탭과 팝업에 자동으로 걸 방법이 있는지(`Target.setAutoAttach`의 `waitForDebuggerOnStart`로 첫 요청 전에 걸 수 있는지).
  - G6. 가로채기가 요청 하나에 더하는 지연(100개 평균).
- [ ] **Step 2: `21-downloads.mjs`.** 잴 것:
  - L1. `Browser.setDownloadBehavior {behavior:"allowAndName", downloadPath, eventsEnabled:true, browserContextId}`가 프로필별로 다른 폴더를 가질 수 있는지(1단계 측정: 한 연결은 컨텍스트 하나의 정책만 쥔다. 프로필마다 연결 하나로 되는지).
  - L2. `Browser.downloadWillBegin`의 `frameId`로 탭(타깃)을 찾을 수 있는지: 링크 클릭, `window.open`으로 연 탭, 새 탭이 곧 닫히는 다운로드, iframe 안의 링크, 서비스 워커가 시작한 다운로드, 사람이 그 창에서 직접 누른 다운로드.
  - L3. 진행 중 취소(`Browser.cancelDownload`)가 되는지, `downloadProgress`의 `totalBytes`와 `receivedBytes`로 한도를 넘는 순간을 알 수 있는지, 취소 뒤 부분 파일이 남는지.
  - L4. 저장되는 파일 이름이 무엇인지(식별자), 원래 이름은 어디서 얻는지.
  - L5. 정책 연결이 닫혔을 때 진행 중이던 다운로드가 어디로 가는지.
- [ ] **Step 3: `22-clipboard.mjs`.** 시작할 때 시스템 클립보드 내용을 저장하고 끝날 때 되돌린다(`pbpaste`, `pbcopy`). 잴 것:
  - C1. 권한을 주지 않은 에이전트 프로필 탭에서 `navigator.clipboard.writeText`와 `readText`가 `Runtime.evaluate`(사용자 제스처 있음/없음)로 되는지.
  - C2. `Browser.setPermission`/`Browser.grantPermissions`/`resetPermissions`로 `clipboardReadWrite`, `clipboardSanitizedWrite`를 거부할 수 있는지, 그 설정이 프로필(컨텍스트) 단위인지 출처 단위인지 탭 단위인지.
  - C3. `document.execCommand('copy')`와 `'paste'`가 되는지, 막을 방법이 있는지.
  - C4. CDP 키 입력(`Input.dispatchKeyEvent`의 Meta+C, Meta+V, Meta+X와 `commands: ["copy"]`)이 시스템 클립보드를 바꾸는지.
  - C5. 페이지가 `copy` 이벤트 핸들러로 클립보드 내용을 바꿀 수 있는지.
- [ ] **Step 4: 결과를 `docs/spike-findings.md`에 적는다.** 질문마다 관측값, 그리고 작업 4, 5, 7이 쓸 방법을 한 줄씩. 프로필 단위로만 강제되는 항목의 목록(설계 D8)을 여기서 확정한다.
- [ ] **Step 5: 멈춤 조건을 판정한다.** 아래 가운데 하나라도 해당하면 컨트롤러가 사용자에게 알리고 그 작업은 시작하지 않는다.
  - G1에서 `Runtime.evaluate`로 낸 요청이나 페이지 스크립트의 `fetch`가 잡히지 않는다.
  - G4에서 에이전트 쪽이 가로채기를 끄거나 가릴 수 있고, 프록시 필터로 그 메서드를 막아도 막히지 않는다.
  - L2에서 링크 클릭 다운로드의 탭을 찾을 수 없다.
  - C1~C4에서 막을 방법이 없는 쓰기 경로가 있다(읽기만 막히는 경우 포함).
- [ ] **Step 6: 커밋.** `spike: request interception, download attribution and clipboard control on Chrome 157`

---

### Task 3: 세션 정책과 동작 분류

**Files:**
- Modify: `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-core/src/argv.rs`, `daemon.rs`, `session.rs`, `service.rs`, `crates/pegboard-cli/src/main.rs`
- Create: `crates/pegboard-core/src/policy.rs`
- Test: `policy.rs`와 `argv.rs`의 단위 시험, `crates/pegboard-cli/tests/e2e_policy.rs`

**Interfaces:**
- Produces (`pegboard-protocol`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionClass {
    Navigate, Click, Fill, Snapshot, Scroll, Wait, Read, Get, Interact,
    Eval, Download, Upload, State, Clipboard,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionPolicy {
    pub allow: Vec<ActionClass>,          // opens a class that is denied by default
    pub deny: Vec<ActionClass>,           // closes a class that is allowed by default
    pub allow_domains: Vec<String>,       // empty = no allow-list
    pub allow_private_network: bool,
    pub no_localhost: bool,
    pub upload_dir: Option<String>,       // absolute path, checked at session.create
    pub no_expire: bool,
}

pub mod codes {
    pub const POLICY_DENIED: &str = "policy_denied";
    pub const PROFILE_POLICY_CONFLICT: &str = "profile_policy_conflict";
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
}
```

  `SessionCreateParams`에 `#[serde(default)] pub policy: SessionPolicy`, `SessionInfo`에 `#[serde(default)] pub policy: SessionPolicy`.
- Produces (`argv.rs`): `Form`에 `class: ActionClass` 필드(모든 형식에 붙인다. `form(...)` 생성자가 분류를 인자로 받게 해서, 분류 없는 형식은 컴파일되지 않는다). `pub fn class_of(argv: &[String]) -> Option<ActionClass>`(문법에 맞는 argv의 분류. 맞지 않으면 `None`).
- Produces (`policy.rs`):

```rust
// Free functions: SessionPolicy itself lives in the protocol crate.
pub fn default_allowed(class: ActionClass) -> bool;   // Upload, State, Clipboard => false; the rest true
pub fn validate(policy: &SessionPolicy) -> Result<(), RpcError>;  // invalid_params
pub fn judge_action(policy: &SessionPolicy, class: ActionClass) -> Result<(), RpcError>; // policy_denied

/// Settings Chrome enforces per profile only (the list is fixed by Task 2's findings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePins { /* one field per profile-level item, e.g. clipboard: bool */ }
pub fn pins_of(policy: &SessionPolicy) -> ProfilePins;
pub struct PinTable;   // profile name -> (ProfilePins, live session count)
impl PinTable {
    pub fn acquire(&self, profile: &str, pins: ProfilePins) -> Result<PinGuard, RpcError>; // profile_policy_conflict
}
// Dropping the PinGuard releases the session's hold; the pin clears when the count reaches 0.
```

- 분류 매핑(문법의 첫 단어 기준. 하위 명령이 다르면 따로 적는다):

| 분류 | 명령 |
|---|---|
| `navigate` | `open`, `back`, `forward`, `reload`, `tab new`, `tab`(전환), `tab close`, `frame` |
| `click` | `click`, `dblclick`, `check`, `uncheck`, `select`, `drag`, `mouse *` |
| `fill` | `fill`, `type`, `press`, `keyboard *` |
| `snapshot` | `snapshot`, `screenshot`, `pdf`, `a11y`, `highlight` |
| `scroll` | `scrollintoview`, `mouse wheel` |
| `wait` | `wait` (`wait --fn`은 `eval`) |
| `read` | `console`, `errors`, `network requests`, `vitals`, `tab list`, `dialog status`, `cookies`(읽기), `cookies get`, `storage * `(읽기), `storage * get` |
| `get` | `get *`, `is *` |
| `interact` | `hover`, `focus`, `dialog accept`, `dialog dismiss` |
| `eval` | `eval`, `wait --fn` |
| `state` | `cookies set`, `storage local set` 꼴(값을 쓰는 형식), `storage * clear` |

  `download`, `upload`, `clipboard`는 이 작업에서 분류만 정의한다(명령은 작업 5~7).
- 판정 위치: `daemon::run_command`에서 `argv::check`가 통과한 다음, `runner.exec` 전에 `policy::judge_action`. 거부되면 `policy_denied`를 돌려주고, 동작 기록에는 1단계의 거부 규칙대로 남긴다(`command`는 명령 이름, 인자는 `REDACTED`, `redacted: true`, `error`는 코드만).
- `policy_denied`의 문구(그대로): `the session's policy does not allow <class> commands; a person can allow it when creating the session with --allow <class>`. `eval`처럼 `--deny`로 닫힌 경우: `the session's policy denies <class> commands (--deny <class>)`.
- CLI: `pegboard session create`에 `--allow <c,...>`, `--deny <c,...>`, `--allow-domain <d,...>`, `--allow-private-network`, `--no-localhost`, `--upload-dir <dir>`, `--no-expire`. 종료 코드: `policy_denied`는 1, `profile_policy_conflict`와 잘못된 옵션은 2. `session info`는 정책을 보여 준다.
- `validate`: 같은 분류가 `allow`와 `deny`에 둘 다 있으면 거절. `allow`에 기본 허용 분류, `deny`에 기본 거부 분류가 있으면 거절(뜻이 없는 옵션이라 오타일 가능성이 크다). `allow_domains`의 항목은 호스트 이름이나 IP 한 개씩(스킴, 경로, 포트, 와일드카드 없음. 소문자로 맞춘다). `upload_dir`는 절대 경로이고 실제로 있는 디렉터리여야 한다. `upload_dir`가 있는데 `allow`에 `upload`가 없거나 그 반대면 거절.

- [ ] **Step 1: 실패하는 단위 시험을 쓴다** (`policy.rs`, `argv.rs`).

```rust
#[test] fn every_form_has_a_class_and_the_table_matches() {
    // for each (argv, class) row of the table above: class_of(argv) == Some(class)
    // cookies set, storage local set k v, storage session clear => State
    // cookies, cookies get, storage local, storage local get k => Read
    // wait --fn "x" => Eval;  wait 100 => Wait
}
#[test] fn defaults() { /* Upload, State, Clipboard denied; every other class allowed */ }
#[test] fn allow_and_deny_change_one_class_only() {}
#[test] fn validate_rejects_contradictions() {
    // allow+deny same class; allow navigate; deny upload; upload_dir without allow upload;
    // allow upload without upload_dir; allow_domains "https://a.test", "a.test/x", "*.a.test", "a.test:80", ""
}
#[test] fn pins_conflict_and_release() {
    // acquire(p, A) ok; acquire(p, A) ok; acquire(p, B) => profile_policy_conflict naming the item and its pinned value;
    // drop both A guards; acquire(p, B) ok
}
```

- [ ] **Step 2: 실패 확인.** `cargo test -p pegboard-core --lib policy` → 컴파일 오류.
- [ ] **Step 3: 구현.** 프로토콜 타입, `Form.class`, `policy.rs`, `run_command`의 판정, `SessionManager`가 세션 기록에 정책과 `PinGuard`를 쥠, `service`의 `session.create`가 `validate` 뒤 `PinTable::acquire`, CLI 옵션.
- [ ] **Step 4: 단위 시험 통과 확인.** `cargo test --workspace`.
- [ ] **Step 5: 실제 브라우저 시험을 쓴다** (`e2e_policy.rs`, 컨트롤러가 돌린다).

```rust
#[test] #[ignore] fn state_commands_need_allow_state() {
    // default session: `cookies set a b` => exit 1, stderr has policy_denied and "--allow state"; `cookies` (read) => 0
    // session created with --allow state: `cookies set a b` => 0
    // the recording of the denied command: command "cookies", args all REDACTED, redacted true, error "policy_denied..." code only
}
#[test] #[ignore] fn deny_eval_blocks_eval_and_wait_fn() {}
#[test] #[ignore] fn second_session_cannot_widen_a_pinned_profile() {
    // (uses whichever item Task 2 found to be profile-level; if none, assert that two sessions with
    //  different policies coexist in one profile and each keeps its own)
}
#[test] #[ignore] fn session_info_shows_the_policy_and_no_token() {}
#[test] fn bad_policy_options_exit_2() { /* no daemon needed: validation is repeated client-side */ }
```

- [ ] **Step 6: 1단계 시험의 기대값을 고친다.** `cookies set`과 `storage ... set`을 쓰는 기존 시험(`e2e_actionlog.rs`, `e2e_runner.rs`, `e2e_isolation.rs`)은 `--allow state`로 세션을 만들게 바꾼다. 어떤 시험을 바꿨는지 보고서에 적는다. 단언을 지우지 않는다.
- [ ] **Step 7: 커밋.** `feat: session policy with action classes; state writes need --allow state`

---

### Task 4: 주소 규칙

작업 2의 G1~G6 결과로 강제 방법을 정한다. 순수 판정 함수는 결과와 무관하게 먼저 만든다.

**Files:**
- Create: `crates/pegboard-core/src/address.rs`, `crates/pegboard-core/src/guard.rs`
- Modify: `crates/pegboard-core/src/session.rs`(탭이 세션에 들어올 때 `guard`를 건다), `proxy/filter.rs`(가로채기를 끄는 메서드 거부), `proxy/server.rs`
- Test: `address.rs` 단위 시험, `crates/pegboard-cli/tests/e2e_policy.rs`

**Interfaces:**
- Consumes: `SessionPolicy`(작업 3).
- Produces (`address.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict { Allow, Deny(&'static str) }   // the str is one of the reasons below

pub const OWN_PORT: &str = "pegboard's own port";
pub const SCHEME: &str = "scheme not allowed";
pub const NOT_LISTED: &str = "not in the session's allowed domains";
pub const LOOPBACK: &str = "localhost is closed for this session";
pub const PRIVATE: &str = "private network address";

/// `own_ports`: the browser's debugging port and the session proxy's port.
pub fn judge_url(url: &str, policy: &SessionPolicy, own_ports: &[u16]) -> Verdict;
/// Second look once the connection's real address is known.
pub fn judge_resolved(ip: std::net::IpAddr, port: u16, host: &str, policy: &SessionPolicy, own_ports: &[u16]) -> Verdict;
pub fn is_loopback(ip: IpAddr) -> bool;
pub fn is_private(ip: IpAddr) -> bool;
```

- 판정 순서(설계 5.3 그대로): ① 자기 포트(루프백 주소의 `own_ports`) 거부 ② 스킴(`http`, `https`, `ws`, `wss`, `data`, `about:blank`. `ws`/`wss`는 하위 요청에서만 나온다) ③ 허용목록이 있으면 목록의 도메인과 하위 도메인만(루프백도 목록에 있어야 하고, 사설망은 목록에 있어도 `allow_private_network`가 있어야 한다) ④ 루프백은 허용, `no_localhost`면 거부 ⑤ 사설망은 거부, `allow_private_network`면 허용.
- 주소 표기는 WHATWG URL 규칙으로 정규화한 뒤 판정한다(`url` 크레이트의 `Host`). `is_private`: `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `100.64/10`, `fc00::/7`, `fe80::/10`, IPv4로 매핑된 IPv6(`::ffff:a.b.c.d`)는 IPv4로 풀어서 판정, `0.0.0.0/8`과 `::`는 거부. `is_loopback`: `127/8`, `::1`, 호스트 이름 `localhost`와 `*.localhost`(끝의 점 무시, 대소문자 무시).
- Produces (`guard.rs`): 세션의 타깃에 요청 가로채기를 걸고 요청마다 `judge_url`, 주소가 알려지면 `judge_resolved`로 판정해 통과시키거나 끊는다. 걸지 못한 타깃은 세션에 넣지 않는다(닫고 오류). 구체적인 CDP 호출은 작업 2의 결과를 따른다.
- 거부된 탐색이 에이전트에게 보이는 꼴: `open <url>`이 `policy_denied`와 이유(`Verdict::Deny`의 문구)로 끝난다. 하위 요청이 막힌 것은 명령을 실패시키지 않는다(페이지 안의 요청 실패로 보인다).
- 1단계의 `normalize_url`(스킴 검사)은 그대로 두고, `open`과 `tab new`는 실행 전에 `judge_url`도 거친다(가로채기에 닿기 전에 명확한 오류를 주기 위해서).

- [ ] **Step 1: 실패하는 표 시험을 쓴다** (`address.rs`).

```rust
#[test] fn address_table() {
    // (url, policy, own_ports=[9222, 9333]) => verdict
    // default policy:
    //   http://localhost:3000/            Allow
    //   http://LOCALHOST./ , http://a.localhost/ , http://127.0.0.2/ , http://[::1]/   Allow
    //   http://127.0.0.1:9222/json , http://localhost:9333/ , ws://[::1]:9222/devtools/x   Deny(OWN_PORT)
    //   http://2130706433:9222/ , http://0x7f.1:9222/        Deny(OWN_PORT)   (normalise to 127.0.0.1)
    //   http://10.0.0.1/ , http://192.168.1.1/ , http://172.16.0.1/ , http://169.254.169.254/ ,
    //   http://100.64.0.1/ , http://[fc00::1]/ , http://[fe80::1]/ , http://[::ffff:10.0.0.1]/ , http://0.0.0.0/   Deny(PRIVATE)
    //   https://example.com/ , data:text/html,x , about:blank            Allow
    //   file:///etc/hosts , chrome://settings , javascript:1 , blob:x    Deny(SCHEME)
    // no_localhost:                 http://localhost/ => Deny(LOOPBACK); own port still Deny(OWN_PORT)
    // allow_private_network:        http://10.0.0.1/ => Allow; own port still Deny(OWN_PORT)
    // allow_domains [example.com]:  https://example.com/ , https://a.b.example.com/ Allow;
    //                               https://example.com.evil.test/ , https://notexample.com/ , http://localhost/ ,
    //                               https://example.org/  => Deny(NOT_LISTED)
    // allow_domains [localhost]:    http://localhost:3000/ Allow; own port Deny(OWN_PORT)
    // allow_domains [10.0.0.1]:     http://10.0.0.1/ Deny(PRIVATE); with allow_private_network => Allow
}
#[test] fn resolved_address_overrides_a_public_name() {
    // judge_resolved(10.0.0.5, 80, "public.example", default) => Deny(PRIVATE)
    // judge_resolved(127.0.0.1, 9222, "public.example", default) => Deny(OWN_PORT)
    // judge_resolved(127.0.0.1, 80, "public.example", default) => Allow (loopback is open by default)
    // with allow_domains [public.example] and a private address => Deny(PRIVATE)
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.** `cargo test -p pegboard-core --lib address`.
- [ ] **Step 3: `guard.rs`를 구현하고 세션에 건다.** 세션의 첫 탭, 프록시가 만든 탭, 입양한 팝업, `new_tab` 모두(1단계 최종 검토가 적은 "범위에 타깃이 들어가는 경로" 여섯 곳). 프록시 필터가 가로채기를 끄거나 가리는 메서드(작업 2 G4의 목록)를 거부하는지 단위 시험으로 고정한다.
- [ ] **Step 4: 실제 브라우저 시험을 쓴다** (`e2e_policy.rs`). 시험용 서버는 루프백의 두 포트에 띄우고, 사설 주소는 `--host-resolver-rules`를 줄 수 있는 시험 전용 환경 변수(`PEGBOARD_TEST_HOST_RULES`. 없으면 아무것도 하지 않는다)로 흉내 낸다.

```rust
#[test] #[ignore] fn own_ports_are_never_reachable() {
    // eval fetch('http://127.0.0.1:<debug port>/json/version') and to the proxy port: both fail in the page;
    // the same with --allow-private-network and with --allow-domain 127.0.0.1
    // open http://127.0.0.1:<debug port>/json => policy_denied
}
#[test] #[ignore] fn private_network_is_closed_unless_allowed() {
    // open http://private.test/ (mapped to a private address) => policy_denied;
    // from a page on localhost: eval fetch to it fails, an <img> to it does not load, a WebSocket to it does not open
    // session with --allow-private-network: all work
}
#[test] #[ignore] fn public_name_resolving_to_a_private_address_is_blocked() {}
#[test] #[ignore] fn redirect_to_private_address_is_blocked() {
    // http://localhost:<p>/redirect?to=http://private.test/secret : the page never receives the body
}
#[test] #[ignore] fn allow_domain_closes_everything_else_including_localhost() {
    // --allow-domain listed.test : open http://localhost:<p>/ => policy_denied; subrequest to unlisted.test fails;
    // --allow-domain listed.test,localhost : localhost opens
}
#[test] #[ignore] fn agent_cannot_turn_the_guard_off() {
    // core level (e2e_proxy style): a CDP client on the session endpoint sends each method from Task 2 G4;
    // each is refused; a fetch to a private address still fails afterwards
}
#[test] #[ignore] fn new_tabs_and_popups_are_guarded_before_their_first_request() {
    // tab new <private url> => denied; a page's window.open to a private url: the popup never loads it
}
#[test] #[ignore] fn workers_follow_the_rules_task_2_established() {
    // service worker / shared worker requests: blocked, or (if Task 2 found them profile-level) pinned per profile
}
```

- [ ] **Step 5: 남는 위험을 문서에 적는다.** 요청 자체가 부작용을 내는 경우(응답 단계에서만 주소를 알 수 있을 때)와, 작업 2에서 잡히지 않는다고 나온 요청 종류를 README "알려진 한계"와 architecture.md 11절 "남는 위험"에 한 줄씩.
- [ ] **Step 6: 커밋.** `feat: address rules enforced by the daemon (own ports, localhost, private network, allowed domains)`

---

### Task 5: 다운로드를 세션 디렉터리로

**Files:**
- Create: `crates/pegboard-core/src/downloads.rs`
- Modify: `crates/pegboard-core/src/browser.rs`(1단계의 `deny_downloads`, `deny_conns`, `denied`를 `downloads.rs`로 옮긴다), `session.rs`, `service.rs`, `daemon.rs`, `crates/pegboard-cli/src/main.rs`, `crates/pegboard-core/src/argv.rs`(필요하면)
- Test: `downloads.rs` 단위 시험, `e2e_policy.rs`

**Interfaces:**
- Consumes: `SessionPolicy`, `ActionClass::Download`.
- Produces: 프로필(에이전트 컨텍스트)마다 `pegboardd`만 아는 임시 폴더 `<PEGBOARD_HOME>/downloads/<프로필 디렉터리 이름>/`(0700). 다운로드가 끝나면 시작한 탭의 세션을 찾아 `<PEGBOARD_HOME>/sessions/<id>/files/`로 옮긴다. 옮길 때의 이름: 원래 이름에서 경로 구분자와 제어 문자를 빼고, 비면 `download`, 같은 이름이 있으면 `-1`, `-2`를 붙인다(기존 파일을 덮어쓰지 않는다). 0600.
- 지우는 경우: 세션을 찾을 수 없음, 그 세션의 정책이 `download`를 거부함, 파일 하나가 200 MiB를 넘음, 세션 합계가 1 GiB를 넘음, 세션이 그 사이 닫힘.
- Produces: 코어 API `files.list { sessionId, token }` → `[{ name, bytes, modifiedAt }]`(세션 파일 디렉터리의 일반 파일만, 이름순). CLI `pegboard files list`(세션은 `--session` 또는 `PEGBOARD_SESSION`).
- Produces: 다운로드 결과를 에이전트에게 알리는 길. 다운로드가 끝나거나 지워지면 그 세션의 다음 `command.exec` 결과의 `output`에 `"downloads": [{ "name", "bytes" } | { "name", "dropped": "<이유>" }]`를 붙이고(한 번만), 동작 기록에 항목 하나를 남긴다(`command: "download"`, 이름과 크기. URL은 1단계의 주소 정리 규칙으로).
- 정책 연결: 프로필마다 하나, 데몬 수명 동안 유지(1단계 측정). 끊기면 다시 걸고, 다시 걸릴 때까지 그 프로필의 세션은 `command.exec`를 `browser_failed`로 거절한다. 5초 안에 다시 걸지 못하면 그 프로필의 세션을 닫는다.
- 사람 프로필에는 어떤 다운로드 정책도 걸지 않는다(1단계 조건). 컨텍스트 ID 없이 부르지 않는다.

- [ ] **Step 1: 실패하는 단위 시험을 쓴다** (`downloads.rs`. Chrome 없이 도는 부분).

```rust
#[test] fn file_names_are_made_safe() {
    // "../../x.sh" => "x.sh"; "a/b\\c.txt" => "c.txt"; "" and "." and ".." => "download";
    // "report.pdf" twice => "report.pdf", "report-1.pdf"; a name with \n or \0 loses them; 300-byte name is cut to 200 bytes keeping the extension
}
#[test] fn limits() {
    // 200 MiB + 1 byte => dropped "file larger than 200 MiB"; total 1 GiB reached => dropped "session download limit of 1 GiB reached"
}
#[test] fn attribution() {
    // target in session A => A; unknown target => None; target whose session closed => None
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 1단계의 차단을 새 방식으로 바꾼다.** `--deny download`인 세션의 다운로드는 지운다. 1단계 시험 `agent_download_writes_no_file`은 "`~/Downloads`에 파일이 생기지 않는다"를 유지하고, "어디에도 생기지 않는다"는 `--deny download` 세션의 시험으로 옮긴다.
- [ ] **Step 4: 실제 브라우저 시험을 쓴다** (`e2e_policy.rs`). `~/Downloads`는 1단계 시험처럼 고유한 이름으로 읽기만 한다.

```rust
#[test] #[ignore] fn download_lands_in_the_session_files_dir_only() {
    // click the link => `files list` shows the file with the right size; nothing with that name in ~/Downloads;
    // the next command's output carries "downloads":[{name,bytes}] once
}
#[test] #[ignore] fn two_sessions_downloading_the_same_name_keep_their_own_file() {}
#[test] #[ignore] fn unattributed_download_is_deleted() {
    // a download started in the agent window from a tab that belongs to no session (opened by the test's human-side helper
    // in the agent profile): no file in any session dir, none in ~/Downloads, the staging dir is empty afterwards
}
#[test] #[ignore] fn deny_download_writes_no_file_anywhere() {}
#[test] #[ignore] fn oversized_download_is_dropped_and_reported() {
    // limits are lowered for the test through a test-only env var (PEGBOARD_TEST_DOWNLOAD_LIMITS="1024,4096")
}
#[test] #[ignore] fn download_policy_is_reapplied_after_its_connection_drops() {
    // the test closes the policy connection through a test-only hook; a download afterwards still lands in the session dir
    // and never in ~/Downloads; commands in between are refused, not run unprotected
}
#[test] #[ignore] fn human_downloads_still_work() { /* phase 1 test kept */ }
#[test] #[ignore] fn files_list_needs_the_session_token() {}
```

- [ ] **Step 5: 커밋.** `feat: downloads go to the session's files directory; policy connection is re-applied`

---

### Task 6: 업로드

**Files:**
- Modify: `crates/pegboard-core/src/argv.rs`(`upload` 형식), `daemon.rs`, `runner.rs`(필요하면), `crates/pegboard-cli/src/main.rs`
- Test: `argv.rs` 단위 시험, `e2e_policy.rs`

**Interfaces:**
- Consumes: `SessionPolicy.upload_dir`, `ActionClass::Upload`.
- Produces: 문법에 `upload <selector> <file>...` 형식(분류 `upload`). `<file>`은 맨 파일 이름이고, 다음 두 곳에서 이 순서로 찾는다: 세션의 `upload_dir`, 세션 파일 디렉터리(다운로드로 받은 것). 1단계의 파일 이름 규칙을 그대로 쓴다(`paths.rs`의 검사: 경로 구분자 없음, 심볼릭 링크 아님, 소유자, 폴더 밖을 가리키지 않음). 확장자 제한은 두지 않는다.
- agent-browser에는 찾은 파일의 절대 경로를 넘긴다. 동작 기록에는 파일 이름만 남긴다(경로 없음).
- 프록시 필터가 업로드에 필요한 CDP 메서드(`DOM.setFileInputFiles` 등)를 지금 막고 있으면, 그 메서드는 `pegboardd`가 판정을 통과시킨 명령에서만 지나가게 한다. 에이전트가 CDP를 직접 보낼 길은 없지만(MCP와 CLI만), 필터의 기본 거부를 넓히지 않는다: 파일 경로 인자가 그 세션의 `upload_dir` 또는 세션 파일 디렉터리 안인지 필터에서 다시 검사한다.

- [ ] **Step 1: 실패하는 단위 시험.**

```rust
#[test] fn upload_takes_bare_file_names_only() {
    // "a.txt" ok; "../a.txt", "/etc/passwd", "sub/a.txt", "", "." rejected (argv_rejected)
}
#[test] fn upload_is_class_upload() {}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_policy.rs`).

```rust
#[test] #[ignore] fn upload_needs_allow_and_a_directory() {
    // default session: upload => policy_denied naming --allow upload and --upload-dir
    // --allow upload --upload-dir D: a file in D uploads (the page's input.files[0].name and size match)
}
#[test] #[ignore] fn upload_cannot_reach_outside_the_directory() {
    // a symlink inside D pointing outside; a name with ..; an absolute path: each rejected, the page's input stays empty
}
#[test] #[ignore] fn a_downloaded_file_can_be_uploaded() {}
#[test] #[ignore] fn upload_is_logged_by_file_name_only() {}
```

- [ ] **Step 4: 커밋.** `feat: upload from a directory the person names, behind --allow upload`

---

### Task 7: 클립보드

작업 2의 C1~C5 결과로 방법을 정한다.

**Files:**
- Modify: `crates/pegboard-core/src/session.rs` 또는 `browser.rs`(프로필에 권한 설정), `argv.rs`(키 조합), `policy.rs`(`ProfilePins`에 항목), `daemon.rs`
- Test: 단위 시험, `e2e_policy.rs`

**Interfaces:**
- Consumes: `ActionClass::Clipboard`, `ProfilePins`.
- Produces: `--allow clipboard`가 없는 세션에서 다음이 시스템 클립보드를 읽지도 바꾸지도 못한다: `eval`의 `navigator.clipboard.*`, `document.execCommand('copy'|'cut'|'paste')`, `press`와 `keyboard`의 복사, 잘라내기, 붙여넣기 조합(`Meta+c`, `Meta+x`, `Meta+v`, `Control+c|x|v`, `Control+Insert`, `Shift+Insert`, `Shift+Delete`).
- 키 조합은 `argv::class_of`가 `clipboard`로 분류해 정책 판정에서 거부한다(대소문자와 수식 키 순서를 가리지 않는다).
- 페이지 쪽 경로는 작업 2가 찾은 방법으로 막는다. 프로필 단위로만 되는 것이면 `ProfilePins`에 `clipboard: bool`을 넣는다(D8).

- [ ] **Step 1: 실패하는 단위 시험.**

```rust
#[test] fn clipboard_key_combinations_are_class_clipboard() {
    // press Meta+c, meta+C, Control+v, Shift+Insert, Control+Shift+v, c+Meta => Clipboard
    // press c, Meta+a, Enter => not Clipboard
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_policy.rs`). 시험은 시작할 때 시스템 클립보드 내용을 저장하고, 표식 문자열을 넣은 뒤, 끝날 때 원래 내용으로 되돌린다(패닉이 나도 되돌리는 가드).

```rust
#[test] #[ignore] fn agent_cannot_write_the_clipboard_by_default() {
    // marker M in the clipboard; then eval navigator.clipboard.writeText('X'), execCommand('copy') on a selection,
    // press Meta+c on a focused selection: after each, the system clipboard still holds M
}
#[test] #[ignore] fn agent_cannot_read_the_clipboard_by_default() {
    // marker M in the clipboard; eval navigator.clipboard.readText() does not return M; press Meta+v into an input leaves it empty
}
#[test] #[ignore] fn allow_clipboard_opens_it() {}
#[test] #[ignore] fn clipboard_setting_of_one_session_does_not_leak_to_another() {
    // profile-level: a second session with a different clipboard setting in the same profile is refused
    //   (profile_policy_conflict naming clipboard); in another profile it is created and keeps its own setting
    // tab-level: both sessions coexist and each keeps its own setting
}
```

- [ ] **Step 4: 막지 못한 경로가 있으면** 그 경로를 README "알려진 한계"와 architecture.md 11절에 적고 보고서 맨 위에 쓴다(작업 2의 멈춤 조건에 걸리지 않은 범위에서).
- [ ] **Step 5: 커밋.** `feat: clipboard is closed for agent sessions unless allowed`

---

### Task 8: 세션 수명과 넘어온 수정

**Files:**
- Modify: `crates/pegboard-core/src/session.rs`, `service.rs`, `api.rs`, `proxy/filter.rs`, `proxy/server.rs`, `argv.rs`, `extension/bridge/sw.js`(필요하면), `crates/pegboard-cli/src/main.rs`
- Test: 단위 시험, `e2e_session.rs`, `e2e_cli.rs`, `e2e_proxy.rs`

**Interfaces:**
- Produces: 세션 닫기는 그 세션의 범위에 있는 탭과, 그 세션의 그룹 안에 있지만 **어느 세션의 범위에도 없는** 탭만 닫는다(D4). 다른 세션의 범위에 있는 탭은 그 그룹 안에 있어도 닫지 않는다.
- Produces: 유휴 만료. 마지막 `command.exec` 뒤 1시간이 지나면 세션을 닫는다(기록 마감 포함). `SessionPolicy.no_expire`면 하지 않는다. 시험은 기간을 시험 전용 환경 변수(`PEGBOARD_TEST_IDLE_SECS`)로 줄인다.
- Produces: 입력 상한. `command.exec`의 argv 단어 하나 64 KiB, argv 전체 256 KiB, 단어 수 256. 넘으면 `limit_exceeded`(CLI 종료 코드 2). 동작 기록 한 줄은 64 KiB를 넘지 않는다(넘는 인자는 `[<n> bytes]`로 바꾼다).
- Produces: `Client::call`이 응답의 `id`를 요청의 `id`와 대조한다. 다르면 그 연결을 버리고 `internal` 오류를 낸다. 시간 초과 뒤에는 그 `Client`를 다시 쓸 수 없다(다음 호출이 새로 연결한다).
- Produces: 세션의 범위에서 빠진 타깃(사람이 닫은 탭)의 `Target.targetDestroyed`와 `Target.detachedFromTarget`을, 그 연결이 전에 알던 타깃과 세션에 한해 에이전트 쪽에 전한다. 그 연결이 모르던 타깃의 이벤트는 지금처럼 버린다.
- Produces: `tab new 'data:...'`가 그 data 주소로 열린다(지금은 성공으로 끝나지만 `about:blank`다).

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
// e2e_session.rs (ignored)
async fn closing_a_session_leaves_another_sessions_tab_that_sits_in_its_group() {
    // A has t1, t2; the test moves A's t2 into B's group through the bridge (chrome.tabs.group); close B:
    // t2 still exists and is still in A's `targets`; B's own tab is gone
}
async fn closing_a_session_closes_stray_tabs_in_its_group() { /* a tab the human opened inside the group */ }
async fn idle_session_is_closed_after_the_expiry() { /* PEGBOARD_TEST_IDLE_SECS=2; no_expire session survives */ }
// e2e_proxy.rs (ignored)
async fn client_learns_that_a_tab_it_knew_was_closed_by_the_human() {
    // after the human closes the tab: the session client receives Target.targetDestroyed for it; `tab list` no longer shows it
}
async fn client_never_hears_about_targets_it_did_not_know() {}
// e2e_cli.rs (ignored)
fn tab_new_opens_a_data_url() { /* `tab new 'data:text/html,<title>d</title>'` then `get title` => "d" */ }
// unit / api_server.rs
fn oversized_argv_is_refused() { /* one word of 64 KiB + 1, 257 words, 256 KiB + 1 in total => limit_exceeded */ }
fn a_late_response_is_not_taken_for_the_next_call() {
    // stub server answers call 1 after the client's timeout; call 2 on the same Client gets its own answer or an error, never call 1's
}
fn a_log_line_never_exceeds_64_kib() {}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.** 수정마다 커밋 하나.
- [ ] **Step 3: 1단계 문서를 고친다.** README "알려진 한계"에서 해결된 줄(닫힌 탭이 목록에 남는 것, 세션 닫기가 끌려 온 탭을 닫는 것)을 지우고, M6의 기대 결과 설명에 "2a부터 B를 닫아도 A의 탭은 닫히지 않는다"를 덧붙인다.
- [ ] **Step 4: 커밋.**

---

### Task 9: `pegboard mcp` 서버와 관리 도구

**Files:**
- Create: `crates/pegboard-cli/src/mcp/mod.rs`, `crates/pegboard-cli/src/mcp/tools.rs`
- Modify: `crates/pegboard-cli/src/main.rs`(`mcp` 하위 명령), `crates/pegboard-core/src/service.rs`(운영자용 메서드)
- Test: `mcp` 모듈의 단위 시험, `crates/pegboard-cli/tests/e2e_mcp.rs`

**Interfaces:**
- Consumes: `Client`(코어 API), `SessionPolicy`, 작업 3의 CLI 정책 옵션(같은 이름).
- Produces: `pegboard mcp [--operator] [--tools <묶음,...>] [정책 옵션]`. stdio로 줄 단위 JSON-RPC 2.0. 지원 메서드: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`. 그 밖은 `-32601`. 표준 출력에는 프로토콜 메시지만 쓴다(로그는 표준 오류).
- Produces (`mcp/mod.rs`):

```rust
pub struct Options { pub operator: bool, pub tools: Vec<Bundle>, pub policy: SessionPolicy }
pub enum Bundle { Core, Tabs, State, Network, All }
/// Sessions this server may act on. Tokens never leave this type.
pub struct Owned { /* session id -> token, created_here: bool */ }
impl Owned {
    pub fn adopt_from_env(&mut self) -> Result<(), String>;      // PEGBOARD_SESSION handle
    pub fn insert(&mut self, id: SessionId, token: String, created_here: bool);
    pub fn token(&self, id: &SessionId) -> Option<&str>;
    pub fn ids(&self) -> Vec<SessionId>;
}
pub async fn serve(opts: Options, paths: Paths) -> i32;    // exit code
```

- 시작할 때: 데몬이 없으면 `browser start`와 같은 경로로 띄운다(기한 20초). 띄우지 못하면 `initialize`에 오류로 답하지 않고, 첫 `tools/call`에 "pegboard is not running and could not be started: <이유>"를 돌려준다(MCP 클라이언트가 서버 시작 실패를 사용자에게 보여 주지 않는 경우가 많다).
- 끝날 때(stdin EOF, SIGTERM, SIGINT): `created_here`인 세션을 닫는다(세션마다 기한 15초). 넘겨받은 세션은 닫지 않는다.
- 세션 수 상한: 이 서버가 만든 살아 있는 세션 16개. 넘으면 `limit_exceeded`.
- 관리 도구(설계 5.2의 표). 기본 모드에서 `pegboard_session_create`의 인자는 `name`, `url`, `profile`뿐이고 스키마에 `additionalProperties: false`를 둔다. 정책 키를 주면 오류다(무시하지 않는다). 정책은 `Options.policy`가 붙는다. `--operator`에서는 `policy` 인자를 받는다.
- 기본 모드에서 자기 것이 아닌 `sessionId`나 그 세션의 `recordingId`를 주면, 없는 세션과 같은 오류(`session_not_found`, 같은 문구)를 돌려준다.
- `--operator`가 남의 세션에 할 수 있는 것: 목록, `session_info`, `session_close`, 기록 읽기. 조종(동작 도구)은 자기 세션만. 그래서 코어 API에 토큰 없이 닫고 정보를 보는 운영자용 메서드가 필요하다: `session.close_any { sessionId, keepTabs }`, `session.info_any { sessionId }`(소켓 소유자 검사만. `browser.stop`과 같은 수준). CLI에는 노출하지 않는다.
- 도구 결과: 성공은 `content: [{type:"text", text}]`, 실패는 `isError: true`와 `<code>: <message>`. 결과 어디에도 토큰과 핸들이 없다.
- `pegboard_session_create`의 결과 텍스트는 세션 ID, 이름, 프로필, 붙은 정책 한 줄 요약(에이전트가 무엇이 막혀 있는지 미리 안다).

- [ ] **Step 1: 실패하는 단위 시험** (`mcp` 모듈. 코어 API는 스텁).

```rust
#[test] fn initialize_and_tools_list() { /* protocol version echoed; list has the management tools of the mode */ }
#[test] fn default_mode_has_no_profile_or_stop_tools() {}
#[test] fn policy_keys_are_refused_in_default_mode() {
    // session_create {name:"x", allow:["upload"]} and {policy:{...}} => isError, no session created (stub saw no call)
}
#[test] fn foreign_session_ids_look_like_missing_ones() {
    // session_info / session_close / recording_get with an id not in Owned => the same text as for an id that does not exist
}
#[test] fn results_never_contain_a_token() {
    // every tool's result and every error path, with a stub that returns tokens in its replies
}
#[test] fn session_limit_is_16() {}
#[test] fn malformed_input_does_not_stop_the_server() {
    // a non-JSON line, a batch array, a request with no id, a 5 MiB line, an unknown method: each answered or skipped; the next request still works
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_mcp.rs`. 시험은 `pegboard mcp`를 자식 프로세스로 띄우고 stdin/stdout으로 말한다. 하네스: `Mcp::start(home, args) -> Mcp`, `mcp.call(tool, json) -> (is_error, text)`).

```rust
#[test] #[ignore] fn mcp_starts_the_daemon_and_creates_a_session() {}
#[test] #[ignore] fn sessions_are_closed_when_stdin_closes() {
    // create two sessions over MCP, close stdin: within 20 s `pegboard session list` (CLI) shows none of them;
    // a session adopted from PEGBOARD_SESSION is still there
}
#[test] #[ignore] fn default_mode_cannot_see_or_touch_another_servers_session() {
    // server 1 creates S; server 2: session_list does not show S; session_info/close/recording_get with S's id => session_not_found
}
#[test] #[ignore] fn operator_mode_lists_and_closes_but_does_not_drive_other_sessions() {}
#[test] #[ignore] fn server_policy_is_attached_to_every_session() {
    // `pegboard mcp --deny eval`: `pegboard session info` (CLI) for the created session shows deny eval
}
#[test] #[ignore] fn adopted_session_keeps_its_own_wider_policy() {}
```

- [ ] **Step 4: 커밋.** `feat: pegboard mcp server with own-sessions and operator modes`

---

### Task 10: 문법에서 생성한 동작 도구와 동등성

**Files:**
- Modify: `crates/pegboard-core/src/argv.rs`(인자 이름, `forms()`), `crates/pegboard-cli/src/mcp/tools.rs`
- Test: `argv.rs`와 `tools.rs`의 단위 시험, `crates/pegboard-cli/tests/e2e_mcp.rs`

**Interfaces:**
- Consumes: 작업 9의 `Owned`, `Options.tools`.
- Produces (`argv.rs`): 문법의 공개 설명. `Arg`와 `Flag`에 MCP 인자 이름이 필요하므로 `Arg`에 `name: &'static str`를 더한다(예: `selector`, `text`, `url`, `key`, `file`, `x`, `y`).

```rust
pub struct FormSpec {
    pub words: &'static [&'static str],
    pub class: ActionClass,
    pub args: Vec<ArgSpec>,
    pub rest: Option<RestSpec>,
    pub flags: Vec<FlagSpec>,
}
pub struct ArgSpec { pub name: &'static str, pub kind: ArgKind, pub required: bool, pub secret: bool }
pub struct RestSpec { pub name: &'static str, pub kind: ArgKind, pub required: bool }
pub struct FlagSpec { pub names: &'static [&'static str], pub value: Option<ArgKind> }
pub enum ArgKind { Selector, Text, Expr, Url, Key, Number, TabRef, File, Choice(&'static [&'static str]) }
pub fn forms() -> Vec<FormSpec>;
```

- Produces (`tools.rs`): 도구 하나가 명령의 첫 단어 하나다. 이름은 `pegboard_<첫 단어>`. 인자: `sessionId`(자기 세션이 하나뿐이면 생략 가능. 둘 이상인데 생략하면 오류), 하위 명령이 있는 명령은 `command`(하위 단어들을 공백으로 이은 문자열의 enum. 예: `pegboard_get`의 `"url"`, `"text"`), 그리고 그 명령의 형식들이 가진 인자와 플래그의 합집합(플래그는 `--` 없는 긴 이름을 camelCase로). 스키마는 `additionalProperties: false`.

```rust
pub fn action_tools(bundles: &[Bundle]) -> Vec<ToolDef>;             // generated from argv::forms()
pub fn to_argv(tool: &str, arguments: &serde_json::Value) -> Result<Vec<String>, String>;
```

- `to_argv`의 규칙: 문자열 인자는 argv 단어 하나가 된다(쪼개지 않는다). `-`로 시작하는 값이 플래그로 읽히는 자리에는 넣을 수 없고(문법이 `--` 뒤의 글자 그대로를 허용하는 `fill`, `type`의 텍스트만 `--`를 앞에 붙여 넘긴다), 배열은 반복 값을 받는 자리에만, 숫자는 Number 자리에만 받는다. `to_argv`가 만든 argv는 반드시 `argv::check`를 다시 거친다(데몬에서). `to_argv`는 편의이고 검사가 아니다.
- 도구 묶음(첫 단어 기준): `core` = `open back forward reload snapshot screenshot pdf get is click dblclick fill type press hover focus check uncheck select scrollintoview wait eval dialog frame upload`와 `pegboard_files_list`, `tabs` = `tab`, `state` = `cookies storage`, `network` = `network console errors vitals a11y`, `all` = 전부. 문법의 모든 첫 단어가 어느 묶음에든 들어 있어야 한다(시험으로 고정).
- `screenshot`: 결과 텍스트와 함께 저장된 PNG를 MCP 이미지 콘텐츠(`{type:"image", data:<base64>, mimeType:"image/png"}`)로 돌려준다. 5 MiB를 넘으면 이미지는 빼고 파일 이름만 알려 준다.
- 도구 설명은 영어 한두 문장. 설명에 CDP, 디버깅 포트, 다른 세션, 프로필 디렉터리를 언급하지 않는다.

- [ ] **Step 1: 실패하는 단위 시험.**

```rust
#[test] fn every_first_word_of_the_grammar_is_a_tool_in_some_bundle() {}
#[test] fn round_trip_every_form() {
    // for each FormSpec, build a minimal valid arguments object from its spec, to_argv it,
    // and assert argv::check accepts the result and argv::class_of gives the form's class
}
#[test] fn tool_arguments_cannot_build_argv_outside_the_grammar() {
    // {selector:"--cdp"}, {selector:"#a", cdp:9222}, {text:["a","b"]} where one word is expected, {x:"1; rm"} for a Number,
    // {command:"url --cdp 9222"}, {sessionId:"s_abcdefgh.token"}, nested arrays, a 1 MiB string:
    // each is an error from to_argv, or produces an argv that argv::check rejects; none reaches the runner (stub)
}
#[test] fn fill_text_starting_with_a_dash_is_sent_as_text() { /* {selector:"#a", text:"--cdp 9222"} => [..., "--", "--cdp 9222"] */ }
#[test] fn schemas_reject_unknown_keys() {}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 동등성 시험** (`e2e_mcp.rs`, 실제 브라우저).

```rust
#[test] #[ignore] fn cli_and_mcp_run_the_same_command_the_same_way() {
    // one session; for a list covering every first word (open, snapshot -i, get url, get text #out, click #btn, fill #name "a b",
    // press Enter, tab list, eval "1+1", screenshot, wait 100, is visible #btn, cookies):
    // run it through the CLI and through MCP; the two action-log entries have the same command, args, ok, redacted and url;
    // the MCP text equals the CLI's stdout (modulo the boundary nonce)
}
#[test] #[ignore] fn the_same_inputs_are_rejected_by_both() {
    // the escape cases of e2e_isolation's escape_flags_and_commands_are_refused, through MCP: all rejected, none runs
}
#[test] #[ignore] fn secrets_typed_over_mcp_are_redacted_like_the_cli() {
    // fill a password field over MCP; the recording never contains the text (phase 1 test, MCP edition)
}
#[test] #[ignore] fn screenshot_returns_an_image() {}
#[test] #[ignore] fn page_text_carries_boundary_markers() {}
```

- [ ] **Step 4: 커밋.** `feat: MCP action tools generated from the command grammar; CLI/MCP equivalence tests`

---

### Task 11: MCP 격리 시험 묶음과 문서

**Files:**
- Create: `crates/pegboard-cli/tests/e2e_mcp_isolation.rs`
- Modify: `README.md`, `docs/architecture.md`(7절, 11절), `docs/roadmap.md`(2단계), `docs/phase1-manual-checks.md`(필요한 줄만)

이 묶음은 MCP 도구만으로 "셸 없는 에이전트가 할 수 있는 일"을 시도한다. `e2e_isolation.rs`처럼 시험마다 깨끗한 브라우저를 쓰고(작업 1의 공유 대상이 아니다), 이후 모든 단계에서 유지한다.

- [ ] **Step 1: 시험을 쓴다.** 준비는 `e2e_isolation.rs`의 `Rig`와 같다(사람 프로필에 쿠키가 있는 탭 하나, 세션 A와 B는 서로 다른 MCP 서버, C는 다른 프로필).

```rust
#[test] #[ignore] fn mcp_agent_cannot_list_or_switch_to_human_tabs() {}
#[test] #[ignore] fn mcp_agent_cannot_read_human_cookies_or_storage() {}
#[test] #[ignore] fn mcp_new_tabs_open_in_the_agent_profile_with_the_human_window_last_active() {
    // same premise control as the CLI suite (re-invoke Chrome with the human profile window last)
}
#[test] #[ignore] fn mcp_agent_cannot_reach_another_servers_session() {
    // by session id, by target id (pegboard_tab {command:"", tab:"<B's target id>"}), by recording id
}
#[test] #[ignore] fn mcp_agent_cannot_widen_its_policy() {
    // every tool, with policy-looking keys added; session_create with policy keys; then a state write is still policy_denied
    // and `pegboard session info` (CLI) shows the server's policy unchanged
}
#[test] #[ignore] fn mcp_results_and_errors_never_carry_tokens_or_ports() {
    // collect every tool result of a run that includes failures; none contains the session token, the proxy token,
    // the debugging port number or a ws:// URL
}
#[test] #[ignore] fn mcp_agent_cannot_reach_pegboards_own_ports_or_the_private_network() {}
#[test] #[ignore] fn mcp_agent_cannot_write_the_clipboard() {}
#[test] #[ignore] fn mcp_download_lands_only_in_the_session_dir() {}
#[test] #[ignore] fn killed_mcp_server_leaves_sessions_that_expire() {
    // SIGKILL the server: its sessions are still listed by the CLI, then closed by the idle expiry (PEGBOARD_TEST_IDLE_SECS=3)
}
```

- [ ] **Step 2: 컨트롤러가 돌린다.** 실패하는 것이 있으면 시험을 느슨하게 고치지 않는다. 원인을 찾아 제품을 고치고, 고칠 수 없는 한계면 README "알려진 한계"와 architecture.md 11절에 적은 뒤 `#[ignore = "known limit: ..."]` 사유를 붙이고 컨트롤러가 사용자에게 알린다.
- [ ] **Step 3: 각 시험이 실패할 수 있음을 보인다.** 시험마다 지키는 코드를 잠깐 망가뜨려 실패를 확인하고 되돌린다(1단계 작업 10의 방식). 망가뜨릴 수 없는 시험은 그 이유를 적는다.
- [ ] **Step 4: 문서.**
  - README: "빠른 시작"에 MCP 설정 예(클라이언트 설정에 넣을 `command`와 `args`, 정책 옵션 예 하나), "세션 정책" 절(옵션 표와 기본값), "아직 안 되는 것"과 "알려진 한계" 갱신. 빠른 시작의 모든 명령과 설정 예는 실제로 한 번 실행해 본 것만 적는다(컨트롤러가 확인).
  - architecture.md 7절: MCP 도구 구성이 "agent-browser의 도구를 중계"에서 "문법에서 생성"으로 바뀐 것과 이유. 11절: 기본 정책 표(`state` 거부, 클립보드, 내부망, 자기 포트), "남는 위험"에 작업 2, 4, 5, 7이 남긴 것.
  - roadmap.md: 2단계를 2a와 2b로 나눈 것, 2a의 완료 상태(자동 시험 이름과 함께), 프로필 복사가 2b 뒤로 간 것, 2a가 남긴 것(실시간 승인, `network route`, 자동 연결 흉내, 파일 기술자 정리).
- [ ] **Step 5: 전체 확인.** `cargo fmt --check && cargo clippy --workspace --tests -- -D warnings && cargo test --workspace`, 그리고 컨트롤러가 `cargo test --workspace --no-fail-fast -- --ignored --test-threads=1`.
- [ ] **Step 6: 커밋.** `test: MCP isolation suite; docs: MCP setup, session policy, phase 2a status`

---

## 작업 순서와 의존

```
1 (시험 브라우저 재사용) ─┐
2 (측정) ─────────────────┼─> 3 (정책) ─> 4 (주소) ─> 5 (다운로드) ─> 6 (업로드) ─> 7 (클립보드) ─> 8 (수명) ─> 9 (MCP 서버) ─> 10 (동작 도구) ─> 11 (격리 묶음, 문서)
```

- 1과 2는 서로 독립이다. 2의 멈춤 조건에 걸린 작업(4, 5, 7 가운데)은 사용자의 결정이 나올 때까지 건너뛰고 다음 작업으로 간다.
- 9와 10은 3~8이 끝난 뒤에 한다. MCP가 생기기 전에 정책이 먼저 있어야, MCP가 열리는 순간부터 묶여 있다.
