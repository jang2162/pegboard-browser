# 3b단계: 브라우저 화면 패치 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `upload`와 `state`가 사람에게 묻는 동작이 되고(포크 모드는 에이전트 창의 확인 창, 어디서나 코어 API의 `confirm.respond`), 에이전트 창이 구분되고 에이전트 프로필이 숨으며, 구글 로그인·비밀번호 관리자가 꺼지고, 제품 이름과 아이콘이 Pegboard가 되고, 데몬 다시 띄우기를 포기하면 사람이 안다.

**Architecture:** 판정·대기·시간은 데몬의 확인 중개자(`confirm` 모듈)가 쥔다. 정책 판정에 셋째 값 `Ask`가 생기고, `run_command`가 `Ask`에서 보여 줄 내용을 만들고(업로드는 사본을 먼저 뜬다) 중개자에 묻는다. 응답자는 API 구독자와(어느 모드든) 브라우저(포크 모드, `Pegboard.requestConfirmation` + `confirmationAnswered` 이벤트)이고 먼저 온 답이 이긴다. 브라우저 쪽 새 코드는 `//pegboard/ui`(확인 창, 알림 창, 시험 훅)와 `//pegboard/browser_parts`(메인 파츠, 설정 기본값)에 두고, 표시·숨기기는 기존 도메인 핸들러가 Chromium의 기존 기능(프로필 테마 색, 창 사용자 제목, 프로필 생략)으로 건다. 제품 이름 문자열은 생성 스크립트가 바꾼다.

**Tech Stack:** Rust(기존 워크스페이스), tokio, serde_json, `libc`(`O_NOFOLLOW`). Chromium Extended Stable `156.0.8078.109`, C++, GN/ninja, Python 3(grit), Swift(아이콘). 순정 모드 시험은 Chrome for Testing 157, agent-browser 0.42.0.

**Spec:** [2026-11-25-phase3b-browser-ui-design.md](./2026-11-25-phase3b-browser-ui-design.md). 앞 문서: [3a 설계](./2026-11-21-phase3a-fork-skeleton-design.md)와 [3a 계획](./2026-11-21-phase3a-fork-skeleton.md), [2a 설계](./2026-11-13-phase2a-mcp-and-policy-design.md) 5.3·5.5, [architecture.md](../architecture.md) 7절 API 표, 11절.

## Global Constraints

- 설계 2절의 실패 조건이 하나라도 생기면 그 작업은 끝난 것이 아니다: 페이지·에이전트가 확인에 답하거나 꾸민다, 세션 A의 답이 B를 푼다, 창이 보인 것과 실제가 다르다, 확인을 기다리는 명령이 끝나지 않는다, 창이 사람 창의 키보드 초점을 가져간다, 사람 프로필에 표시·숨기기가 걸린다.
- 3a 계획 Global Constraints의 보장 목록을 약하게 만들지 않는다(`argv::check`, 가림 규칙, 프록시 기본 거부, 요청 가드, 클립보드 방어, 다운로드 귀속, 녹화 지연 기준 60ms, 사람 프로필에 아무것도 걸지 않기, `Pegboard` 도메인은 주인 세션 하나에만).
- 묻는 분류는 `upload`, `state` 둘. `--allow` = 묻지 않고 허용, `--deny` = 묻지 않고 거부. `--allow upload`에는 여전히 `--upload-dir`이 필요하다. `network`는 문법에 넣지 않는다.
- 확인 시간 **60초**(`CONFIRM_TIMEOUT`) → 거부. 결과는 `policy_denied`, `data.reason` ∈ {`denied`, `timed_out`, `no_responder`, `cancelled`, `stale`}, `data.confirmId`. 늦은 답과 없는 ID는 `confirm_not_found`.
- 답 = `approve` | `deny` | `allowForSession`. 버튼 문구 "허용", "거부", "이 세션에서 계속 허용". 기본 버튼은 거부, Esc·창 닫기도 거부, 버튼은 보인 뒤 **1초** 잠긴다. 창은 비활성으로 띄운다. 업로드의 "이 세션에서 계속 허용"은 `--upload-dir`이 있을 때만 내놓는다.
- 순정 모드에는 확인 창이 없다. 응답자(API 구독자)가 없으면 바로 `no_responder`, 메시지에 "confirmation needs the Pegboard browser".
- 정한 값: `confirmId` 32자리 소문자 16진수(128비트), 브라우저 창 안전 시간 75초, 동시에 열린 확인 창 16개, `details` 40줄·값 1,000자, 창에 보이는 쿠키·저장소 값 200자, 업로드 파일 하나 200 MiB·한 명령 16개.
- 에이전트 프로필 판정은 3a 그대로(디렉터리 이름이 정확히 `PegboardAgent`이거나 `PegboardAgent-`로 시작). 모르는 컨텍스트는 사람 것.
- 시험 훅: Rust는 `testenv::var`(`PEGBOARD_TEST_HOOKS`가 있을 때만). C++은 브라우저 자신의 환경에 `PEGBOARD_TEST_HOOKS`가 있을 때만 `PEGBOARD_TEST_DIALOG_*`를 읽는다. **데몬에는 자동으로 답하는 훅을 두지 않는다.**
- **실제 브라우저를 띄우는 것은 구현 에이전트가 하지 않는다.** Rust 작업(1~5)의 구현 에이전트는 코드, 브라우저 없는 시험(`cargo test --workspace`), `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`까지 하고, 보고서에 (1) 돌려야 할 명령 그대로, (2) 시험마다 기대 결과, (3) 새 브라우저 시험마다 "이 한 줄을 이렇게 바꾸면 실패한다"는 변형 하나를 적는다. 컨트롤러가 맥미니에서 돌리고 변형까지 확인한다.
- **C++ 작업(6~10)은 개발 기기의 Chromium 트리**(`156.0.8078.109`, 브랜치 `pegboard-work`)에서 `browser/scripts/{apply,export,build,package-dev,budget}.sh`로 한다. 빌드는 언제나 `-j 8`(전체 병렬에서 기기가 커널 패닉으로 멈췄다)이고 도구 시간 제한에 끊기지 않게 떼어 돌리고 끝을 기다린다(증분 30초~3분, 전체 다시 빌드 약 2시간 45분). 앱은 `package-dev.sh`로 만들어 맥미니로 옮긴다.
- **실제 브라우저 시험은 맥미니의 GUI 세션**(터미널 앱, 화면 잠금 풀림, 원격 데스크톱 연결 끊음. 클립보드 동기화가 시험을 흔든다)에서 `--test-threads=1`로 돈다. 포크를 띄우는 시험은 언제나 `--use-mock-keychain`. 임시 `PEGBOARD_HOME`만 쓴다.
- 모든 C++ 변경에는 변형 확인("이 줄을 이렇게 바꾸면 시험 X가 실패한다")을 적고 컨트롤러가 빌드해 확인한다. 작업 4, 7은 구현 검토와 별도로 **보안 검토자** 한 명이 더 본다.
- 업스트림 예산: 3a 끝 15개 파일, 120줄. 3b 손 패치 증가 목표 15줄 이하. 패치마다 `budget.sh` 값을 커밋 메시지나 보고서에 적는다. 새 UI는 `//pegboard` 안에.
- 문서와 코드에 홈 디렉터리 경로, 사용자 이름, 기기 이름을 쓰지 않는다("개발 기기의 Chromium 트리"). 문서는 한국어. 커밋 메시지는 영어, 끝에 `Co-Authored-By: <구현한 모델 이름> <noreply@anthropic.com>`. 푸시와 main 병합은 컨트롤러가 한다.

## Review Focus

설계가 암시하지만 시험이 놓치기 쉬운 것. 각 줄의 시험은 괄호 안 작업에 넣었다.

1. **에이전트나 페이지가 확인을 푼다.** 같은 탭에 다른 세션이나 세션 엔드포인트에 직접 붙은 클라이언트가 `Input.dispatchKeyEvent`(Enter, Space), `Page.handleJavaScriptDialog {accept:true}`, `Pegboard.confirmationAnswered` 흉내, `Pegboard.*` 호출을 보내도 확인은 그대로 기다리다 `timed_out`이 된다. MCP 도구 목록에 `confirm`이 없다(작업 7 `agent_input_cannot_answer_the_dialog`, `proxy_refuses_pegboard_confirmation_methods`, 작업 3 `mcp_has_no_confirmation_tool`).
2. **세션 A의 답이 세션 B를 푼다.** 두 세션이 동시에 기다릴 때 A의 `confirmId`로 답하면 B는 그대로이고, A의 "이 세션에서 계속 허용"은 B와 A의 다른 분류를 넓히지 않는다(작업 2 `an_answer_only_resolves_its_own_confirmation`, `allow_for_session_grants_that_session_and_class_only`, 작업 3 e2e `an_answer_for_session_a_does_not_release_session_b`, 작업 7 `two_sessions_get_their_own_dialogs`).
3. **승인과 실행 사이에 바뀐다** (TOCTOU). 확인을 띄운 뒤 원본 파일을 고치거나 링크로 바꿔도 올라가는 것은 띄울 때의 내용이다. 승인 뒤 탭이 다른 출처로 가면 `storage` 쓰기는 `stale`, `cookies set`은 띄울 때의 주소로 고정된다(작업 4 `staging_copies_the_opened_file`, e2e `upload_shows_real_paths_and_uploads_the_bytes_shown`, `upload_origin_change_is_stale`, 작업 3 `cookie_without_url_is_pinned_to_the_asked_tab`, `storage_write_after_navigation_is_stale`).
4. **기다리는 명령이 끝나지 않는다.** 60초, 세션 닫기(닫기가 세션의 기록 자물쇠를 기다리다 막히면 안 된다), 데몬 종료, 브라우저 응답자 사라짐(제어 연결 끊김, 도메인이 창을 못 띄움) 어느 경우에도 명령은 끝나고 세션은 다음 명령을 받는다. CLI·MCP의 소켓 읽기 제한이 대기 시간을 덮는다(작업 2 `timeout_denies`, `cancel_session_ends_its_wait_and_no_other`, `exec_call_limit_includes_the_confirmation_wait`, 작업 3 e2e `closing_the_session_ends_a_pending_command`, `timeout_denies_and_frees_the_session`, 작업 5 `losing_the_control_link_unregisters_the_browser`, 작업 7 `a_daemon_restart_closes_open_dialogs`).
5. **창의 글로 사람을 속인다.** 에이전트가 고르는 세션 이름·파일 이름·쿠키 이름에 양방향 서식 문자(U+202E 등)나 제어 문자, 아주 긴 글을 넣어 다른 확장자나 다른 세션처럼 보이게 한다. 데몬과 브라우저가 각각 `\u{…}`로 바꾸고 자르며, 창은 언제나 세션 ID와 프로필을 보인다(작업 3 `details_escape_bidi_and_control_characters`, 작업 7 `bidi_text_is_shown_escaped`).

## 파일 구조

| 파일 | 할 일 | 작업 |
|---|---|---|
| `crates/pegboard-protocol/src/lib.rs` | `confirm_not_found`, 확인 관련 형식, `SessionInfo.allowed_for_session` | 1, 2 |
| `crates/pegboard-core/src/policy.rs` | `ClassDefault`, `Verdict`, `judge_action`, `validate` | 1 |
| `crates/pegboard-cli/src/main.rs`, `mcp/{mod.rs,tools.rs}` | 옵션 설명, 정책 줄, 거부 이유 표시 | 1, 3 |
| `crates/pegboard-core/src/confirm/mod.rs` | `ConfirmBroker`(대기 표, 응답자, 경쟁, 시간, 세션 허용) | 2 |
| `crates/pegboard-core/src/confirm/details.rs` | 보여 줄 내용, 글 거르기, state 출처 고정 | 3 |
| `crates/pegboard-core/src/confirm/staging.rs` | 업로드 사본 | 4 |
| `crates/pegboard-core/src/api.rs` | `confirm.subscribe` 알림 통로, 클라이언트 읽기 제한 | 2 |
| `crates/pegboard-core/src/service.rs` | `confirm.*` 메서드, 닫기·종료 때 취소, 사본 정리 | 2, 4 |
| `crates/pegboard-core/src/daemon.rs` | `run_command`의 묻기 | 3, 4 |
| `crates/pegboard-core/src/actionlog.rs` | `ActionEntry.confirm` | 3 |
| `crates/pegboard-core/src/{argv.rs,upload.rs,session.rs,proxy/server.rs}` | 절대 경로, 경로 검사, 묻기 모드의 프록시 폴더 | 4 |
| `crates/pegboard-core/src/bridge/domain.rs`, `browser.rs` | 브라우저 응답자 | 5 |
| `crates/pegboard-core/tests/e2e_confirm.rs` | 확인 실측(순정: API 응답자, 포크: 확인 창) | 3, 4, 7 |
| `browser/scripts/rebrand-strings.py`, `apply.sh`, `export.sh`, `budget.sh`, `browser/branding/*` | 제품 이름 문자열, 아이콘 | 6 |
| `crates/pegboard-core/tests/e2e_branding.rs` | 문자열·기본값 실측 | 6, 9 |
| `browser/pegboard/ui/{BUILD.gn,confirm_dialog.{h,cc},daemon_notice.{h,cc},display_text.{h,cc},test_hooks.{h,cc}}` | 확인 창, 알림 창, 글 거르기, 시험 훅 | 7, 10 |
| `browser/pegboard/devtools/{pegboard_protocol.pdl,pegboard_handler.{h,cc},BUILD.gn}` | 확인 메서드, 표시·숨기기 | 7, 8 |
| `browser/pegboard/browser_parts/{BUILD.gn,main_parts.{h,cc},profile_defaults.{h,cc}}` | 메인 파츠, 설정 기본값 | 9 |
| `browser/pegboard/bootstrap/daemon_launcher.{h,cc}`, `browser/patches/pegboard-bootstrap.patch` | 포기 알림, 메인 파츠 연결 | 9, 10 |
| `crates/pegboard-core/tests/{e2e_domain.rs,e2e_bootstrap.rs}` | 표시·숨기기, 포기 알림 | 8, 10 |

---

### Task 1: 정책 판정에 "묻기"

**Files:**
- Modify: `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-core/src/policy.rs`, `crates/pegboard-core/src/session.rs:276`(`allows_downloads`), `crates/pegboard-core/src/daemon.rs:166-177`, `crates/pegboard-cli/src/main.rs`(옵션 설명 146-160, 정책 줄 540), `crates/pegboard-cli/src/mcp/mod.rs`(서버 옵션 설명)
- Test: 같은 파일의 `#[cfg(test)]`

**Interfaces:**
- Produces:

```rust
// pegboard-protocol
pub mod codes { pub const CONFIRM_NOT_FOUND: &str = "confirm_not_found"; }
// SessionInfo: classes a person allowed for the rest of the session (not the policy).
#[serde(default)] pub allowed_for_session: Vec<ActionClass>,

// policy.rs (default_allowed는 지운다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ClassDefault { Allow, Ask, Deny }
pub fn class_default(class: ActionClass) -> ClassDefault;   // Upload, State → Ask; Clipboard → Deny; 나머지 Allow
#[derive(Debug)] pub enum Verdict { Allow, Ask, Deny(RpcError) }
pub fn judge_action(policy: &SessionPolicy, granted: &[ActionClass], class: ActionClass) -> Verdict;
```

규칙(설계 4.1 표): `deny`에 있으면 `Deny`; `allow`에 있으면 `Allow`; `granted`에 있으면 `Allow`; 아니면 `class_default`. `validate`: `--allow`는 `Ask`·`Deny` 분류에만, `--deny`는 `Allow`·`Ask` 분류에만 뜻이 있다(아니면 지금 문구의 `invalid_params`). `--allow upload`에 `--upload-dir`이 없으면 `"--allow upload needs --upload-dir (without --allow, an upload asks a person)"`. `--upload-dir`은 `--deny upload`와 함께면 `invalid_params`, 그 밖에는 된다.
- 이 작업에서 `run_command`는 `Verdict::Ask`를 지금처럼 거부한다(`policy_denied`, `data.reason = "no_responder"`, 메시지 "… needs a person's confirmation, which is not available yet"). 작업 3이 바꾼다.
- CLI 정책 줄은 `allow …; deny …; ask <Ask인 분류들>; …`. MCP 서버 옵션 설명도 같은 규칙.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn upload_and_state_ask_by_default()            // judge_action(&default, &[], Upload|State) == Ask; Clipboard → Deny; Eval → Allow
#[test] fn allow_and_deny_skip_the_question()           // allow:[State] → Allow; deny:[Upload] → Deny(policy_denied)
#[test] fn a_grant_turns_ask_into_allow_for_that_class_only() // granted [State]: State → Allow, Upload → Ask
#[test] fn a_grant_does_not_beat_deny()                 // deny:[State], granted [State] → Deny
#[test] fn deny_upload_and_deny_state_are_valid_now()
#[test] fn deny_clipboard_and_allow_eval_still_have_no_effect()
#[test] fn upload_dir_without_allow_upload_is_valid()   // 묻기 + 폴더
#[test] fn allow_upload_still_needs_upload_dir()        // 메시지에 "without --allow, an upload asks a person"
#[test] fn upload_dir_with_deny_upload_is_refused()
#[test] fn policy_line_shows_what_asks()                // main.rs: 기본 정책 → "ask upload,state"; --deny state → "ask upload"
```

- [ ] **Step 2: `cargo test -p pegboard-protocol -p pegboard-core policy:: && cargo test -p pegboard-cli policy_line` → 컴파일 실패 또는 FAIL 확인.**
- [ ] **Step 3: 구현한다.** `judge_action`을 부르는 곳(`daemon.rs`, `session.rs`의 `allows_downloads`, 시험들)을 새 모양으로 옮긴다. 기존 시험 중 "`--deny upload` has no effect"를 기대하던 것은 새 규칙으로 고친다.
- [ ] **Step 4: `cargo test --workspace`, clippy, fmt 통과.** `e2e`의 기존 시험 중 2a 문구("a person can allow it when creating the session")를 단언하는 것을 찾아(`grep -rn "a person can allow" crates`) 새 문구로 고치고 목록을 보고서에 적는다.
- [ ] **Step 5: 커밋** `feat: upload and state ask a person by default; --allow and --deny skip the question`

---

### Task 2: 확인 중개자와 코어 API

**Files:**
- Create: `crates/pegboard-core/src/confirm/mod.rs`
- Modify: `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-core/src/lib.rs`, `api.rs`(연결 루프, `Handler`, `Client`의 읽기 제한), `service.rs`(`dispatch`, `finish_session`, `shutdown`, `session.info`의 `allowed_for_session`)
- Test: `confirm/mod.rs`, `api.rs`의 `#[cfg(test)]`, `crates/pegboard-core/tests/api_server.rs`

**Interfaces:**
- Consumes: 작업 1의 `Verdict`, `codes::CONFIRM_NOT_FOUND`
- Produces:

```rust
// pegboard-protocol (모두 camelCase)
pub enum ConfirmAnswer { Approve, Deny, AllowForSession }            // "approve" | "deny" | "allowForSession"
pub enum ConfirmOutcome { Approved, ApprovedForSession, Denied, TimedOut, NoResponder, Cancelled, Stale }
pub enum ConfirmBy { Browser, Api, Session }                         // Session = 앞서 받은 "이 세션에서 계속 허용"
pub struct ConfirmDetail { pub label: String, pub value: String }
pub struct ConfirmRequired { pub confirm_id: String, pub session_id: SessionId, pub session_name: String,
    pub profile: String, pub class: ActionClass, pub details: Vec<ConfirmDetail>,
    pub offer_allow_for_session: bool, pub expires_at: String /* RFC 3339 */ }
pub struct ConfirmResolved { pub confirm_id: String, pub outcome: ConfirmOutcome, pub by: Option<ConfirmBy> }
pub struct ConfirmRespondParams { pub confirm_id: String, pub answer: ConfirmAnswer }

// confirm/mod.rs
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// PEGBOARD_TEST_CONFIRM_TIMEOUT_MS (testenv::var), else CONFIRM_TIMEOUT.
pub fn confirm_timeout() -> Duration;
pub struct ConfirmPlace { pub browser_context_id: String, pub target_ids: Vec<String> }
pub struct ConfirmAsk { pub session_id: SessionId, pub session_name: String, pub profile: String,
    pub class: ActionClass, pub details: Vec<ConfirmDetail>, pub offer_allow_for_session: bool,
    pub place: Option<ConfirmPlace> }
pub struct ConfirmDecision { pub confirm_id: String, pub outcome: ConfirmOutcome,
    pub by: Option<ConfirmBy>, pub waited: Duration }
#[async_trait] pub trait BrowserResponder: Send + Sync {
    async fn show(&self, req: &ConfirmRequired, place: &ConfirmPlace) -> anyhow::Result<()>;
    async fn cancel(&self, confirm_id: &str);
}
pub enum ConfirmNotice { Required(ConfirmRequired), Resolved(ConfirmResolved) }
pub struct ConfirmBroker { /* Mutex<표>, broadcast::Sender<ConfirmNotice>, 구독자 수 */ }
impl ConfirmBroker {
    pub fn new(timeout: Duration) -> Arc<ConfirmBroker>;
    pub async fn ask(&self, ask: ConfirmAsk) -> ConfirmDecision;
    pub fn respond(&self, p: &ConfirmRespondParams, by: ConfirmBy) -> Result<(), RpcError>;
    pub fn cancel_session(&self, id: &SessionId);    // 그 세션의 대기만 Cancelled
    pub fn cancel_all(&self);                        // 데몬 종료
    pub fn forget_session(&self, id: &SessionId);    // 세션 허용 목록 지우기(닫힌 뒤)
    pub fn granted(&self, id: &SessionId) -> Vec<ActionClass>;
    pub fn pending(&self) -> Vec<ConfirmRequired>;
    pub fn subscribe(&self) -> ConfirmSubscription; // drop하면 구독자 수가 준다
    pub fn set_browser(&self, responder: Option<Arc<dyn BrowserResponder>>); // None이면 대기 중 확인에서 브라우저가 빠진다
}
// api.rs
pub trait Handler { /* 기존 */ fn confirm_subscription(&self) -> Option<ConfirmSubscription> { None } }
```

동작(설계 4.2):
- `ask`: 응답자(브라우저가 있고 `place`가 있음, 또는 구독자 ≥ 1)가 없으면 기다리지 않고 `NoResponder`. 있으면 표에 넣고 `Required`를 알리고 브라우저의 `show`를 부른다. `show`가 실패하면 브라우저는 그 확인에서 빠지고, 남은 응답자가 없으면 `NoResponder`. 그다음 답, 취소, `timeout`, "응답자가 모두 사라짐" 중 먼저 오는 것으로 끝난다.
- 끝나면 표에서 빼고, 브라우저가 그 확인에 있었으면 `cancel`(답이 브라우저에서 왔어도 부른다. 창이 이미 닫혔으면 그냥 성공), `Resolved`를 알린다.
- `respond`: 표에 없으면 `confirm_not_found`. `AllowForSession`인데 `offer_allow_for_session`이 거짓이면 `invalid_params`이고 확인은 그대로 기다린다. `AllowForSession`이면 그 확인의 세션 ID의 허용 목록에 그 확인의 분류 하나를 더한다.
- `service.rs`: `confirm.subscribe`(응답 `{}` 뒤 그 연결은 알림 통로), `confirm.list` → `{pending}`, `confirm.respond`(`by = Api`). 알림 줄: `{"jsonrpc":"2.0","method":"confirm.required","params":…}`, `confirm.resolved`. `finish_session`은 **기록 자물쇠를 잡기 전에** `cancel_session`을, 끝에 `forget_session`을 부른다. `shutdown`은 처음에 `cancel_all`. `session.info`·`session.list`가 `allowed_for_session`을 채운다.
- `api.rs`: 알림 통로가 된 연결에는 쉬는 연결 끊기가 없고 요청을 더 읽지 않으며(받은 줄은 버린다), 상대가 끊으면 구독이 끝난다. 알림이 밀려 버려지면(`Lagged`) 그 연결을 닫는다(클라이언트는 다시 붙어 `confirm.list`를 본다). `Client`의 `command.exec` 읽기 제한은 `timeoutMs + 15초 + confirm_timeout()`.

- [ ] **Step 1: 실패하는 시험을 쓴다** (가짜 브라우저 응답자: `show` 결과를 고를 수 있고 `cancel`을 기록하는 구조체, `tokio::time::pause`).

```rust
#[tokio::test] async fn no_responder_denies_at_once()                  // 구독자 0, place None → NoResponder, waited < 10ms
#[tokio::test] async fn first_answer_wins_and_a_late_one_is_not_found()// Api가 approve → Approved/Api; 같은 ID로 Browser deny → confirm_not_found
#[tokio::test] async fn an_answer_only_resolves_its_own_confirmation() // A, B 대기 → A의 ID로 approve → B는 pending에 남는다
#[tokio::test] async fn allow_for_session_grants_that_session_and_class_only() // A에 State 허용 → granted(A)==[State], granted(B)==[]
#[tokio::test] async fn allow_for_session_needs_the_offer()            // offer false → invalid_params, 여전히 pending
#[tokio::test] async fn timeout_denies()                               // 61초 진행 → TimedOut, 브라우저 cancel 기록, Resolved 알림
#[tokio::test] async fn cancel_session_ends_its_wait_and_no_other()    // cancel_session(A) → A Cancelled, B pending
#[tokio::test] async fn cancel_all_ends_every_wait()
#[tokio::test] async fn a_browser_that_cannot_show_drops_out()         // show Err + 구독자 0 → NoResponder; 구독자 1 → pending
#[tokio::test] async fn losing_every_responder_ends_the_wait()         // 브라우저만 → set_browser(None) → NoResponder
#[tokio::test] async fn resolution_cancels_the_browser_window()        // Api 답에도 브라우저 cancel(id)이 불린다
#[test] fn confirm_ids_are_32_lowercase_hex()
#[test] fn the_timeout_hook_needs_test_hooks()                         // testenv gated: 훅 없이 변수만 있으면 60초
// api.rs / tests/api_server.rs
#[tokio::test] async fn subscribe_streams_required_and_resolved()
#[tokio::test] async fn a_subscribed_connection_is_not_idle_closed()   // 11초 동안 열려 있다
#[tokio::test] async fn respond_unknown_is_confirm_not_found()
#[test] fn exec_call_limit_includes_the_confirmation_wait()           // timeoutMs 30000 → 30 + 15 + 60초
```

- [ ] **Step 2: FAIL 확인 → Step 3: 구현.**
- [ ] **Step 4: `cargo test --workspace`, clippy, fmt.**
- [ ] **Step 5: 커밋** `feat: a confirmation broker in the daemon; confirm.subscribe, confirm.list, confirm.respond`

---

### Task 3: 명령 경로의 묻기와 `state`

**Files:**
- Create: `crates/pegboard-core/src/confirm/details.rs`, `crates/pegboard-core/tests/e2e_confirm.rs`
- Modify: `daemon.rs`(`run_command_with_hook`), `actionlog.rs`(`ActionEntry`), `service.rs`(중개자를 `run_command`에 넘김), `crates/pegboard-cli/src/main.rs`(거부 이유 표시), `crates/pegboard-cli/src/mcp/tools.rs`(결과 문구), `crates/pegboard-cli/tests/{e2e_policy.rs,e2e_mcp.rs}`

**Interfaces:**
- Consumes: 작업 1 `Verdict`, 작업 2 `ConfirmBroker::ask`, `ConfirmAsk`, `ConfirmDecision`
- Produces:

```rust
// confirm/details.rs
/// Control, format and bidi characters become `\u{…}`; at most `max_chars` (then '…').
pub fn display_text(s: &str, max_chars: usize) -> String;
pub const SHOWN_VALUE_CHARS: usize = 200;
pub const DETAIL_VALUE_CHARS: usize = 1000;
pub enum StatePin { CookieUrl(String) /* --url to insert */, Origin(String) /* must still match */, None }
/// What a state command shows and what pins it, given the tab it would act on.
pub fn state_details(argv: &[String], tab_url: Option<&str>) -> (Vec<ConfirmDetail>, StatePin);
// actionlog.rs
pub struct ConfirmLog { pub outcome: ConfirmOutcome, pub by: Option<ConfirmBy>, pub wait_ms: u64 }
// ActionEntry: #[serde(default, skip_serializing_if = "Option::is_none")] pub confirm: Option<ConfirmLog>
```

`run_command`의 순서: `argv::check` → `judge_action(policy, broker.granted(id), class)` → 기존 `require_*` 검사 → 주소 판정 → (업로드 해석, 작업 4) → **`Ask`면**: 탭(`runner.bound_target` 또는 세션의 유일한 탭)과 그 주소를 보고 `state_details` → `broker.ask` → 결과에 따라 거부(`policy_denied`, `data {reason, confirmId}`, 동작 기록에 `confirm`) 또는 진행. `Allow`가 `granted`에서 왔으면 `confirm = {outcome: Approved, by: Session}`. 진행 직전: `StatePin::CookieUrl(u)`면 `--url u`를 실행할 단어에 넣는다(`--url`·`--domain`이 없던 경우만), `StatePin::Origin(o)`면 탭의 지금 출처가 `o`가 아니면 `stale`로 거부. 성공 결과에는 `"confirmed": {"by": …}`.
- 보여 줄 내용(설계 4.4): `세션`, `프로필`은 중개자가 아니라 창이 따로 보이므로 `details`에는 분류별 줄만: 쿠키 `이름`, `값`(`SHOWN_VALUE_CHARS`), `도메인`/`경로`/`URL`, 플래그; 저장소 `출처`, `종류`, `키`, `값`; `clear`는 `출처`와 "전체 삭제". 모든 값은 `display_text`를 거친다. 세션 이름도 `display_text(name, 60)`.
- 메시지: `denied` "a person denied this {class} command", `timed_out` "no answer within {n} s; the {class} command was denied", `no_responder` "the {class} command needs a person's confirmation; confirmation needs the Pegboard browser (or a client of confirm.subscribe)", `cancelled` "the session closed (or the daemon stopped) while the {class} command waited for a person", `stale` "the page changed after a person approved; the {class} command was not run".
- CLI는 `policy_denied`의 `data.reason`을 메시지 끝에 `(reason: …)`로 보인다. 종료 코드는 지금 `policy_denied`의 것 그대로.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
// details.rs
#[test] fn details_escape_bidi_and_control_characters()  // "a\u{202E}fdp.exe\n" → "a\\u{202e}fdp.exe\\u{a}"; U+2066..2069, U+200E/F, U+061C, U+FEFF도
#[test] fn details_cap_values_at_200_chars()
#[test] fn cookie_without_url_is_pinned_to_the_asked_tab()  // ["cookies","set","n","v"], tab "https://a.test/x" → CookieUrl("https://a.test/x")
#[test] fn cookie_with_domain_is_not_repinned()
#[test] fn storage_set_pins_the_origin()                     // Origin("https://a.test")
#[test] fn a_tabless_state_command_shows_no_origin_and_pins_none() // tab None → 출처 줄 "(탭 없음)", StatePin::None
// daemon.rs
#[test] fn denial_reasons_are_in_error_data()
#[test] fn a_denied_command_logs_its_outcome_without_args()  // 기록 줄: command "cookies", args 가림, confirm.outcome "denied"
#[test] fn storage_write_after_navigation_is_stale()          // Origin pin과 다른 지금 출처 → stale, 실행 없음(가짜 runner가 호출을 기록)
```

`tests/e2e_confirm.rs` (`#[ignore]`, 순정 구성, 시험 안에서 소켓에 `confirm.subscribe` 연결을 열어 답한다):

```rust
#[tokio::test] #[ignore] async fn state_waits_for_an_api_answer_and_runs()          // storage local set → confirm.required(내용에 출처·키) → approve → 페이지의 localStorage에 값
#[tokio::test] #[ignore] async fn state_denied_by_api_is_denied_with_reason_denied()// 값이 없다, 기록 줄 confirm.by "api"
#[tokio::test] #[ignore] async fn no_responder_denies_at_once_with_the_browser_hint()// 구독 없이 → 1초 안에 no_responder
#[tokio::test] #[ignore] async fn timeout_denies_and_frees_the_session()            // PEGBOARD_TEST_CONFIRM_TIMEOUT_MS=2000, 답 없음 → timed_out, 다음 `get url`이 바로 된다
#[tokio::test] #[ignore] async fn closing_the_session_ends_a_pending_command()      // 대기 중 session.close → 명령은 5초 안에 cancelled, 닫기도 5초 안에 끝
#[tokio::test] #[ignore] async fn allow_for_session_skips_the_next_question_only_in_that_session()
#[tokio::test] #[ignore] async fn an_answer_for_session_a_does_not_release_session_b()
```

`pegboard-cli`: `e2e_policy::cli_state_shows_who_denied`(stderr에 `reason: denied`), `e2e_mcp::mcp_state_waits_for_a_person`(구독자가 approve → 도구 결과 성공), `e2e_mcp::mcp_has_no_confirmation_tool`(도구 목록과 `--tools all` 어디에도 `confirm` 없음, 브라우저 없이 도는 시험).

- [ ] **Step 2: FAIL → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt.**
- [ ] **Step 5: 컨트롤러가 맥미니에서 순정 구성으로 돌린다.** `PEGBOARD_TEST_HOOKS=1 cargo test -p pegboard-core --test e2e_confirm -- --ignored --test-threads=1`, `cargo test -p pegboard-cli --test e2e_policy --test e2e_mcp -- --ignored --test-threads=1`. 기대: PASS. 변형: `finish_session`의 `cancel_session` 호출을 빼면 `closing_the_session_ends_a_pending_command`가 실패한다.
- [ ] **Step 6: 커밋** `feat: state commands wait for a person's answer; the outcome is logged`

---

### Task 4: 업로드의 묻기 (보안 검토 대상)

**Files:**
- Create: `crates/pegboard-core/src/confirm/staging.rs`
- Modify: `argv.rs`(`UploadFile`), `upload.rs`, `daemon.rs`(`resolve_upload`, `resolve_names`, `hide_paths`), `session.rs:409-418`(묻기 모드의 프록시 폴더), `proxy/server.rs`(`set_upload_dirs` 그대로 사용), `service.rs`(사본 정리: 세션 닫기, 데몬 시작), `paths.rs`(`session_confirm_dir`)
- Test: 각 파일 `#[cfg(test)]`, `tests/e2e_confirm.rs`(추가)

**Interfaces:**
- Consumes: 작업 3의 묻기 흐름, `display_text`
- Produces:

```rust
// upload.rs
pub const MAX_UPLOAD_FILE_BYTES: u64 = 200 << 20;
pub const MAX_UPLOAD_FILES: usize = 16;
/// An absolute path an asking session may upload: no `..` component, the last component not a
/// symbolic link, a regular file whose real path is outside every `forbidden` directory
/// (pegboard's home, the browser's user data directory), at most MAX_UPLOAD_FILE_BYTES.
pub fn check_absolute(path: &str, forbidden: &[PathBuf]) -> Result<PathBuf /* real */, String>;
// paths.rs
pub fn session_confirm_dir(&self, id: &SessionId) -> PathBuf;   // <home>/sessions/<id>/confirm
// confirm/staging.rs
pub struct StagedFile { pub real: PathBuf, pub staged: PathBuf, pub name: String, pub size: u64 }
pub struct Staged { pub dir: PathBuf, pub files: Vec<StagedFile> }   // Drop: dir를 지운다
/// Opens each file with O_NOFOLLOW, checks the open file (fstat: regular, size limit) and copies
/// from that descriptor into `dir/<name>` (0700 dir, 0600 files).
pub fn stage(files: &[(PathBuf /* real */, String /* name */)], dir: &Path) -> std::io::Result<Staged>;
pub fn remove_leftovers(confirm_root: &Path);
```

규칙(설계 4.3 표 그대로):
- `argv`: `upload <sel> <file>...`의 `<file>`은 맨 이름(지금 규칙) 또는 `/`로 시작하는 절대 경로(NUL, `..` 구성 요소 없음). 상대 경로(`a/b`)는 지금처럼 거부.
- 데몬: 정책이 `Allow`(`--allow upload`)거나 `--upload-dir`이 있으면 절대 경로는 거부("this session uploads only by file name from its upload directory"). 묻기 + 폴더 없음이면 맨 이름은 세션 파일 디렉터리에서만, 절대 경로는 `check_absolute`(금지 목록: `paths.home`, 브라우저 사용자 데이터 디렉터리).
- 묻기면 확인을 띄우기 전에 `stage`(디렉터리 `session_confirm_dir(id)/<confirmId>`), 보여 줄 내용: 파일마다 `파일: <실제 경로 display_text>`, `크기: <사람이 읽는 크기>`, 그리고 `대상 페이지: <탭 출처>`. 승인되면 실행 단어의 경로를 사본 경로로 바꾸고, 프록시 폴더를 `[사본 폴더]`로 두고 실행, 끝나면(성공·실패·시간 초과 모두) 프록시 폴더를 다시 `[]`로 하고 `Staged`를 버린다. 탭 출처가 바뀌었으면 `stale`.
- 묻기 모드의 세션은 만들 때 프록시 폴더가 `[]`다(`--upload-dir`이 있어도). `Allow` 모드와 `allowForSession`을 받은 뒤(그때 `[upload_dir, files]`로 바꾼다)는 지금 그대로.
- `offer_allow_for_session` = `upload_dir.is_some()`.
- `hide_paths`는 원래 경로, 실제 경로, 사본 경로를 모두 이름으로 바꾼다.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn upload_takes_bare_names_or_absolute_paths()      // argv: "/tmp/a.txt" 통과, "a/b", "../a", "/x/../y", "" 거부
#[test] fn absolute_paths_are_refused_when_the_session_has_an_upload_dir()
#[test] fn absolute_path_checks()                           // 마지막이 링크, 디렉터리, 홈 안, 사용자 데이터 안(링크로 들어가는 경우 포함), 200 MiB + 1(set_len), 없는 파일 → 거부
#[test] fn seventeen_files_are_refused()
#[test] fn staging_copies_the_opened_file()                 // stage 뒤 원본을 고치거나 링크로 바꿔도 사본은 처음 내용
#[test] fn staging_refuses_a_file_swapped_for_a_link_before_open() // O_NOFOLLOW: 검사와 열기 사이에 링크가 된 경우 거부
#[test] fn staged_dir_goes_when_dropped()
#[test] fn leftover_staging_is_removed_at_start()
#[test] fn ask_mode_proxy_takes_no_folder_outside_a_command() // session.rs: 묻기+--upload-dir 세션 만든 직후 scope.upload_dirs == []
#[test] fn allow_for_session_is_offered_only_with_an_upload_dir()
#[test] fn hide_paths_covers_staged_paths()
```

`tests/e2e_confirm.rs`(`#[ignore]`, 순정):

```rust
#[tokio::test] #[ignore] async fn upload_shows_real_paths_and_uploads_the_bytes_shown()
// 폴더 없는 묻기 세션, 파일 입력 페이지(FileReader로 내용을 시험 서버에 보낸다). 링크 디렉터리를 거친 절대 경로로 업로드 →
// confirm.required의 파일 줄은 실제 경로 → 그 뒤 원본 내용을 바꾸고 approve → 서버가 받은 내용은 처음 내용, 파일 이름은 원래 이름.
#[tokio::test] #[ignore] async fn an_unapproved_file_cannot_be_set_through_the_session_endpoint()
// 묻기+--upload-dir 세션의 엔드포인트에 직접 붙어 DOM.setFileInputFiles(폴더 안 파일) → -32000 거부.
#[tokio::test] #[ignore] async fn upload_origin_change_is_stale()
// 대기 중에 탭이 다른 출처로 간다(페이지 스크립트의 지연 이동) → approve → stale, 서버는 아무것도 받지 않는다.
#[tokio::test] #[ignore] async fn upload_with_an_upload_dir_offers_allow_for_session_and_then_stops_asking()
```

- [ ] **Step 2: FAIL → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt.**
- [ ] **Step 5: 컨트롤러가 맥미니 순정 구성으로 `e2e_confirm` 전체와 기존 업로드 시험(`grep -rln upload crates/*/tests`)을 돌린다.** 기대: PASS. 변형: `stage`를 건너뛰고 원본 경로로 실행하면 `upload_shows_real_paths_and_uploads_the_bytes_shown`이 실패한다. 묻기 모드 세션 만들 때 `[upload_dir, files]`를 넣으면 `an_unapproved_file_cannot_be_set_through_the_session_endpoint`가 실패한다.
- [ ] **Step 6: 보안 검토자가 본다**(경로 검사, 사본, 프록시 폴더의 시간 창, `hide_paths`). 지적을 반영한 뒤 커밋 `feat: uploads ask a person, who sees the real paths; the approved copy is what uploads`

---

### Task 5: 브라우저 응답자 (Rust, 가짜 브라우저로)

**Files:**
- Modify: `crates/pegboard-core/src/bridge/domain.rs`, `browser.rs`(`fork_parts`, `ForkParts`), `service.rs`(시작 때 `set_browser`), `daemon.rs`(묻기의 `place`)
- Test: 같은 파일 `#[cfg(test)]`

**Interfaces:**
- Consumes: 작업 2 `BrowserResponder`, `ConfirmRequired`, `ConfirmPlace`, `ConfirmBroker::{respond,set_browser}`
- Produces (작업 7의 C++이 이 모양을 구현한다):

| Rust | CDP | 응답 |
|---|---|---|
| `DomainResponder::show(req, place)` | `Pegboard.requestConfirmation {confirmId, browserContextId, targetIds, sessionId, sessionName, profileDirectory, actionClass, details:[{label, value}], offerAllowForSession}` | `{}`. 창을 띄울 수 없으면 `-32000` |
| `DomainResponder::cancel(id)` | `Pegboard.cancelConfirmation {confirmId}` | `{}` |
| 이벤트 | `Pegboard.confirmationAnswered {confirmId, answer: "approve"\|"deny"\|"allowForSession"}` | → `broker.respond(.., ConfirmBy::Browser)` |

```rust
pub struct DomainResponder { conn: CdpConn }
impl DomainResponder { pub fn new(control: CdpConn) -> DomainResponder; }
pub fn confirmation_answer(e: &CdpEvent) -> Option<ConfirmRespondParams>;
```

- 포크(`ForkLaunched`, `Hosted`)에서 데몬은 시작 때 `broker.set_browser(Some(DomainResponder))`, 제어 연결의 이벤트 감시(`follow_profiles`와 같은 수신기 또는 하나 더)에서 `confirmationAnswered`를 넘긴다. 물리 연결이 끝나면 `set_browser(None)`. 순정은 `None` 그대로.
- `place`: 세션의 컨텍스트(`AgentContexts::context_of(profile)`)와 세션의 타깃 목록. 컨텍스트를 모르면 `place = None`(브라우저는 응답자가 아니다).
- 늦은 답(`confirm_not_found`)과 모양이 틀린 이벤트는 `tracing::debug`로만 남긴다.
- **구현에서 정한 것** (처음 제안은 위 표와 `tracing::debug`):
  - `ConfirmPlace`에 `profile_directory`를 더했다(`requestConfirmation`의 `profileDirectory`는 세션 이름이 아닌 실제 디렉터리 `PegboardAgent` / `PegboardAgent-<이름>`). `targetIds`는 세션의 탭이고, agent-browser가 붙은 탭(명령이 닿는 탭)이 세션 탭이면 맨 앞이다. 세션 탭이 아닌 것은 넣지 않는다. 탭이 없어도 컨텍스트를 알면 자리는 있다(그 프로필의 창).
  - 함수: `domain::confirm_place(agents, profile_dir, acted_on, targets) -> Option<ConfirmPlace>`, `SessionManager::confirm_place(id, acted_on)`(순정이면 `None`), `domain::serve_confirmations(broker, conn, link) -> JoinHandle`(이벤트 수신기를 먼저 만들고 `set_browser(Some)`, 링크가 끝나면 `set_browser(None)`), `Browser::serve_confirmations(broker) -> Option<JoinHandle>`(순정 `None`). `Daemon::assemble`이 부른다.
  - 이벤트는 제어 연결 자신의 세션(`sessionId` 없음) 것만, `params`는 정확히 `{confirmId, answer}`(다른 필드가 있으면 버린다). 답은 `broker.respond(.., ConfirmBy::Browser)`이고, 브라우저가 받지 않은(또는 이미 빠진) 확인은 중개자가 `confirm_not_found`로 거절한다.
  - 워크스페이스에 `tracing`이 없어 늦은 답과 틀린 이벤트는 기록 없이 버린다. 이벤트 큐가 넘쳐(`Lagged`) 잃은 답은 그 확인을 시간 초과(거부)로 끝낸다(닫힌 쪽으로 실패).

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[tokio::test] async fn request_sends_the_shape_the_domain_expects()
#[tokio::test] async fn an_answer_event_reaches_the_broker_as_by_browser()
#[tokio::test] async fn a_refused_request_drops_the_browser_out()      // -32000 → 구독자 없으면 NoResponder
#[tokio::test] async fn losing_the_control_link_unregisters_the_browser() // 가짜 브라우저 끊기 → 대기 중 확인 NoResponder, 3초 안
#[tokio::test] async fn answers_for_unknown_ids_are_ignored()
#[tokio::test] async fn an_unknown_context_gives_no_place()
#[test] fn stock_has_no_browser_responder()
```

- [ ] **Step 2: FAIL → Step 3: 구현 → Step 4: `cargo test --workspace`, clippy, fmt, 맥미니 순정 `e2e_confirm` 퇴행 없음.**
- [ ] **Step 5: 커밋** `feat: the fork's Pegboard domain answers confirmations (daemon side)`

---

### Task 6: 제품 이름 문자열과 아이콘 (Chromium 트리)

빌드가 오래 걸릴 수 있으므로 C++ 작업 중 가장 먼저, 작업 1 이후 언제든 병행한다.

**Files:**
- Create: `browser/scripts/rebrand-strings.py`, `browser/scripts/test_rebrand_strings.py`(작은 `.grd`/`.xtb` 예제), `crates/pegboard-core/tests/e2e_branding.rs`
- Modify: `browser/scripts/{apply.sh,export.sh,budget.sh}`, `browser/branding/icon.swift`, `browser/branding/mac/{app.icns,Assets.car}`, `browser/README.md`

**Interfaces:**
- Produces:
  - `rebrand-strings.py <src>`: 대상 `chrome/app/chromium_strings.grd`, `components/components_chromium_strings.grd`와 각 `<translations>`가 가리키는 `.xtb`. 메시지 본문의 제품 이름 `Chromium`을 `Pegboard`로(제외: `The Chromium Authors`, `Chromium OS`/`ChromiumOS`, `chromium.org`를 포함한 주소, 오픈 소스 고지 메시지. 제외 목록은 스크립트 맨 위 상수). 바뀐 메시지마다 `<src>/tools/grit`의 지문 함수로 옛 ID와 새 ID를 계산해 `.xtb`의 `id`와 번역 본문을 같이 바꾼다. 다시 돌려도 바뀌는 것이 없다(멱등). 출력: 바꾼 메시지 수, 제외한 수, 파일마다 바꾼 번역 수.
  - `apply.sh`: 패치를 모두 얹은 뒤 `rebrand-strings.py`를 돌리고 커밋 제목 `rebrand-strings (generated)`로 남긴다.
  - `export.sh`: 그 커밋을 패치로 뽑지 않는다(제목으로 가린다).
  - `budget.sh`: 손 패치 합계와 생성 커밋 합계를 따로 낸다.
  - 아이콘: 설계 4.7.

- [ ] **Step 1: `python3 browser/scripts/test_rebrand_strings.py --src <트리>`를 쓴다**: 예제 grd·xtb에서 본문, 번역, ID가 바뀌고, 제외 목록이 그대로이고, 두 번 돌려도 같다. `shellcheck browser/scripts/*.sh` 통과.
- [ ] **Step 2: 시험을 쓴다** (`e2e_branding.rs`, `#[ignore]`, `PEGBOARD_TEST_BROWSER=fork:…`일 때만. 파이프 판 `RawLink`로 사람 탭을 `chrome://settings/help`로 보내고 본문 글을 읽는다):

```rust
#[tokio::test] #[ignore] async fn about_page_says_pegboard_in_english()  // --lang=en-US: "About Pegboard" 있음, 제품 이름 "Chromium" 없음("The Chromium Authors"는 있어도 된다)
#[tokio::test] #[ignore] async fn about_page_says_pegboard_in_korean()   // --lang=ko: "Pegboard 정보"(156의 한국어 문구에 맞춰 단언), 영어 문구로 돌아가지 않음
```

- [ ] **Step 3: 트리에서 `PEGBOARD_APPLY_FORCE=1 apply.sh` → `build.sh`(떼어 돌림). 걸린 시간과 다시 컴파일된 단계 수를 잰다.** 아이콘을 다시 그리고 `make-icons.sh`.
- [ ] **Step 4: `package-dev.sh`, 컨트롤러가 맥미니에서 `e2e_branding`을 돌린다.** 기대: PASS. 변형: 스크립트의 `.xtb` ID 바꾸기를 끄고 다시 빌드하면 `about_page_says_pegboard_in_korean`이 실패한다(한국어 문구가 영어로 돌아간다). 아이콘은 Finder와 Dock 스크린샷을 보고서에 붙인다.
- [ ] **Step 5: `budget.sh` 기록(손 패치 변화 0, 생성 커밋 줄 수), 커밋** `build(browser): product name strings say Pegboard (generated); a real app icon`

---

### Task 7: 확인 창과 `Pegboard` 도메인의 확인 메서드 (C++, 보안 검토 대상)

**Files:**
- Create: `browser/pegboard/ui/{BUILD.gn,confirm_dialog.h,confirm_dialog.cc,display_text.h,display_text.cc,test_hooks.h,test_hooks.cc}`
- Modify: `browser/pegboard/devtools/{pegboard_protocol.pdl,pegboard_handler.h,pegboard_handler.cc,BUILD.gn}`, `browser/pegboard/BUILD.gn`, `crates/pegboard-core/src/browser.rs`(`PEGBOARD_TEST_BROWSER_NO_HOOKS`), 필요하면 `browser/patches/pegboard-cdp-domain.patch`(의존·`visibility`), `crates/pegboard-core/tests/{e2e_confirm.rs,e2e_domain.rs}`

**Interfaces:**
- Consumes: 작업 5의 CDP 표(이름·매개변수·응답 그대로)
- Produces:

```cpp
namespace pegboard {
// Text an agent may choose, as plain label text: control, format and bidi characters become
// \u{…} (the daemon's display_text rule), at most `max_chars` then U+2026.
std::u16string DisplayText(std::string_view utf8, size_t max_chars);

enum class ConfirmAnswer { kApprove, kDeny, kAllowForSession };
struct ConfirmRequest {  // requestConfirmation's parameters
  std::string confirm_id, session_id, session_name, profile_directory, action_class;
  std::vector<std::pair<std::string, std::string>> details;
  bool offer_allow_for_session;
};
// A browser-modal dialog on `browser`'s window, shown inactive. `done` runs once with the
// answer (closing it any other way is kDeny); never after Close().
class ConfirmDialog {
 public:
  static std::unique_ptr<ConfirmDialog> Show(BrowserWindowInterface* browser, ConfirmRequest,
                                             base::OnceCallback<void(ConfirmAnswer)> done);
  void Close();  // no answer
};
namespace test_hooks {
bool Enabled();                                     // PEGBOARD_TEST_HOOKS in the browser's own env
void LogDialog(const base::Value::Dict& entry);     // PEGBOARD_TEST_DIALOG_LOG, one JSON line
std::optional<std::string> AnswerFor(const std::string& session_name); // PEGBOARD_TEST_DIALOG_ANSWER
base::TimeDelta AnswerAfter();                      // PEGBOARD_TEST_DIALOG_ANSWER_AFTER_MS, default 1100 ms
}
}  // namespace pegboard
```

규칙(설계 4.5):
- 핸들러: `requestConfirmation`은 주인 세션만(아니면 `-32601`), 컨텍스트·탭이 에이전트 것이 아니면 `-32000`, `confirmId`는 32자리 소문자 16진수이고 열린 것과 겹치지 않음, `actionClass` ∈ {`upload`, `state`}, `details` ≤ 40, 값 ≤ 1,000자, 열린 창 ≤ 16. 창은 `targetIds` 중 처음 찾은 탭의 창, 없으면 그 컨텍스트의 가장 최근 일반 창, 없으면 `-32000 "no window"`. `cancelConfirmation`은 없으면 성공. 답과 창 닫힘은 주인 세션에만 `confirmationAnswered`. 주인이 떨어지거나 바뀌면 열린 창을 모두 `Close()`(이벤트 없음). 창마다 75초 안전 시간(`Close()`).
- 창: 웹 콘텐츠 밖의 브라우저 모달(156의 `ui::DialogModel` + `constrained_window` 브라우저 모달 함수, 정확한 이름은 트리에서 찾는다), `ShowInactive`, 기본 버튼·Esc·닫기 = 거부, 버튼 1초 잠금(잠긴 동안의 누름은 무시하고 훅 기록에 `"ignored": "locked"`), 문구는 설계 4.5. 앱이 활성이 아니면 `[NSApp requestUserAttention:NSInformationalRequest]` 한 번.
- 작업 5가 보내는 모양 그대로(`crates/pegboard-core/src/bridge/domain.rs` `request_params`와 시험 `request_sends_the_shape_the_domain_expects`): `details`는 데몬이 정한 순서다. 업로드는 `호스트`(맨 앞, 왼쪽을 자른 호스트), `대상 페이지`(출처), 파일마다 `파일`(실제 경로 전체)과 `크기`, 끝에 `읽을 수 있는 쪽`(허용하면 에이전트와 그 출처가 내용을 읽는다); 쿠키는 `호스트`(맨 앞), `URL`·`도메인`(있을 때), `경로`, `플래그` 줄이 있다. 창은 `details`의 순서를 바꾸지 않고 하나도 빼지 않는다. `offerAllowForSession`이 거짓이면 셋째 버튼이 없다. `sessionName`은 이미 데몬이 거른 글이다(브라우저가 한 번 더 거른다).
- **경로 표시**: `파일` 값(그리고 쿠키의 `경로`)은 **LTR로 격리해서**(`base::i18n::WrapStringWithLTRFormatting` 또는 라벨의 방향을 LTR로 고정. 격리 문자는 `DisplayText`로 거른 **뒤에** 창이 붙인다) **자르지 않고** 보인다: 줄 바꿈(`SetMultiLine`)으로 다 보이고 가운데·끝 생략(`ELIDE_*`)은 쓰지 않는다. 데몬은 1,000자를 넘거나 거르면 달라지는 경로는 묻기 전에 거부하므로, `DisplayText(value, 1000)`은 경로를 바꾸지 않아야 한다. 오른쪽에서 왼쪽 글자(히브리어 등)가 든 경로도 사람 창이 RTL 언어여도 디렉터리 순서가 뒤집혀 보이지 않아야 한다.
- 시험 훅: `Enabled()`가 거짓이면 다른 훅 함수는 아무것도 하지 않는다. 기록 한 줄: `{"kind":"confirm","confirmId","profile","lines":[창에 보인 글 전부],"buttons":[…],"keyWindowProfileBefore","keyWindowProfileAt0","keyWindowProfileAt200ms","appActiveBefore","appActiveAt200ms","pressed","ignored"?}`. `PEGBOARD_TEST_DIALOG_ANSWER` = `<세션 이름>=<답>` 쉼표 목록, `*`는 나머지(답: `approve`, `deny`, `allowForSession`, `none`). 훅은 진짜 버튼 처리 함수를 부른다.

- **구현에서 정한 것** (작업 7):
  - **창 모양**: 처음 제안은 브라우저 모달(`constrained_window::CreateBrowserModalDialogViews`, `ModalType::kWindow`)이었다. 맥에서 그것은 에이전트 창의 시트이고, AppKit이 시트를 띄우면서 키 창으로 만든다. 실측: 사람의 `Default` 창이 키 창이던 상태에서 띄운 직후는 `Default`, 200ms 뒤 `sheet:PegboardAgent`(초점을 가져갔다). `ShowInactive`도 시트에는 효과가 없다(브리지가 시트면 보이기 상태를 보지 않고 `beginSheet`를 부른다). 그래서 **모달이 아닌 자식 창**(`ModalType::kNone`, 부모는 에이전트 창의 위젯, `views::DialogDelegate::CreateDialogWidget`, 에이전트 창 위쪽 가운데)으로 바꿨다. 여전히 브라우저 UI이고 웹 콘텐츠 밖이다. `ShowInactive`면 부모 위에 놓일 뿐 키 창도 앱 활성도 바뀌지 않는다. 대가: 창이 떠 있는 동안 사람이 에이전트 창의 탭을 쓸 수 있다(막지 않는다).
  - 한 창에는 확인 창을 하나씩만 띄우고, 같은 창의 다른 요청은 요청 순서로 기다린다(앞 것이 끝나면 뜬다). 기다리는 동안에도 16개 한도와 75초 안전 시간에 든다. 기다리던 것의 창이 사라졌으면 `deny`.
  - 탭: `targetIds`는 **모두** 그 프로필의 에이전트 탭이어야 한다(닫힌 탭, 사람 탭, 다른 프로필의 탭 하나라도 있으면 `-32000`). 창은 닫히는 중이 아닌 첫 탭의 창, 탭이 없으면 그 프로필의 가장 최근 일반 창. `profileDirectory`가 컨텍스트의 실제 디렉터리와 다르면 `-32000`. `sessionId`는 ASCII 글자·숫자·`_`·`-` 64자 이하, 라벨은 40자 이하(정하지 않았던 값).
  - `targetIds[0]`의 탭이 닫히면(`TabInterface::RegisterWillDetach`, `kDelete`) `deny`. 창이 닫혀 대화상자 위젯이 닫혀도 `deny`.
  - `DisplayText`는 데몬 규칙과 같되 **역슬래시를 두 번 바꾸지 않는다**: 데몬이 이미 `\\`로 바꿔 보내므로, 브라우저가 다시 바꾸면 `\u{202e}`가 `\\u{202e}`로 보인다. 데몬이 보낸 글에는 아무것도 바뀌지 않는다(멱등). 데몬을 거치는 시험으로는 `DisplayText` 변형을 잡을 수 없어 `bidi_text_is_shown_escaped`는 도메인에 직접 보내는 `e2e_domain` 시험이다.
  - 경로(`파일`, `경로`)는 라벨 방향을 LTR로 고정하고(`DIRECTIONALITY_FORCE_LTR`) `/`마다 뒤에 U+200E를 넣는다. 방향 고정만으로는 오른쪽에서 왼쪽 글자인 디렉터리 둘이 `/`로 이어질 때 한 덩어리로 뒤집힌다. 세션 이름은 FSI…PDI, ID·프로필·출처는 LRI…PDI로 둘러싼다. 이 표시는 창이 붙인 것이라 시험은 `common::without_marks`로 빼고 비교한다. 값은 줄 바꿈(`SetAllowCharacterBreak`, `NO_ELIDE`)으로 다 보이고, 줄이 많으면 상세 줄 부분이 스크롤된다(최대 높이 320).
  - 버튼은 1초 동안 비활성이고, 잠긴 동안의 누름은 무시한다(`ignored: "locked"`). 확인 창이 스스로 닫히는 일은 없다(누름 처리 함수가 정한다). Esc와 기본 버튼(Return)은 거부의 처리 함수로 가고, 잠금도 똑같이 받는다.
  - 기록: 끝날 때 `{"kind":"confirm", …, "sessionId", "defaultButton", "appActiveAt0", "attention", "pressed", "by"("button"|"hook"), "closed"("answered"|"cancelled"|"owner_gone"|"safety_time"|"tab_closed"|"window_closed"|"destroyed")}` 한 줄, 띄울 때 `{"kind":"confirm_shown","confirmId","profile"}` 한 줄. 답하는 훅은 창에 없는 버튼(내놓지 않은 "이 세션에서 계속 허용")은 누르지 않는다.
  - 시험 배치: 도메인 수준의 검사(주인, 컨텍스트, 한도, 내용, 이스케이프, 잠금, 에이전트 입력, 탭 닫힘, 주인 바뀜, 초점, 프록시)는 시험이 띄운 포크에 직접 보내는 `e2e_domain`에, 데몬을 거치는 것(세션의 창, 허용·거부, 세션 허용, 두 세션, 훅의 문)은 `e2e_confirm`에 두었다. `agent_input_cannot_answer_the_dialog`는 세션 엔드포인트 대신 파이프의 다른 브라우저 세션(프록시보다 넓은 권한)으로 그 탭에 입력을 보낸다. 초점 시험은 두 전제를 다 본다: 사람 창이 키 창이고 앱이 앞(키 창 `Default` 그대로), 다른 앱이 앞(키 창 없음 그대로, 앱 비활성 그대로, Dock 튕김 요청).

- [ ] **Step 1: 시험을 쓴다** (`e2e_confirm.rs`의 포크 판, `#[ignore]`, `PEGBOARD_TEST_BROWSER=fork:…`일 때만. 데몬이 포크를 `ForkLaunched`로 띄우고 브라우저 환경에 `PEGBOARD_TEST_HOOKS=1`, `PEGBOARD_TEST_DIALOG_LOG`, `PEGBOARD_TEST_DIALOG_ANSWER`를 준다. 사람 창은 시험이 먼저 연 `Default` 창).

```rust
#[tokio::test] #[ignore] async fn dialog_appears_in_the_sessions_agent_window()  // 기록: profile = 세션 프로필, lines에 세션 ID·실제 경로·출처, 버튼 셋
#[tokio::test] #[ignore] async fn approve_runs_and_deny_denies_with_reason_denied()// 기록 줄 confirm.by "browser"
#[tokio::test] #[ignore] async fn allow_for_session_skips_the_next_dialog()
#[tokio::test] #[ignore] async fn the_dialog_does_not_take_focus()               // keyWindowProfile*가 모두 "Default", appActive 변화 없음
#[tokio::test] #[ignore] async fn agent_input_cannot_answer_the_dialog()         // ANSWER=none, 확인 대기 중 같은 탭에 세션 엔드포인트로 Enter/Space와 Page.handleJavaScriptDialog → 짧은 시간(2초) 뒤 timed_out
#[tokio::test] #[ignore] async fn two_sessions_get_their_own_dialogs()           // A=approve, B=deny: A만 실행, B는 denied, 기록의 창 프로필·세션 ID가 각자
#[tokio::test] #[ignore] async fn early_presses_are_ignored()                    // ANSWER_AFTER_MS=100 → ignored "locked", 그 뒤 timed_out
#[tokio::test] #[ignore] async fn bidi_text_is_shown_escaped()                   // 세션 이름 "x\u{202E}gpj.exe" → lines에 "\u{202e}" 글자, 원래 문자 없음
#[tokio::test] #[ignore] async fn a_daemon_restart_closes_open_dialogs()         // 대기 중 제어 세션을 닫고 새 세션이 claim → 창이 닫혔다(기록 "closed"), 이벤트 없음
#[tokio::test] #[ignore] async fn dialog_hooks_need_test_hooks()                 // 브라우저 환경에 HOOKS 없이 ANSWER=approve → timed_out(데몬 쪽 짧은 시간).
// 데몬에는 HOOKS가 있어야 하므로 시험 변수 PEGBOARD_TEST_BROWSER_NO_HOOKS=1(testenv)이면 ForkLaunched가 띄우는 브라우저의 환경에서 PEGBOARD_TEST_HOOKS만 뺀다(이 작업에서 browser.rs에 더한다)
```

`e2e_domain.rs`에 더한다: `confirmation_methods_are_only_on_the_claiming_session`(-32601), `domain_refuses_confirmations_for_human_contexts`(사람 컨텍스트·탭 → -32000, 창 없음), `confirmation_limits_are_enforced`(ID 모양, 겹침, 41줄, 17번째 창), `proxy_refuses_pegboard_confirmation_methods`.

- [ ] **Step 2: 컴파일 확인(`cargo test --no-run`).**
- [ ] **Step 3: 트리에서 구현하고 빌드한다(떼어 돌림, `-j 8`).** `//pegboard/ui`가 `//chrome/browser/ui` 헤더를 쓰려고 업스트림 BUILD.gn을 고쳐야 하면 그 줄을 `pegboard-cdp-domain.patch`에 넣고 이유를 머리말에 적는다(설계 5절 위험 2). 창을 비활성으로 띄울 수 없는 것으로 드러나면(설계 5절 위험 1) 멈추고 컨트롤러에게 보고한다.
- [ ] **Step 4: `package-dev.sh`, 컨트롤러가 맥미니에서 포크 구성으로 `e2e_confirm`, `e2e_domain`, `e2e_proxy`.** 기대: PASS. 변형(하나씩 빌드): `ShowInactive`를 `Show`로 → `the_dialog_does_not_take_focus` 실패; 1초 잠금 제거 → `early_presses_are_ignored` 실패; `test_hooks::Enabled()` 검사 제거 → `dialog_hooks_need_test_hooks` 실패; 주인 검사 제거 → `confirmation_methods_are_only_on_the_claiming_session` 실패; 컨텍스트 검사 제거 → `domain_refuses_confirmations_for_human_contexts` 실패; `DisplayText` 대신 원문 → `bidi_text_is_shown_escaped` 실패.
- [ ] **Step 5: 보안 검토자가 본다**(도메인 입력 검사, 주인 규칙, 창이 웹 콘텐츠 밖인지, 시험 훅의 문, 글 거르기). 반영 뒤 `export.sh`, `budget.sh` 기록, 커밋 `feat(browser): a confirmation dialog in the agent window, raised through the Pegboard domain`

---

### Task 8: 에이전트 창 표시와 에이전트 프로필 숨기기 (C++)

**Files:**
- Modify: `browser/pegboard/devtools/{pegboard_handler.h,pegboard_handler.cc,BUILD.gn}`, `crates/pegboard-core/tests/e2e_domain.rs`

**Interfaces:**
- Produces: 핸들러가 에이전트 프로필을 올리거나 만들 때(`OnProfileAdded`, `OnProfileReady`) `MarkAgentProfile(Profile*)`: 테마 사용자 색 하나(상수 `kAgentThemeColor`), 프로필 항목 생략. 도메인이 여는 에이전트 창에 창 사용자 제목 `"Pegboard 에이전트"`. 에이전트가 아닌 프로필에는 부르지 않는다(판정은 `IsAgentProfile`).

- [ ] **Step 1: 시험을 쓴다** (`e2e_domain.rs`, `#[ignore]`, 포크. 테마 값은 그 프로필 탭을 루트 세션에서 `chrome://prefs-internals`로 보내 읽고, 생략은 `chrome://local-state`의 프로필 항목으로 본다. 156의 키 이름은 트리에서 확인해 단언에 쓴다. 창 제목은 시험 훅 기록 `{"kind":"agent_window","title"}`으로 본다).

```rust
#[tokio::test] #[ignore] async fn agent_profiles_get_the_agent_theme_and_title()
#[tokio::test] #[ignore] async fn agent_profiles_are_omitted_from_the_profile_list()
#[tokio::test] #[ignore] async fn human_profiles_are_not_marked_or_hidden()       // Default와 Profile 1의 테마·생략 값이 바뀌지 않았다
#[tokio::test] #[ignore] async fn no_profile_picker_at_launch_with_agent_profiles()
// 같은 --user-data-dir로: 에이전트 프로필 둘을 만든 브라우저를 끄고 다시 켠다 → 타깃 목록에 chrome://profile-picker가 없고 Default 창이 있다
#[tokio::test] #[ignore] async fn agent_windows_from_the_last_run()              // 위 재시작에서 에이전트 프로필 창이 다시 열리는지 출력만(PASS로 두고 보고서에 기록)
```

- [ ] **Step 2: 트리에서 구현·빌드, `package-dev.sh`, 컨트롤러가 맥미니에서 `e2e_domain` 전체.** 기대: PASS. 생략이 선택 화면 판정을 덮지 않으면 설계 4.6의 대안 규칙대로 하고 보고한다. `agent_windows_from_the_last_run`이 다시 열림을 보이면, `//pegboard` 안의 작은 고침으로 되면 고치고 아니면 열린 항목으로 남긴다.
- [ ] **Step 3: 변형:** `MarkAgentProfile`의 `IsAgentProfile` 검사를 빼면 `human_profiles_are_not_marked_or_hidden`이 실패한다. 생략 줄을 빼면 `no_profile_picker_at_launch_with_agent_profiles`와 `agent_profiles_are_omitted_from_the_profile_list`가 실패한다.
- [ ] **Step 4: `export.sh`, `budget.sh`(업스트림 변화 0 기대), 커밋** `feat(browser): agent windows wear their own color and title; agent profiles stay out of the profile list`

---

### Task 9: 메인 파츠와 로그인·비밀번호 관리자 기본값 (C++)

**Files:**
- Create: `browser/pegboard/browser_parts/{BUILD.gn,main_parts.h,main_parts.cc,profile_defaults.h,profile_defaults.cc}`
- Modify: `browser/patches/pegboard-bootstrap.patch`(포함 경로와 클래스 이름만, 줄 수 같음), `browser/pegboard/bootstrap/BUILD.gn`, `browser/pegboard/BUILD.gn`, `crates/pegboard-core/tests/e2e_branding.rs`

**Interfaces:**
- Produces:

```cpp
namespace pegboard {
// The one ChromeBrowserMainExtraParts pegboard-bootstrap.patch adds: owns the daemon launcher
// and the profile defaults, and forwards PostBrowserStart / PostMainMessageLoopRun.
class MainParts : public ChromeBrowserMainExtraParts { /* DaemonLauncher launcher_; ProfileDefaults defaults_; */ };
// Changes the default (not the user's value) of each pref below on every profile as it is added.
class ProfileDefaults : public ProfileManagerObserver {
 public:
  static void ApplyTo(Profile* profile);   // also for the profiles already loaded at PostBrowserStart
};
}
```

설정(설계 4.6 표, 156의 이름은 트리에서 확인하고 상수로): `signin.allowed` = false, `credentials_enable_service` = false, `credentials_enable_autosignin` = false, 패스키 저장 제안 설정 = 끔. 방법은 `PrefService`의 기본값 바꾸기(사용자 값이 이긴다). 156에 그 함수가 없거나 첫 창이 옛 값을 보면(설계 5절 위험 5) 프로필 설정 등록 시점에 거는 길을 찾고, 업스트림 줄이 들면 기록한다.

- [ ] **Step 1: 시험을 쓴다** (`e2e_branding.rs`, 포크, `chrome://prefs-internals`와 `chrome://settings/people`의 본문):

```rust
#[tokio::test] #[ignore] async fn signin_and_password_saving_are_off_by_default()   // Default와 에이전트 프로필: 네 설정 값
#[tokio::test] #[ignore] async fn the_people_page_offers_no_sign_in()               // "로그인"/"Sign in", "동기화"/"sync" 버튼 문구가 없다(156 문구로 단언)
#[tokio::test] #[ignore] async fn a_chosen_value_wins_over_the_default()             // 켜기 전에 Default/Preferences에 credentials_enable_service: true → 켠 뒤에도 true
#[tokio::test] #[ignore] async fn the_first_window_sees_the_defaults()               // 앱 시작 직후의 첫 창에서 위 값
```

- [ ] **Step 2: 트리에서 구현·빌드, `package-dev.sh`, 맥미니에서 `e2e_branding`, 그리고 메인 파츠를 바꿨으므로 `e2e_bootstrap` 전체.** 기대: PASS.
- [ ] **Step 3: 변형:** `ApplyTo`의 `credentials_enable_service` 줄을 빼면 `signin_and_password_saving_are_off_by_default`가 실패한다. `MainParts`가 `PostMainMessageLoopRun`을 넘기지 않게 하면 `e2e_bootstrap::daemon_exits_when_the_browser_quits`가 실패한다.
- [ ] **Step 4: `export.sh`, `budget.sh`, 커밋** `feat(browser): sign-in and the built-in password manager are off by default`

---

### Task 10: 데몬 포기 알림 (C++)

**Files:**
- Create: `browser/pegboard/ui/{daemon_notice.h,daemon_notice.cc}`
- Modify: `browser/pegboard/bootstrap/{daemon_launcher.h,daemon_launcher.cc,BUILD.gn}`, `crates/pegboard-core/tests/e2e_bootstrap.rs`

**Interfaces:**
- Consumes: 작업 7의 `test_hooks`, 작업 9의 `MainParts`
- Produces:

```cpp
namespace pegboard {
// A browser-modal notice on the last active normal window (any profile), shown inactive:
// "Pegboard 에이전트 데몬이 멈췄습니다. 에이전트 기능을 다시 쓰려면 다시 시작하세요." [다시 시작] [닫기].
// No window: shown on the next window that opens.
class DaemonNotice { public: static void Show(base::OnceClosure restart); static void CloseIfShown(); };
}
// DaemonLauncher: on giving up (kMaxRestarts in kRestartWindow) calls DaemonNotice::Show with a
// restart that clears restarts_ and Launch()es. Never for kExitLockHeld. A daemon that starts
// again (restart) closes a notice still open.
```

시험 훅 기록: `{"kind":"daemon_stopped","profile","keyWindowProfileAt0","keyWindowProfileAt200ms","pressed"}`. `PEGBOARD_TEST_DIALOG_ANSWER`의 `*=restart` 또는 `*=close`.

- [ ] **Step 1: 시험을 쓴다** (`e2e_bootstrap.rs`, `#[ignore]`, `PEGBOARD_TEST_APP`):

```rust
#[tokio::test] #[ignore] async fn giving_up_shows_a_notice_without_taking_focus() // 1분 안에 4번 죽인다 → 기록 한 줄, 키 창 그대로
#[tokio::test] #[ignore] async fn restart_from_the_notice_starts_a_daemon()       // ANSWER *=restart → 새 데몬이 소켓에 답한다, 다시 4번 죽이면 다시 알림
#[tokio::test] #[ignore] async fn a_held_lock_shows_no_notice()                   // 3a의 a_held_lock_is_not_retried 구성 → 기록 없음
```

- [ ] **Step 2: 트리에서 구현·빌드, `package-dev.sh`, 맥미니에서 `e2e_bootstrap` 전체.** 기대: 기존 9개와 새 3개 PASS.
- [ ] **Step 3: 변형:** 포기 분기의 `Show` 호출을 빼면 `giving_up_shows_a_notice_without_taking_focus`가 실패한다. 다시 시작이 `restarts_`를 비우지 않으면 `restart_from_the_notice_starts_a_daemon`의 두 번째 알림 단언이 실패한다. `kExitLockHeld` 분기에 `Show`를 넣으면 `a_held_lock_shows_no_notice`가 실패한다.
- [ ] **Step 4: `export.sh`, `budget.sh`, 커밋** `feat(browser): tell the person when the daemon will not be restarted, with a way to restart it`

---

### Task 11: 전체 확인, 측정, 문서

**Files:**
- Modify: `docs/spike-findings.md`("3b 작업 N" 절들), 설계 문서(만든 그대로, "처음 제안은 …"), `docs/architecture.md`(11절 기본 정책 표와 "확인이 필요한 동작" 문단, 7절 API 표와 메시지 초안에 `confirm.*`, 남는 위험), `docs/roadmap.md`(할 일 5, 7 완료, 진행 현황), `README.md`(정책 옵션, 확인 창, `confirm.*`), `browser/README.md`(문자열 스크립트, 아이콘, 새 `//pegboard` 디렉터리)

- [ ] **Step 1: 맥미니에서 포크 구성 전체**(`PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_BROWSER=fork:<앱> PEGBOARD_TEST_APP=<앱> cargo test -p pegboard-core -- --ignored --test-threads=1`)**와 순정 구성 전체**(`pegboard-core`, `pegboard-cli` `--skip diag_`). 기대: 모두 PASS, 녹화 지연 중앙값 60ms 이하.
- [ ] **Step 2: 사람이 직접 본다(컨트롤러가 사용자에게 부탁).** 포크 앱을 평소처럼 켜고, MCP 기본 모드 에이전트로 `upload`와 `cookies set`을 보내 창을 본다: 사람 창을 쓰는 중에 초점을 잃지 않는가, 에이전트 창에서 창을 알아볼 수 있는가, 앱이 뒤에 있을 때 Dock이 튕기는가, 문구가 읽히는가. 에이전트 창의 색과 제목, 프로필 메뉴, About 화면(한국어), 아이콘, 비밀번호 저장 제안이 없는 것. 결과를 spike-findings에 적는다.
- [ ] **Step 3: 기록한다.** 패치별 업스트림 줄 수(손 패치 합계, 생성 커밋 따로), 새 `//pegboard` 줄 수, 빌드 시간(문자열 첫 빌드, `//pegboard/ui` 증분), 확인 창이 뜨기까지의 시간(`confirm.required`에서 기록 줄까지).
- [ ] **Step 4: 문서를 고치고 커밋** `docs: phase 3b done (browser UI), measurements`

---

## 작업 순서와 의존

1 → 2 → 3 → 4 → 5는 이 기기(Rust)와 맥미니 순정 시험으로 진행한다.
6은 1 이후 언제든 개발 기기의 Chromium 트리에서 병행한다(빌드가 길 수 있어 일찍 시작한다).
7은 5, 6 뒤(트리의 문자열 커밋 위에서). 8은 7 뒤(같은 핸들러 파일). 9는 6 뒤 언제든, 10은 7, 9 뒤. 11은 마지막.

## 설계와의 차이, 이 계획이 정한 것

- 오류 코드 `confirm_not_found`, 거부 이유 값 다섯, 결과의 `confirmed: {by}`, 동작 기록의 `confirm: {outcome, by, waitMs}` 모양을 정했다.
- 데몬 시험 훅은 시간 줄이기(`PEGBOARD_TEST_CONFIRM_TIMEOUT_MS`) 하나다. 답하는 훅은 브라우저 쪽에만 있다.
- `PEGBOARD_TEST_DIALOG_ANSWER`의 형식(`<세션 이름>=<답>` 목록)과 `PEGBOARD_TEST_DIALOG_ANSWER_AFTER_MS`를 정했다.
- 3a의 부트스트랩 연결이 넣는 클래스를 `pegboard::MainParts`로 바꿔(업스트림 줄 수 같음) 기본값과 알림을 같은 자리에서 돌린다.
- `pegboard confirm` CLI 명령은 만들지 않는다(설계 4.2).
