# 2b단계: 화면 녹화와 재생 (블랙박스) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 세션을 만들면 설정 없이 세션 탭의 화면(바뀔 때 초당 최대 2장, 명령마다 직전·직후 한 장), 탐색 기록, 사람 개입이 남고, 오래된 것은 저절로 지워지며, 사람이 `pegboard recording open <id>`로 기본 브라우저에서 사고 위치부터 되짚어 본다.

**Architecture:** `pegboardd` 안에 녹화기(`recording::recorder::Recorder`)를 둔다. 녹화기는 브라우저에 자기 CDP 연결을 하나 따로 열고(프레임이 데몬의 주 연결 `browser.conn`의 이벤트 방송을 채우지 않게), 세션에 들어온 탭마다 붙어 `Page.startScreencast`를 받고, `daemon::run_command`의 앞뒤에서 `Page.captureScreenshot`을 시간 제한 안에서 찍는다. 디스크 쓰기는 세션마다 하나인 쓰기 작업이 하고, 명령은 녹화를 기다리지 않는다(앞뒤 캡처의 제한 시간만큼만). 녹화 디렉터리의 관리(진행 중 목록, keep, 삭제, 보존 정리)는 `recording::store::RecordingStore` 하나가 하고, 재생은 데몬이 시작할 때 루프백에 띄우는 작은 HTTP 서버(`recording::replay`)가 녹화마다 새로 만든 토큰으로 정적 페이지와 녹화 파일을 읽기 전용으로 내준다.

**Tech Stack:** Rust(기존 워크스페이스), tokio, serde_json, `httparse`(이미 쓰는 것. 재생 서버), `toml`(새 의존성, `config.toml`), `data-encoding`(프레임 base64), clap. 브리지 확장(`extension/bridge/sw.js`, 작업 1 결과에 따라). Chrome for Testing 157, agent-browser 0.42.0. 실험은 Node(`spikes/`)와 Rust `#[ignore]` 시험.

**Spec:** [2026-11-17-phase2b-recording-design.md](./2026-11-17-phase2b-recording-design.md). 앞 문서: [architecture.md](../architecture.md) 8절과 11절, [2a 계획](./2026-11-13-phase2a-mcp-and-policy.md), [spike-findings.md](../spike-findings.md)(0단계 A1: 포커스 유지 없이 숨은 탭은 화면 스트림 0~1프레임).

## Global Constraints

- 설계 2절의 실패 조건이 하나라도 생기면 그 작업은 끝난 것이 아니다: 녹화 때문에 에이전트 명령이 눈에 띄게 느려진다(4.6절), 사람 프로필의 탭이 한 장이라도 찍힌다, 녹화가 보존 한도를 넘어 디스크를 채운다, 재생 화면이나 녹화 파일에 에이전트 세션이 닿는다, 화면 기록을 끈 도메인의 화면이 남는다, 동작 기록에서 가린 값이 재생 화면의 동작 목록(텍스트)에서 드러난다.
- 화면: 바뀔 때 받은 프레임은 **초당 최대 2장**, 모든 명령의 **직전·직후 한 장씩**. JPEG **품질 60, 가로 최대 1280픽셀**(제안. 작업 1 측정으로 바꾸면 설계 문서에 적는다).
- 성능(설계 4.6): 명령 하나에 녹화가 더하는 시간 **중앙값 50ms 이하, 최악 1초**. 앞뒤 캡처가 실패하거나 제한 시간을 넘기면 프레임 없이 명령을 진행한다. 녹화는 명령을 막거나 실패시키지 않는다.
- 보존: 끝난 지 **14일**이 지난 것, 그다음 전체가 **5GB**를 넘으면 오래된 것부터. **진행 중인 녹화와 `keep`한 것은 지우지 않는다.** 용량으로 지울 때는 **화면(`screen/`)부터** 지우고 동작 기록은 14일까지 둔다. 녹화 하나의 화면은 **1GB**에서 멈춘다(동작 기록은 계속). 2b 이전 녹화도 14일 규칙. 데몬 시작 때, **1시간마다**, 녹화가 끝날 때 정리한다. `keep`한 것은 한도에 세지 않는다.
- 설정: `<PEGBOARD_HOME>/config.toml`의 `[recording] max_age_days`, `max_total_gb`, `max_per_recording_gb`, `screen_off_domains`. 파일이 없으면 기본값(14, 5, 1, 빈 목록). 1GB는 10^9바이트로 센다.
- 저장: `<PEGBOARD_HOME>/recordings/<recordingId>/`, 디렉터리 **0700**, 파일 **0600**. `manifest.json`의 형식 버전은 **2**(지금 1).
- 재생: 루프백 `http://127.0.0.1:<포트>/r/<토큰>/`, 토큰은 열 때마다 새로, **그 녹화 하나에만**, **30분 뒤나 데몬 종료 시** 사라진다. 사람의 기본 브라우저로 연다(macOS `open`). 바이너리에 넣은 HTML/JS/CSS만 쓰고 외부 자원은 없다. 페이지에서 온 글은 텍스트로만 넣는다. 이 포트는 2a 주소 규칙의 "pegboard 자신의 포트"다.
- 에이전트에게 화면을 주지 않는다. 에이전트 쪽 프록시 필터는 화면 스트림 메서드를 거부한다. MCP 기본 모드에는 `keep`/`delete`가 없다.
- 2b에서 만들지 않는 것: 동영상 내보내기(`recording export`), 네트워크 본문 기록, 변조 방지, 원격 열람, 실시간 보기.
- 1단계와 2a의 보장을 약하게 만들지 않는다: `argv::check`, 동작 기록의 가림 규칙, 프록시 필터의 기본 거부, 요청 가드, 클립보드 방어, 다운로드 귀속, 사람 프로필에 아무것도 걸지 않는 것.
- 애매하면 닫는 쪽으로 만든다: 화면을 끈 도메인인지 모르면 찍지 않는다, 세션 것인지 모르는 탭은 찍지 않는다, 지워도 되는지 모르는 녹화는 지우지 않는다, 읽을 수 없는 `config.toml`이면 데몬을 띄우지 않는다.
- **실제 브라우저를 띄우는 것은 구현 에이전트가 하지 않는다.** 이 기기에는 쓸 수 있는 브라우저가 없다. `#[ignore]` 시험과 `spikes/`의 실험은 컨트롤러가 두 번째 기기(Mac mini)에서 돌려 결과를 돌려준다. 구현 에이전트는 코드, 브라우저 없는 시험(`cargo test --workspace`), `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`까지 하고, 보고서에 (1) 돌려야 할 명령을 그대로, (2) 시험마다 기대 결과, (3) 새 브라우저 시험마다 "이 한 줄을 이렇게 바꾸면 실패한다"는 변형 하나(파일, 줄, 바꿀 내용)를 적는다. 컨트롤러가 변형을 넣어 실패를 확인하고 되돌린 결과까지 돌려준다.
- 실제 브라우저 시험은 임시 `PEGBOARD_HOME`만 쓴다. 사용자의 기본 브라우저를 띄우지 않는다(`recording open` 시험은 `PEGBOARD_TEST_OPEN_LOG`로 주소만 받는다).
- 새 시험용 환경 변수는 모두 `testenv::var`로 읽는다(`PEGBOARD_TEST_HOOKS`가 있을 때만). 새 시험용 코어 API 메서드는 `test.dropDownloadPolicy`처럼 `PEGBOARD_TEST_HOOKS`일 때만 있고 세션 토큰을 요구한다.
- 문서와 코드에 홈 디렉터리 경로, 사용자 이름, 기기 이름을 쓰지 않는다. 문서는 한국어. 커밋 메시지는 영어, 끝에 `Co-Authored-By: <구현한 모델 이름> <noreply@anthropic.com>`. 푸시와 main 병합은 컨트롤러가 한다.

## Review Focus

설계가 암시하지만 시험이 놓치기 쉬운 것. 각 줄의 시험은 괄호 안 작업에 넣었다.

1. **사람 프로필의 탭이 한 장이라도 찍힌다.** 에이전트 탭이 연 팝업이 사람 프로필로 가는 일은 없지만, 녹화기가 "세션에 들어왔다"는 알림만 믿으면 잘못된 타깃 ID 하나로 사람 탭에 붙는다. 녹화기는 붙기 전에 타깃의 브라우저 컨텍스트를 자기 연결로 다시 확인한다(작업 3 `may_record_only_in_the_session_context`, `human_profile_tab_is_never_recorded`).
2. **화면을 끈 도메인의 프레임이 쓰인다.** 리다이렉트로 그 도메인에 닿을 때, 팝업이 처음부터 그 주소일 때, 탐색 직후 새 문서의 첫 프레임이 주소 갱신보다 먼저 올 때, 명령 앞뒤 캡처가 그 사이에 찍힐 때(작업 3 `screen_off_domain_frames_are_never_written`, 작업 4 `commands_on_a_screen_off_domain_have_no_frames`, 작업 1 S6).
3. **재생 토큰이 다른 녹화를 열거나 만료되지 않는다.** 다른 녹화 ID를 경로에 넣기, `..`와 심볼릭 링크, 만료 뒤, 데몬을 다시 띄운 뒤, `Host`가 루프백이 아닌 요청(DNS rebinding), 지운 녹화(작업 7 `a_token_opens_only_its_recording`, `an_expired_token_is_refused`, `paths_outside_the_recording_are_refused`, `a_foreign_host_header_is_refused`).
4. **보존 정리가 진행 중이거나 keep한 녹화를 지운다.** 진행 중 판정을 `ended_at` 유무로 하면, 데몬이 강제로 죽어 `ended_at`이 비어 남은 녹화는 영원히 안 지워지고, 반대로 판정을 바꾸면 진행 중인 것이 지워질 수 있다. 진행 중은 데몬의 살아 있는 세션 목록으로만 판정하고, 고아 녹화는 마지막 쓰기 시각을 끝난 시각으로 삼는다. manifest를 읽을 수 없는 디렉터리는 건드리지 않는다(작업 6 `prune_never_deletes_a_running_or_kept_recording`, `an_orphan_counts_from_its_last_write`, `an_unreadable_manifest_is_left_alone`).
5. **녹화 실패가 명령을 실패시키거나 제한보다 늦춘다.** 화면 잠금처럼 캡처가 멈추는 상태, 녹화기 연결이 끊긴 동안, 디스크 쓰기 오류(작업 4 `a_slow_capture_never_delays_a_command_beyond_the_cap`, `commands_run_while_the_recorder_is_disconnected`, 작업 3 `write_errors_drop_frames_only`).
6. **재생 화면이 페이지의 글을 HTML로 해석한다.** 페이지 제목, 주소, 오류 문구에 `<img onerror>`가 든 녹화(작업 7 `replay_page_shows_page_text_as_text`, `assets_never_parse_html`).

## 파일 구조

| 파일 | 책임 | 작업 |
|---|---|---|
| `spikes/40-screencast.mjs`, `41-human-signals.mjs`, `42-disk.mjs`, `spikes/ext-activity/` (새) | 측정(Chrome 동작) | 1 |
| `crates/pegboard-core/tests/probe_2b.rs` (새, 작업 4에서 지움) | 측정(실제 데몬 부품 위의 지연과 충돌) | 1 |
| `crates/pegboard-protocol/src/lib.rs` | `RecordExtra`, `SessionPolicy.record`, `codes::RECORDING_RUNNING`, `RecordingSummary` | 2, 5, 6 |
| `crates/pegboard-core/src/actionlog.rs` | `ActionEntry`의 새 필드, `Manifest` 형식 2, `ManifestHandle`, `SessionLog` | 2 |
| `crates/pegboard-core/src/recording/mod.rs` (새) | 하위 모듈 선언 | 2 |
| `crates/pegboard-core/src/recording/format.rs` (새) | 줄 형식(`IndexLine`, `NavLine`, `NetworkLine`, `ConsoleLine`), 0700/0600 파일 도우미 | 2, 5 |
| `crates/pegboard-core/src/recording/config.rs` (새) | `config.toml`의 `[recording]` | 2 |
| `crates/pegboard-core/src/recording/screen.rs` (새) | 프레임 묶기(`Throttle`), 프레임 쓰기(`ScreenWriter`), 녹화 하나의 한도(`ScreenBudget`), 찍어도 되는지(`may_record`, `ScreenGate`) | 3 |
| `crates/pegboard-core/src/recording/recorder.rs` (새) | 녹화기: 자기 CDP 연결, 탭마다 스트림, 앞뒤 캡처, 탐색·개입·선택 기록 | 3, 4, 5 |
| `crates/pegboard-core/src/recording/events.rs` (새) | 탐색 기록 줄 만들기, 사람 개입 판정(순수 함수), 네트워크·콘솔 줄 | 5 |
| `crates/pegboard-core/src/recording/store.rs` (새) | 녹화 디렉터리 관리: 진행 중 목록, keep, 삭제, 요약 | 6 |
| `crates/pegboard-core/src/recording/retention.rs` (새) | 보존 정리 계획(순수 함수)과 실행 | 6 |
| `crates/pegboard-core/src/recording/replay/mod.rs`, `model.rs`, `assets/index.html`, `assets/app.js`, `assets/app.css` (새) | 재생 서버, 토큰, 재생 자료, 정적 페이지 | 7 |
| `crates/pegboard-core/src/paths.rs` | `Paths.config` | 2 |
| `crates/pegboard-core/src/address.rs` | `host_listed`(도메인 목록 대조를 공개) | 2 |
| `crates/pegboard-core/src/session.rs` | 녹화기에 탭 알리기, `own_ports`에 재생 포트, 녹화기 접근자 | 3, 7 |
| `crates/pegboard-core/src/daemon.rs` | 명령 앞뒤 캡처 | 4 |
| `crates/pegboard-core/src/service.rs` | 녹화 시작·끝, 보존 주기, 녹화 RPC, 재생 서버 | 3, 6, 7 |
| `crates/pegboard-core/src/proxy/filter.rs` | 화면 스트림 메서드 거부 | 3 |
| `crates/pegboard-cli/src/main.rs` | `--record`, `recording list/show/open/keep/unkeep/delete/prune`, `browser start`의 설정 검사 | 2, 5, 6, 7 |
| `crates/pegboard-cli/src/mcp/tools.rs` | 녹화 목록·조회 표시, 운영자 모드의 keep/delete, 정책 스키마의 `record` | 5, 6 |
| `crates/pegboard-cli/tests/common/mod.rs` | `SCREEN_OFF_NAME`, 공유 홈의 `config.toml`, 작은 한도의 홈 | 3 |
| `crates/pegboard-cli/tests/e2e_recording.rs` (새) | 녹화 시험(CLI, 실제 데몬) | 3~7 |
| `crates/pegboard-core/tests/e2e_replay.rs` (새) | 재생 페이지를 실제 브라우저에서 그리는 시험 | 7 |
| `README.md`, `docs/architecture.md`, `docs/roadmap.md`, `docs/spike-findings.md`, 설계 문서, `docs/phase2b-manual-checks.md` (새) | 문서 | 1, 4, 8 |

---

### Task 1: 측정 (만들기 전에)

설계 5절의 다섯 가지와, 화면을 끈 도메인을 지키는 데 필요한 이벤트 순서(S6)를 잰다. 제품 코드는 바꾸지 않는다.

**방법을 나눈 이유.** Chrome 자체의 동작(창 상태별 프레임, 이벤트 순서, 사람 입력 신호, 디스크 사용량)은 0단계 `01-occluded.mjs`, 2a `20~32`처럼 `spikes/`의 Node 스크립트로 잰다. 창을 가리고 최소화하고 잠그는 조작과 사람이 직접 하는 입력이 필요하고, `spikes/lib`(`launch.mjs`, `cdp.mjs`, `probe.mjs`, `ab.mjs`)이 이미 그 준비를 한다. 반면 지연(S2)과 2a 방어와의 충돌(S4)은 pegboard의 실제 요청 가드, 클립보드 방어, 다운로드 귀속, `Runner` 위에서 재야 의미가 있으므로 코어의 공유 시험 하네스(`tests/common`의 `Harness`)를 쓰는 Rust `#[ignore]` 시험으로 잰다. 이 시험은 녹화기를 흉내 내는 쪽(시험이 연 두 번째 CDP 연결)만 시험 안에 둔다.

**Files:**
- Create: `spikes/40-screencast.mjs`, `spikes/41-human-signals.mjs`, `spikes/42-disk.mjs`, `spikes/ext-activity/manifest.json`, `spikes/ext-activity/sw.js`
- Create: `crates/pegboard-core/tests/probe_2b.rs`
- Modify: `docs/spike-findings.md` (새 절 "2b단계 측정")

**Interfaces:**
- Produces: `docs/spike-findings.md` "2b단계 측정"의 판정 줄(아래 F1~F8). 작업 3~5가 그 줄을 읽고 따른다. 각 줄은 `F<n>: <결정> (근거: <질문 번호>)` 꼴.

각 Node 스크립트는 질문마다 `PASS`/`FAIL`/`관측값`을 한 줄씩 출력하고 `spikes/out/<번호>.json`에 결과를 남긴다(기존 `writeResult`). 사람이 해야 하는 단계는 스크립트가 무엇을 하라고 출력하고 Enter를 기다린다(컨트롤러가 Mac mini에서 한다). 모든 스크립트는 임시 user-data-dir를 쓰고 끝날 때 브라우저를 닫는다.

- [ ] **Step 1: `40-screencast.mjs`.** 에이전트 프로필 탭 하나(100ms마다 배경색이 바뀌는 페이지)에, 연결 F(포커스 유지 역: `Emulation.setFocusEmulationEnabled`와 `Page.setWebLifecycleState active`)와 연결 R(녹화기 역)을 붙인다. 구성 A는 F와 같은 세션에서 스트림, 구성 B는 R의 별도 세션에서 스트림. 잴 것:
  - V1. 10초 동안의 스트림 프레임 수와 `Page.captureScreenshot`(JPEG 60) 성공 여부와 시간(10번의 중앙값과 최대): 보이는 탭, 같은 창의 뒤쪽 탭, 최소화한 창, 다른 창에 가려진 창, 다른 Space(사람 단계), 화면 잠금 중(사람 단계), 디스플레이 절전(사람 단계). 구성 A와 B 각각.
  - V2. 잠금을 풀거나 창을 되돌렸을 때 스트림을 다시 걸지 않아도 프레임이 다시 오는지.
  - V3. 프레임 한 장의 바이트와 픽셀 크기: 스트림(`maxWidth: 1280`), 직접 캡처(그대로, 그리고 `Page.getLayoutMetrics`로 구한 배율의 `clip`으로 가로 1280 이하). 레티나 화면에서 캡처 시간 차이.
  - V4. 확인(ack) 방식: 받자마자 `Page.screencastFrameAck` vs 500ms 늦춰 확인. 초당 프레임, 렌더러와 브라우저 프로세스의 CPU(`ps -o %cpu`), 화면이 멈춘 뒤 마지막 상태의 프레임이 오는지.
  - V5. 다른 연결(에이전트 역)이 같은 탭의 자기 세션에서 `Page.startScreencast`/`Page.stopScreencast`를 보낼 때 R의 스트림이 멈추거나 바뀌는지.
  - S6(순서). R의 세션에서 탐색 50번(직접 이동, 302로 다른 호스트로 넘어가기, 스크립트의 `location=`, `window.open`으로 연 팝업이 처음부터 그 주소): `Page.frameRequestedNavigation`, `Page.frameStartedNavigating`, `Page.frameNavigated`(주 프레임)와, 새 문서의 첫 스트림 프레임이 오는 순서. 문서마다 처음 스크롤 위치를 다르게 두고 프레임의 `metadata.scrollOffsetY`로 어느 문서의 프레임인지 가린다. 직접 캡처를 탐색 도중에 보냈을 때 돌아온 그림이 어느 문서의 것인지도 같은 방법으로 본다.
- [ ] **Step 2: `41-human-signals.mjs`와 `ext-activity/`.** 시험용 확장(권한 `tabs`만. `chrome.tabs.onActivated`, `chrome.windows.onFocusChanged`를 서비스 워커가 `Runtime.addBinding`으로 만든 함수에 넘긴다)을 에이전트 프로필에 올린다. R의 세션에서 `Page.*` 탐색 이벤트를 듣는다. 사람 단계와 자동 단계를 같은 탭에서 번갈아 하고, 단계마다 어떤 신호가 왔는지 표로 남긴다.
  - H1 사람이 주소창에 입력, H2 사람이 뒤로 단추, H3 사람이 링크 클릭, H4 사람이 탭 전환, H5 사람이 다른 앱으로 갔다가 돌아옴.
  - H6 agent-browser의 `open`, `click`(링크), `back`, `tab <n>`(`--cdp`로, 2a 스파이크처럼), H7 페이지 스크립트의 `location=`, `<meta refresh>`, `history.pushState`.
  - H8 바인딩이 서비스 워커가 잠들었다 깨어난 뒤에도 남는지(30초 기다린 뒤 H4 반복).
  - 판정할 것: 사람의 입력(H1~H5)과 에이전트·스크립트(H6, H7)를 가르는 신호가 있는가. 특히 `Page.frameRequestedNavigation`의 유무와 `reason`, `Page.frameStartedNavigating`의 `navigationType`.
- [ ] **Step 3: `42-disk.mjs`.** `--minutes <n>`(기본 30). 시험 페이지 서버가 글 페이지, 그림이 많은 페이지, CSS 애니메이션 페이지를 돌아가며 내주고, 20초마다 탐색, 2초마다 스크롤, 5초마다 흉내 낸 명령 하나(앞뒤 직접 캡처)를 한다. 스트림은 초당 2장으로 묶어(앞 프레임에서 500ms가 안 지났으면 버리고 마지막 것은 남긴다) 실제로 파일에 쓴다. 잴 것: 전체 바이트, 프레임 수, 분당 바이트, 한 시간과 14일 사용량 추정, 5GB로 며칠 치가 남는지.
- [ ] **Step 4: `probe_2b.rs`** (`#[ignore]`, 컨트롤러가 `cargo test -p pegboard-core --test probe_2b -- --ignored --test-threads=1 --nocapture`로 돌린다). 녹화기 흉내: 시험이 브라우저에 두 번째 `CdpConn`을 열고 세션 탭에 붙어 `Page.enable`, `Page.startScreencast {format: "jpeg", quality: 60, maxWidth: 1280}`, 받자마자 확인하고 500ms 묶기로 버린다. 명령마다 직전과 직후 `Page.captureScreenshot`(JPEG 60)을 차례로 기다린다(상한 추정).
  - S2. `fn probe_command_latency_with_and_without_recording()`: 명령 `open`, `snapshot -i`, `click`, `fill`, `type`, `press Enter`, `get url`, `get text`, `eval`, `scroll down 200`, `wait 100`, `screenshot`, `tab new`, `tab list`를 흉내 켜짐/꺼짐으로 번갈아 20번씩 `daemon::run_command`로 돌린다(같은 세션, 짝지은 실행). 명령마다 더해진 시간의 중앙값과 최대를 표로 출력한다. 품질 40, `maxWidth: 800`, 직후 캡처만 하는 설정도 한 번씩.
  - S4. `fn probe_recording_beside_the_2a_defences()`: 흉내를 켠 채 2a의 방어를 그대로 확인한다: 클립보드 쓰기와 읽기가 막힌다(`tests/shared/clipboard.rs`의 도우미, 시스템 클립보드 저장·복원), 사설 주소 요청이 막힌다, 링크 다운로드가 세션 파일 디렉터리에 들어간다, 곧 닫히는 팝업의 다운로드가 귀속된다, `dialog accept`가 된다, 팝업이 세션에 들어간다. 각 줄 `PASS`/`FAIL`.
- [ ] **Step 5: 결과를 `docs/spike-findings.md` "2b단계 측정"에 적고 판정 줄을 쓴다.**
  - F1: 스트림을 거는 곳(구성 B: 녹화기의 자기 연결, 또는 A).
  - F2: 확인 방식(바로, 또는 늦춰서).
  - F3: 직접 캡처의 인자(그대로, 또는 `clip` 배율).
  - F4: 창 상태별로 스트림이 오는 것과 안 오는 것. 안 오는 상태에서 재생 화면이 "화면 없음"을 보여야 하는 조건.
  - F5: 화면 끄기의 기준 이벤트(S6에서 새 문서의 첫 프레임보다 늘 먼저 오는 이벤트)와, 탐색 시작부터 그 이벤트까지 프레임을 버려야 하는지.
  - F6: 사람 개입 신호와 그 확실성(작업 5의 판정표). 시험용 확장 방식(탭 전환 신호)을 브리지에 넣을지.
  - F7: S2의 수치와, 작업 4가 쓸 캡처 제한 시간.
  - F8: 30분 사용량과 14일 추정.
- [ ] **Step 6: 멈춤 조건을 판정한다.** 하나라도 해당하면 컨트롤러가 수치와 함께 사용자에게 보고하고, 사용자가 정하기 전에는 작업 3을 시작하지 않는다.
  - M1. 가려진 탭·최소화·다른 Space·화면 잠금 가운데 어느 상태에서 스트림 프레임이 오지 않고 직접 캡처도 10번 중 2번 이상 실패하거나 1초를 넘는다(설계 5절의 예).
  - M2. S2에서 어느 명령이든 더해진 시간의 중앙값이 50ms를 넘거나 최대가 1초를 넘고, 가벼운 설정(품질 40, 800픽셀, 직후만)으로도 못 맞춘다.
  - M3. S4에서 2a 방어가 하나라도 실패하거나, V5에서 에이전트 쪽 세션이 녹화기의 스트림을 멈출 수 있는데 그 메서드를 프록시에서 거부해도 막히지 않는다.
  - M4. S6에서 새 문서의 프레임(스트림이나 직접 캡처)이 어떤 탐색 이벤트보다 먼저 올 수 있고, 탐색 시작부터 버려도 막히지 않는다(화면 끄기를 지킬 수 없다).
  - M5. 42의 30분 사용량이 1GB를 넘는다(녹화 하나의 한도에 보통 세션이 걸린다). 250MB를 넘으면 멈추지는 않되 수치를 보고하고 화질·크기 조정을 제안한다.
- [ ] **Step 7: 다시 계획할 조건.** 아래는 사용자 보고 없이 컨트롤러가 작업 3~5의 해당 줄을 고친다: F1이 A(별도 연결에서 프레임이 안 온다), F2가 늦춘 확인, F3이 `clip`, F6이 확장의 탭 전환 신호를 쓰기로 함.
- [ ] **Step 8: 커밋.** `spike: screencast visibility, event order, human signals, latency and disk use for phase 2b`

---

### Task 2: 저장 형식 2와 `config.toml`

**Files:**
- Modify: `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-core/src/actionlog.rs`, `paths.rs`, `address.rs`, `lib.rs`, `daemon.rs`, `downloads.rs`(새 필드가 생긴 `ActionEntry` 리터럴), `crates/pegboard-core/Cargo.toml`(`toml = "0.8"`), `crates/pegboard-cli/src/main.rs`(`browser start`의 설정 검사)
- Create: `crates/pegboard-core/src/recording/mod.rs`, `recording/format.rs`, `recording/config.rs`
- Test: 각 파일의 단위 시험

**Interfaces:**
- Produces (`pegboard-protocol`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordExtra { Network, Console }
```

- Produces (`actionlog.rs`):

```rust
pub const MANIFEST_FORMAT: u32 = 2;
pub type SessionLog = Arc<tokio::sync::Mutex<ActionLog>>;   // moved here from service.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind { #[default] Command, Human }

// ActionEntry gains (all #[serde(default)], skipped when empty, so a format-1 line reads as before):
//   kind: EntryKind, frame_before: Option<String>, frame_after: Option<String>,
//   what: Option<String>, certain: Option<bool>
// and derives Default. A frame is named "<targetId>/<file>" relative to screen/.
impl ActionEntry {
    pub fn human(session_id: &str, ts: &str, target_id: &str, url: Option<String>, what: &str, certain: bool) -> ActionEntry;
    // command "human", args [], ok true, kind Human
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenStatus { #[default] None, Recorded, Capped, Pruned }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sizes { pub screen_bytes: u64, pub total_bytes: u64 }

// Manifest gains (#[serde(default)]): keep: bool, screen: ScreenStatus, record: Vec<RecordExtra>,
// sizes: Option<Sizes> (written when the recording ends and after a prune).

/// The manifest of an open recording, shared by its writers; every update rewrites
/// manifest.json atomically (tmp + rename, 0600).
#[derive(Clone)]
pub struct ManifestHandle { /* dir, Arc<std::sync::Mutex<Manifest>> */ }
impl ManifestHandle {
    pub fn get(&self) -> Manifest;
    pub fn update(&self, f: impl FnOnce(&mut Manifest)) -> std::io::Result<()>;
    pub fn dir(&self) -> &std::path::Path;
}
impl ActionLog {
    pub fn manifest_handle(&self) -> ManifestHandle;
    pub fn manifest(&self) -> Manifest;            // was &Manifest
    pub fn dir(&self) -> &std::path::Path;
}
pub fn recording_dir(paths: &Paths, recording_id: &str) -> std::io::Result<PathBuf>;  // now pub
pub fn read_manifest(dir: &std::path::Path) -> std::io::Result<Manifest>;              // now pub
pub fn write_manifest_file(dir: &std::path::Path, m: &Manifest) -> std::io::Result<()>;
```

- Produces (`recording/format.rs`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind { Change, Before, After, Off, Capped }

/// One line of screen/<targetId>/index.jsonl. `Off` and `Capped` are markers without a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexLine { pub ts: String, pub file: Option<String>, pub url: Option<String>,
                       pub width: Option<u32>, pub height: Option<u32>, pub kind: FrameKind }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavEvent { Created, Navigated, Closed }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavLine { pub ts: String, pub target_id: String, pub event: NavEvent,
                     pub url: Option<String>, pub opener: Option<String> }

pub fn create_private_dir(path: &Path) -> std::io::Result<()>;    // 0700; an existing symlink or non-dir is refused
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()>;  // create_new, 0600
pub struct JsonlWriter { /* File opened create_new|append, 0600 */ }
impl JsonlWriter {
    pub fn create(path: &Path) -> std::io::Result<JsonlWriter>;
    pub fn append<T: Serialize>(&mut self, line: &T) -> std::io::Result<u64>;  // bytes written, line capped at actionlog::MAX_LINE_BYTES
}
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>>;   // a missing file is empty; a bad line is skipped
```

- Produces (`recording/config.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RecordingConfig {
    pub max_age_days: u32,            // 14; 1..=3650
    pub max_total_gb: f64,            // 5.0; finite, > 0
    pub max_per_recording_gb: f64,    // 1.0; finite, > 0
    pub screen_off_domains: Vec<String>,  // [] ; host names or IPs, lowercased (policy's allow-domain rule)
}
impl Default for RecordingConfig { /* 14, 5.0, 1.0, [] */ }
impl RecordingConfig {
    pub fn load(paths: &Paths) -> Result<RecordingConfig, String>;  // missing file => default; anything else unreadable => Err naming the file
    pub fn parse(text: &str) -> Result<RecordingConfig, String>;     // top level: only a [recording] table (deny unknown)
    pub fn max_total_bytes(&self) -> u64;            // GB = 10^9 bytes
    pub fn max_per_recording_bytes(&self) -> u64;
    pub fn max_age(&self) -> time::Duration;
    /// Whether frames of a main frame at `url` must not be written. A URL that does not
    /// parse counts as off when the list is not empty; data: and about:blank are never off.
    pub fn screen_off(&self, url: &str) -> bool;
}
```

- Produces: `Paths.config = home.join("config.toml")`. `address::host_listed(url: &url::Url, list: &[String]) -> bool`(지금의 `is_listed`를 감싼 공개 함수, 같은 규칙: 이름은 같거나 하위 도메인, IP는 같은 주소만). `policy::is_host`를 `pub(crate)`로.
- CLI: `pegboard browser start`는 데몬을 띄우기 전에 `RecordingConfig::load`를 부르고, 실패하면 `config.toml: <이유>`를 찍고 종료 코드 2. 데몬도 시작할 때 다시 읽고 실패하면 뜨지 않는다.

- [ ] **Step 1: 실패하는 단위 시험을 쓴다.**

```rust
// actionlog.rs
#[test] fn format_1_recording_reads_as_before() {
    // a manifest without keep/screen/record/sizes and action lines without the new fields:
    // read() gives keep false, screen None, record [], sizes None, kind Command, frames None
}
#[test] fn new_fields_are_left_out_when_empty() { /* a plain command entry serializes without kind/frameBefore/frameAfter/what/certain */ }
#[test] fn human_entry_round_trips() { /* {"kind":"human","command":"human","what":"..","certain":false,...} */ }
#[test] fn manifest_handle_updates_the_file_atomically_with_0600() {}
#[test] fn new_recordings_are_format_2() {}
// format.rs
#[test] fn private_files_and_dirs() { /* dir 0700, file 0600; create_private_dir on a symlink => Err; JsonlWriter refuses an existing file */ }
#[test] fn read_jsonl_skips_bad_lines_and_missing_files() {}
// config.rs
#[test] fn defaults_without_a_file() { /* 14, 5.0, 1.0, [] */ }
#[test] fn config_is_parsed_and_checked() {
    // "[recording]\nmax_age_days = 3\nscreen_off_domains = [\"Bank.Example\"]" => 3, ["bank.example"]
    // max_age_days = 0, max_total_gb = 0, max_total_gb = -1, max_per_recording_gb = nan,
    // screen_off_domains = ["https://bank.example"], ["*.bank.example"], ["bank.example/x"],
    // unknown key "screen_off_domain", unknown table [other], not TOML: each Err with the key named
}
#[test] fn screen_off_matches_domain_and_subdomains_only() {
    // list [bank.example]: https://bank.example/, https://www.bank.example/x, https://BANK.example./ => true
    // https://bank.example.evil.test/, https://notbank.example/, data:text/html,x, about:blank => false
    // "not a url" => true (list not empty); empty list => always false
}
```

- [ ] **Step 2: 실패 확인.** `cargo test -p pegboard-core --lib actionlog recording` → 컴파일 오류.
- [ ] **Step 3: 구현.** 기존 `ActionEntry { .. }` 리터럴은 `..Default::default()`로 맞춘다. `ActionLog::finish`는 그대로 둔다(작업 6이 저장소 쪽으로 옮긴다).
- [ ] **Step 4: 통과 확인.** `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`.
- [ ] **Step 5: CLI 설정 검사 시험** (`e2e_cli.rs`, 브라우저가 필요 없다: 데몬을 띄우기 전에 끝난다).

```rust
#[test] fn browser_start_refuses_a_bad_config() {
    // home with config.toml "[recording]\nmax_age_days = 0": `browser start` => exit 2, stderr names config.toml and max_age_days; no daemon
}
```

- [ ] **Step 6: 커밋.** `feat: recording format 2 and config.toml [recording]`

---

### Task 3: 화면 스트림

**Files:**
- Create: `crates/pegboard-core/src/recording/screen.rs`, `recording/recorder.rs`
- Modify: `crates/pegboard-core/src/session.rs`(`SessionManager::new`가 `Arc<Recorder>`를 받는다. `on_target_added`와 대상이 범위에서 빠지는 곳에서 녹화기에 알린다), `service.rs`(데몬 시작 때 녹화기를 만들고, 세션을 만들 때 `begin_session`, 끝낼 때 `end_session`), `proxy/filter.rs`, `crates/pegboard-core/tests/common/mod.rs`(`Shared`가 녹화기를 넘긴다), `crates/pegboard-cli/tests/common/mod.rs`
- Test: `screen.rs` 단위 시험, `filter.rs` 단위 시험, `crates/pegboard-cli/tests/e2e_recording.rs`(새), `crates/pegboard-core/tests/e2e_proxy.rs`

**Interfaces:**
- Consumes: 작업 2의 `RecordingConfig`, `ManifestHandle`, `SessionLog`, `IndexLine`, `FrameKind`, `JsonlWriter`, `write_private_file`, `create_private_dir`. 작업 1의 F1, F2, F4, F5.
- Produces (`screen.rs`):

```rust
pub const MIN_FRAME_GAP: Duration = Duration::from_millis(500);   // at most 2 frames a second

/// Leading-edge throttle that keeps the latest dropped item and hands it out once the gap
/// has passed without a newer one, so the settled screen is always the last frame.
pub struct Throttle<T> { /* gap, last_out: Option<Instant>, pending: Option<T> */ }
impl<T> Throttle<T> {
    pub fn new(gap: Duration) -> Self;
    pub fn offer(&mut self, now: Instant, item: T) -> Option<T>;   // Some: write it now
    pub fn due(&self) -> Option<Instant>;                          // when tick() would release the pending item
    pub fn tick(&mut self, now: Instant) -> Option<T>;
    pub fn flush(&mut self) -> Option<T>;                          // at the end of the recording
}

pub fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)>;   // from the SOF0/SOF2 marker

/// The daemon records a target only in the session's own context, never in the human's.
pub fn may_record(target_context: &str, session_context: &str, human_context: &str) -> bool;

/// Whether frames of a target may be written now: the main frame's URL is known and not
/// screen-off, and no navigation to a screen-off URL is under way (rule fixed by F5).
pub struct ScreenGate { /* current: Option<String>, pending: Option<String> */ }
impl ScreenGate {
    pub fn new(url: Option<&str>) -> Self;
    pub fn navigation_started(&mut self, url: &str);
    pub fn committed(&mut self, url: &str);
    pub fn open(&self, cfg: &RecordingConfig) -> bool;
}

/// The screen bytes one recording may still write (max_per_recording_bytes).
pub struct ScreenBudget { /* limit, used: AtomicU64, capped: AtomicBool */ }
impl ScreenBudget {
    pub fn new(limit: u64) -> Arc<Self>;
    pub fn try_take(&self, n: u64) -> bool;   // false (and capped from then on) once used + n > limit
    pub fn capped(&self) -> bool;
    pub fn used(&self) -> u64;
}

/// screen/<targetId>/: numbered JPEGs (000001.jpg, ...) and index.jsonl.
pub struct ScreenWriter { /* dir, target, next: u32, index: JsonlWriter, budget */ }
impl ScreenWriter {
    pub fn create(screen_dir: &Path, target_id: &str, budget: Arc<ScreenBudget>) -> std::io::Result<ScreenWriter>;
    /// Writes the frame and its index line; returns "<targetId>/<file>", or None when the
    /// budget is spent (a Capped marker is written once).
    pub fn write_frame(&mut self, ts: &str, url: Option<&str>, jpeg: &[u8], kind: FrameKind) -> std::io::Result<Option<String>>;
    pub fn mark(&mut self, ts: &str, kind: FrameKind, url: Option<&str>) -> std::io::Result<()>;   // Off or Capped
}
```

- Produces (`recorder.rs`):

```rust
pub struct RecorderOptions {
    pub ws_url: String,
    pub human_context: String,
    pub config: RecordingConfig,
}
pub struct Recorder { /* own CdpConn (connect_queued), sessions, targets */ }
impl Recorder {
    /// Connects on first use and reconnects (backoff up to 5 s) when the connection drops.
    pub fn start(opts: RecorderOptions) -> Arc<Recorder>;
    /// From the session manager: a target joined the session's scope (any time, also before begin_session).
    pub fn add_target(&self, id: &SessionId, target_id: &str, context: &str);
    pub fn remove_target(&self, id: &SessionId, target_id: &str);
    /// From the daemon once the action log exists: starts the screen of the session's targets.
    pub async fn begin_session(&self, id: &SessionId, context: &str, log: SessionLog, manifest: ManifestHandle, extras: Vec<RecordExtra>);
    /// Stops the streams, writes the pending (settled) frames and the screen status; idempotent.
    pub async fn end_session(&self, id: &SessionId);
}
```

- 녹화기가 탭에 붙는 순서: 자기 연결로 `Target.getTargetInfo`를 다시 물어 `may_record(그 컨텍스트, 세션 컨텍스트, 사람 컨텍스트)`가 거짓이면 붙지 않는다(그 일은 `daemon.log`에 시험 훅이 켜졌을 때만 남긴다). 붙으면 `Page.enable`, F1·F2대로 `Page.startScreencast {format: "jpeg", quality: 60, maxWidth: 1280, everyNthFrame: 1}`. 프레임마다 확인하고, `ScreenGate::open`이 참일 때만 `Throttle`에 넣는다. 게이트가 닫히는 순간 `Off` 표식을 한 번 쓰고(URL은 `url_for_log`), 닫힌 동안 받은 프레임은 메모리에서도 버린다(묶기에 남은 것 포함).
- 주 프레임 주소는 녹화기 세션의 이벤트로만 따른다(F5의 이벤트). 붙은 직후에는 `Target.getTargetInfo`의 `url`.
- 디스크 쓰기는 세션마다 하나인 쓰기 작업(`spawn_blocking` 또는 전용 스레드)이 한다. CDP 이벤트를 처리하는 쪽은 쓰기를 기다리지 않는다. 쓰기 오류는 그 프레임만 버리고 `daemon.log`에 한 줄(같은 오류는 한 번).
- 첫 프레임을 쓰면 `manifest.screen = Recorded`. 한도에 걸리면 그 세션의 모든 탭의 스트림을 멈추고 `Capped` 표식, `manifest.screen = Capped`. 동작 기록은 계속된다.
- 데몬은 `Daemon::assemble`에서 `Recorder::start`를 `SessionManager::new`보다 먼저 만들고, `create_session_task`에서 `ActionLog`를 연 직후 `begin_session`(실패해도 세션 만들기는 계속한다. 그 세션은 화면 없이 남는다), `finish_session`에서 로그를 마감하기 전에 `end_session`.
- 프록시 필터: `Page.startScreencast`, `Page.stopScreencast`를 `ALLOWED_METHODS`에서 빼고 `Page.screencastFrameAck`와 함께 새 거부 목록 `RECORDING_METHODS`에 둔다. 문구: `not allowed in a pegboard session: pegboard records the session's screen itself`. (지금은 0단계 목록을 따라 허용되어 있다. 설계는 "지금처럼 거부"라고 적었지만 코드는 허용이다.)
- 시험 훅: 코어 API `test.recordTarget {sessionId, token, targetId}` → `{"recorded": bool}`(녹화기에 그 타깃을 억지로 알리고 붙었는지 답한다).
- 코어 시험 하네스: `Shared`가 `pub recorder: Arc<Recorder>`를 쥐고 `SessionManager::new`에 넘긴다(녹화는 `begin_session`을 부르는 시험에서만 시작된다).
- CLI 시험 하네스: `common`에 `pub const SCREEN_OFF_NAME: &str = "screen-off.test"`(127.0.0.1로 풀린다, `host_rules`에 더한다). `shared_home`은 `browser start` 전에 `<home>/config.toml`에 `[recording]\nscreen_off_domains = ["screen-off.test"]`를 쓴다. `pub fn limited_home() -> &'static Home`: 같은 꼴의 두 번째 공유 홈, 설정은 `max_total_gb = 0.004`, `max_per_recording_gb = 0.002`. 시험 페이지에 `/animated`(100ms마다 색이 바뀜), `/settle`(300ms 동안 다섯 번 바뀌고 멈춘 뒤 마지막 바뀐 시각을 `window.settledAt`에), `/to-off`(302로 `http://screen-off.test:<포트>/off`), `/off`, `/popup-off`(`window.open`으로 `/off`를 연다)를 더한다.

- [ ] **Step 1: 실패하는 단위 시험을 쓴다.**

```rust
// screen.rs (tokio time paused where needed)
#[test] fn throttle_lets_two_a_second_through_and_keeps_the_last() {
    // offers at 0,100,200,...,1900 ms: released at 0, 500, 1000, 1500 (+ via tick at 2400 the 1900 one);
    // nothing released twice; flush() after the last offer gives the newest pending item
}
#[test] fn throttle_releases_the_settled_frame_after_the_gap() { /* offer 0 (out), offer 100 (held); tick 599 none; tick 600 => the 100 one */ }
#[test] fn jpeg_size_reads_baseline_and_progressive() { /* two small JPEG fixtures as byte arrays; garbage => None */ }
#[test] fn may_record_only_in_the_session_context() {
    // ("ctxA","ctxA","human") true; ("human","human","human") false; ("ctxB","ctxA","human") false; ("","ctxA","human") false
}
#[test] fn screen_gate_closes_before_an_off_page_can_paint() {
    // list [screen-off.test]: new(Some("http://a.test/")) open; navigation_started("http://screen-off.test/") => closed;
    // committed("http://screen-off.test/") closed; navigation_started("http://a.test/") still closed; committed("http://a.test/") open;
    // new(None) => closed (unknown URL); a redirect: started a.test, committed screen-off.test => closed
}
#[test] fn budget_caps_once() { /* limit 10: take 6 ok, take 5 false and capped, take 1 false */ }
#[test] fn writer_numbers_files_and_indexes_them_with_0600() {}
#[test] fn writer_stops_at_the_budget_with_one_capped_marker() {}
#[test] fn write_errors_drop_frames_only() {
    // screen dir made read-only after create: write_frame => Err; the writer is still usable after the dir is writable again
}
// filter.rs
#[test] fn screencast_methods_are_refused_for_agents() {
    // Page.startScreencast, Page.stopScreencast, Page.screencastFrameAck on an own session => Error with the text above;
    // none of them is in ALLOWED_METHODS
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.** `cargo test -p pegboard-core --lib recording::screen proxy::filter`, 그다음 `cargo test --workspace`.
- [ ] **Step 3: 실제 브라우저 시험을 쓴다** (`e2e_recording.rs`. `shared_home`과 `limited_home`. 녹화 파일은 `<home>/recordings/<id>/`에서 직접 읽는다).

```rust
#[test] #[ignore] fn screen_frames_are_recorded_by_default() {
    // session on /animated, wait 3 s, close: screen/<first target>/ has jpgs and index.jsonl;
    // every index line with a file names an existing file and vice versa; change frames <= 2 per second (+2);
    // dirs 0700, files 0600; manifest format 2, screen "recorded"
}
#[test] #[ignore] fn the_settled_screen_is_the_last_frame() {
    // /settle: the last change frame's ts >= window.settledAt (read with `eval` before closing)
}
#[test] #[ignore] fn human_profile_tab_is_never_recorded() {
    // a human-profile tab is open the whole time (as in e2e_isolation's Rig); session opens tabs and a popup;
    // screen/ holds only dirs of targets that were in the session (session info targets + the popup);
    // the human tab's target id appears in no file of the recording;
    // test.recordTarget with the human tab's id => {"recorded": false} and no dir for it
}
#[test] #[ignore] fn screen_off_domain_frames_are_never_written() {
    // open /to-off (redirect), then `tab new` /popup-off, a popup straight to /off, `tab` back and forth, `open` a normal page:
    // index lines with kind change whose url host is screen-off.test: none; an "off" marker exists for each tab that went there;
    // number of jpgs == number of index lines with a file; frames resume after the normal page
}
#[test] #[ignore] fn per_recording_cap_stops_only_the_screen() {
    // limited_home, /animated for 20 s with commands every second: a "capped" marker, manifest screen "capped",
    // screen bytes <= cap + one frame, every command after the cap is in actions.jsonl
}
#[test] #[ignore] fn hidden_tab_follows_the_measured_rule() {
    // second tab of the session is made active (tab 2), the first keeps changing: per F4 frames keep coming for the
    // first tab, or the index shows no frames for that time and the replay model (task 7) marks a gap
}
// crates/pegboard-core/tests/e2e_proxy.rs
#[test] #[ignore] fn agent_cannot_start_or_stop_the_screencast() {
    // the test starts the session's recording itself (sh.recorder.begin_session with an ActionLog in the shared home);
    // a client on the session endpoint sends Page.startScreencast and Page.stopScreencast on its own session: both refused;
    // the recording's frame count keeps growing afterwards
}
```

- [ ] **Step 4: 실패할 수 있음을 보이는 변형을 적는다** (컨트롤러가 넣고 확인하고 되돌린다). 예: `may_record`가 늘 참, `ScreenGate::open`이 늘 참, `Throttle::offer`가 늘 `Some`, `RECORDING_METHODS`를 비움, `ScreenBudget::try_take`가 늘 참. 시험마다 하나씩.
- [ ] **Step 5: 1단계 호환 시험을 다시 돌린다.** 컨트롤러가 `cargo test -p pegboard-core --test e2e_runner compatibility_matrix -- --ignored`와 `cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1`. 화면 스트림 메서드를 거부해서 깨지는 agent-browser 명령이 있으면 그 명령과 실패 출력을 보고서 맨 위에 쓰고 멈춘다(메서드를 다시 열지 않는다).
- [ ] **Step 6: 커밋.** `feat: per-tab screen recording on the daemon's own connection; agents cannot touch the screencast`

---

### Task 4: 명령 앞뒤 화면과 성능 기준

**Files:**
- Modify: `crates/pegboard-core/src/recording/recorder.rs`, `daemon.rs`, `service.rs`(시험 훅), `docs/plans/2026-11-17-phase2b-recording-design.md`(4.6절에 측정 결과), `docs/spike-findings.md`
- Delete: `crates/pegboard-core/tests/probe_2b.rs`
- Test: `recorder.rs` 단위 시험, `e2e_recording.rs`

**Interfaces:**
- Consumes: 작업 3의 `Recorder`, `ScreenWriter`, `ScreenGate`, `ScreenBudget`. 작업 1의 F3, F7.
- Produces (`recorder.rs`):

```rust
/// Longest wait for one capture. Two per command (before, after): a command waits at most 1 s
/// more for its pictures (design 4.6 worst case), less where the after-capture overlaps the probe.
pub const CAPTURE_LIMIT: Duration = Duration::from_millis(500);
/// After a capture of a target timed out, that target is not captured again until a stream
/// frame arrives from it or this long has passed (a locked screen does not cost 1 s per command).
pub const FROZEN_BACKOFF: Duration = Duration::from_secs(30);

impl Recorder {
    /// Marks the start of a command and captures `target` (bounded by CAPTURE_LIMIT).
    /// None when there is no frame (not recorded, screen-off, capped, disconnected, too slow, frozen).
    pub async fn before_command(&self, id: &SessionId, target: Option<&str>) -> Option<String>;
    /// Captures `target` and marks the end of the command (task 5 uses the marks).
    pub async fn after_command(&self, id: &SessionId, target: Option<&str>) -> Option<String>;
}
pub async fn bounded<T>(limit: Duration, f: impl Future<Output = Option<T>>) -> Option<T>;   // None on timeout
pub struct CaptureHealth { /* per target: frozen_until */ }
impl CaptureHealth {
    pub fn may_capture(&self, target: &str, now: Instant) -> bool;
    pub fn timed_out(&mut self, target: &str, now: Instant);
    pub fn frame_seen(&mut self, target: &str);
}
```

- `SessionManager`에 `pub fn recorder(&self) -> &Arc<Recorder>`. `run_command`의 서명은 바꾸지 않는다.
- `run_command` 안의 자리: 직전 캡처는 정책·주소 판정을 모두 통과한 뒤(거부된 명령에는 찍지 않는다), 대상은 그때의 `runner.bound_target(id)` 또는 세션 탭이 하나면 그 탭. 입력 대상 검사(`harmless_before`)와 `tokio::join!`으로 같이 기다린다. 직후 캡처는 명령이 끝난 뒤의 `bound_target`(바뀌었으면 새 탭)으로, 포커스 검사(`probe`)와 같이 기다린다. 결과를 `entry.frame_before`, `entry.frame_after`에 넣는다. `duration_ms`는 지금처럼 잰다(캡처 시간이 들어간다).
- 캡처: 녹화기 세션에서 F3의 인자로 `Page.captureScreenshot {format: "jpeg", quality: 60, ...}`. 캡처를 보내기 전과 답을 받은 뒤 두 번 `ScreenGate::open`을 보고, 어느 쪽이든 닫혀 있으면 버린다. `ScreenBudget`을 쓴다. `FrameKind::Before`/`After`로 같은 번호 순서에 쓴다.
- 녹화기 연결이 끊겨 있으면 기다리지 않고 바로 `None`.
- 시험 훅: `PEGBOARD_TEST_CAPTURE_DELAY_MS`(캡처마다 먼저 그만큼 잔다), 코어 API `test.setScreen {sessionId, token, on: bool}`(그 세션의 화면 기록을 잠시 끄고 켠다. 성능 비교용), `test.dropRecorder {holdMs}`(녹화기 연결을 끊고 그동안 다시 붙지 않는다).

- [ ] **Step 1: 실패하는 단위 시험.**

```rust
#[tokio::test(start_paused = true)] async fn bounded_gives_up_at_the_limit() {
    // a future that sleeps 5 s => None after exactly CAPTURE_LIMIT; one that answers in 10 ms => Some
}
#[test] fn a_frozen_target_is_skipped_until_a_frame_or_the_backoff() {
    // timed_out(t, 0) => may_capture(t, 1 s) false; frame_seen(t) => true; timed_out then 30 s later => true
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_recording.rs`).

```rust
#[test] #[ignore] fn every_command_has_frames_before_and_after() {
    // open, click, fill, get url, eval, scroll, tab new <url>, tab 1: each entry has frameBefore and frameAfter naming
    // existing files with index kinds before/after; for `tab new` the after frame is in the new tab's dir
}
#[test] #[ignore] fn screen_turned_off_by_the_test_hook_writes_no_frames() {
    // test.setScreen off: commands have no frames and no jpg is added; on again: frames come back (the "recording off/on" case of design 7)
}
#[test] #[ignore] fn a_failed_command_still_has_both_frames() { /* click '#missing' => ok false, both frames */ }
#[test] #[ignore] fn refused_commands_have_no_frames() { /* `cookies set a b` (policy_denied) and an argv_rejected line: no frames */ }
#[test] #[ignore] fn commands_on_a_screen_off_domain_have_no_frames() {
    // open /off; get url; click on the off page: entries without frames, no new jpg; `open /to-off` from a normal page: no after frame
}
#[test] #[ignore] fn a_slow_capture_never_delays_a_command_beyond_the_cap() {
    // own Home with PEGBOARD_TEST_CAPTURE_DELAY_MS=5000: `get url` succeeds, takes < baseline + 1.2 s, entry has no frames;
    // ten commands in a row: total extra < 10 * 1.2 s
}
#[test] #[ignore] fn commands_run_while_the_recorder_is_disconnected() {
    // test.dropRecorder holdMs 5000: commands succeed without frames and add < 100 ms; afterwards frames come back
}
#[test] #[ignore] fn recording_overhead_meets_the_targets() {
    // one session; commands of task 1 S2; 20 paired runs each with test.setScreen on/off alternating;
    // prints per command: median added ms, max added ms; asserts median <= 50 and max <= 1000 for every command
}
```

- [ ] **Step 4: 변형을 적는다.** 예: `CAPTURE_LIMIT`를 10초로, 직후 캡처의 게이트 재확인을 지움, 거부된 명령에도 직전 캡처를 함.
- [ ] **Step 5: 측정 결과를 문서에 적는다.** 컨트롤러가 돌린 `recording_overhead_meets_the_targets`의 표를 설계 문서 4.6절과 `spike-findings.md` "2b단계 측정"에 붙인다(명령, 중앙값, 최대, 돌린 날짜, 기기 종류만. 기기 이름은 쓰지 않는다). 기준을 넘으면 설계 4.6절대로 화질·크기를 낮춰 다시 재고, 그래도 넘으면 컨트롤러가 사용자에게 보고한다.
- [ ] **Step 6: `probe_2b.rs`를 지운다**(질문은 작업 3, 4의 시험이 이어받았다).
- [ ] **Step 7: 커밋.** `feat: frames before and after every command within a bounded wait; recording overhead measured`

---

### Task 5: 탐색 기록, 사람 개입, 네트워크·콘솔

**Files:**
- Create: `crates/pegboard-core/src/recording/events.rs`
- Modify: `recording/recorder.rs`, `recording/format.rs`(`NetworkLine`, `ConsoleLine`), `crates/pegboard-protocol/src/lib.rs`(`SessionPolicy.record`), `crates/pegboard-core/src/policy.rs`(검사), `session.rs`(데몬이 스스로 탭을 활성화할 때 녹화기에 알림), `service.rs`(`begin_session`에 `policy.record`), `crates/pegboard-cli/src/main.rs`(`--record`), `crates/pegboard-cli/src/mcp/tools.rs`(운영자 정책 스키마의 `record`), F6이 확장 신호를 쓰기로 했으면 `extension/bridge/sw.js`와 `crates/pegboard-core/src/bridge.rs`
- Test: `events.rs` 단위 시험, `policy.rs`, `e2e_recording.rs`, `crates/pegboard-cli/tests/e2e_mcp.rs`

**Interfaces:**
- Consumes: 작업 3, 4의 `Recorder`(명령 구간 표시), `SessionLog`, `ActionEntry::human`, `NavLine`, `JsonlWriter`, `url_for_log`. 작업 1의 F5, F6.
- Produces (`pegboard-protocol`): `SessionPolicy`에 `pub record: Vec<RecordExtra>`(`#[serde(default)]`). 프로필 단위 고정(`ProfilePins`)에는 넣지 않는다. `policy::normalized`가 중복을 지운다.
- Produces (`events.rs`):

```rust
/// How long after a command ends its navigations still count as the command's.
pub const GRACE: Duration = Duration::from_secs(1);

pub struct CommandWindows { /* running: bool, last_end: Option<Instant> */ }
impl CommandWindows {
    pub fn begin(&mut self, now: Instant);
    pub fn end(&mut self, now: Instant);
    pub fn covers(&self, at: Instant) -> bool;   // a command is running or ended less than GRACE ago
}

#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// A main-frame navigation committed; `requested` is the reason of the renderer's request
    /// (Page.frameRequestedNavigation) that preceded it, None when the browser started it.
    Navigated { url: String, requested: Option<String> },
    /// The tab became the active tab of its window (bridge, only if F6 says so).
    Activated,
    /// pegboard itself activated the tab (policy re-apply), so the next Activated is not the human's.
    ActivatedByPegboard,
}
/// None: not a human intervention. Some((what, certain)).
pub fn classify(signal: &Signal, in_command: bool, expected_activation: bool) -> Option<(String, bool)>;

// recorder.rs, only when F6 adopts the bridge's tab-activation signal:
impl Recorder { pub fn expect_activation(&self, target_id: &str); }   // the next Activated of this target within 5 s is pegboard's

pub fn nav_line(ts: &str, target_id: &str, event: NavEvent, url: Option<&str>, opener: Option<&str>) -> NavLine;  // url through url_for_log
```

  `classify`의 시작 표(작업 1 F6으로 고친다): 명령 구간 안이면 무엇이든 `None`. 구간 밖에서 브라우저가 시작한 탐색(`requested: None`) → `("navigated outside a command: <url_for_log>", true)`. 페이지의 요청 가운데 `reason`이 링크 클릭·폼 제출 계열 → `("followed a link or form outside a command", false)`. 스크립트·새로 고침 계열 → `None`(탐색 기록에만 남는다). `Activated`는 `expected_activation`이면 `None`, 아니면 `("switched to this tab", true)`.
- Produces (`format.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkLine { pub ts: String, pub target_id: String, pub method: String, pub url: String,
                         pub status: Option<u16>, pub bytes: Option<u64>, pub duration_ms: Option<u64>,
                         pub error: Option<String> }   // no headers, no bodies
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleLine { pub ts: String, pub target_id: String, pub level: String, pub text: String }  // text cut to 1000 chars
```

- 탐색 기록: 녹화기가 세션 탭이 들어올 때 `created`(팝업이면 `opener`), 주 프레임 커밋마다 `navigated`, 떠날 때 `closed`를 `navigation.jsonl`에 쓴다(녹화기의 자기 연결에서 `Target.setDiscoverTargets`와 녹화기 세션의 `Page` 이벤트).
- 사람 개입: `before_command`가 `CommandWindows::begin`, `after_command`가 `end`. 신호마다 `classify`, `Some`이면 `ActionEntry::human`을 세션 로그에 붙인다(로그 잠금은 별도 작업에서 기다린다. 명령 실행 쪽은 녹화기를 기다리지 않는다). `SessionManager`가 정책을 다시 걸려고 탭을 활성화하는 곳은 그 직전에 `Recorder::expect_activation(target)`을 부른다(F6이 확장 신호를 쓰지 않으면 이 함수는 만들지 않는다).
- 네트워크·콘솔: `begin_session`의 `extras`에 있을 때만, 녹화기 세션에서 `Network.enable`, `Runtime.enable`. `Network.loadingFinished`/`loadingFailed`에서 한 줄(주소는 `url_for_log`, 헤더와 본문은 쓰지 않는다). 콘솔은 `Runtime.consoleAPICalled`와 `Runtime.exceptionThrown`. 화면을 끈 도메인의 탭이어도 이 둘은 남긴다(화면만 끄는 설정이다. README에 적는다).
- `create_session_task`가 manifest의 `record`에 세션 정책의 `record`를 넣는다(`Manifest::new` 뒤에 채운다).
- CLI: `PolicyArgs`에 `--record <network,console>`(`value_delimiter = ','`). `session create`와 `mcp`가 같이 받는다. `session info`가 보여 준다. MCP 운영자 모드의 `policy` 스키마에 `record`(enum 배열). 기본 모드의 `pegboard_session_create`에는 없다.

- [ ] **Step 1: 실패하는 단위 시험.**

```rust
// events.rs
#[test] fn windows_cover_the_command_and_its_grace() { /* begin 0, end 100 ms: covers 50, 1099 true; 1101 false; running covers always */ }
#[test] fn classification_table() {
    // in_command: every signal => None
    // Navigated{requested: None} => certain true; Navigated{requested: Some("anchorClick")} => certain false;
    // Navigated{requested: Some("scriptInitiated")}, Some("reload"), Some("metaTagRefresh") => None
    // Activated with expected_activation => None; without => certain true
}
#[test] fn nav_lines_drop_query_and_userinfo() {}
#[test] fn console_text_is_cut_to_1000_chars() {}
// policy.rs
#[test] fn record_is_deduplicated_and_not_pinned() {}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_recording.rs`. 사람 입력은 시험이 자기 브라우저 연결로 흉내 낸다: 세션 엔드포인트가 아니라 데몬 밖에서 붙은 연결. 시험용 연결은 공유 홈의 `chrome/DevToolsActivePort`로 붙는다).

```rust
#[test] #[ignore] fn navigation_log_follows_tabs_and_popups() {
    // open A, tab new B, popup from B, close the popup: created/navigated/closed lines in order, the popup's opener is B,
    // no URL carries its query string
}
#[test] #[ignore] fn agent_commands_are_never_taken_for_human_ones() {
    // 20 commands: open, click on links, back, forward, reload, tab new, tab 1, fill + press Enter (form submit): no human entry
}
#[test] #[ignore] fn a_human_navigation_outside_a_command_is_recorded() {
    // 2 s after the last command, the test's own connection sends Page.navigate to the session tab:
    // one entry kind "human", certain true, with the target id; `recording show` (CLI) prints it
}
#[test] #[ignore] fn a_human_like_click_outside_a_command_is_marked_possible() {
    // the test's own connection dispatches a mouse click on a link: one human entry with certain false
}
#[test] #[ignore] fn page_script_navigation_is_not_human() {
    // the page sets location after 2 s with no command running: navigation.jsonl has it, actions.jsonl has no human entry
}
#[test] #[ignore] fn network_and_console_are_off_by_default_and_on_with_record() {
    // default session: no network.jsonl, no console.jsonl; --record network,console: both exist with lines for
    // the page's fetch and its console.log; manifest record ["network","console"]; `session info` shows it
}
#[test] #[ignore] fn network_log_keeps_no_secrets() {
    // page fetches /api?token=SECRET1 with header Authorization: Bearer SECRET2 and a cookie SECRET3:
    // network.jsonl contains none of SECRET1, SECRET2, SECRET3, "Authorization", "Cookie"
}
// e2e_mcp.rs
#[test] #[ignore] fn mcp_record_option_reaches_every_session() { /* `pegboard mcp --record console`: session info (CLI) shows record console */ }
#[test] #[ignore] fn mcp_default_mode_cannot_turn_recording_extras_on() { /* session_create {record:["network"]} => isError, nothing created */ }
```

- [ ] **Step 4: 변형을 적는다.** 예: `GRACE`를 0으로(명령 직후의 탐색이 사람으로 잡혀 `agent_commands_...`가 실패), `classify`가 `requested`를 보지 않음, `NetworkLine`에 헤더를 넣음.
- [ ] **Step 5: 커밋.** `feat: navigation log, human intervention marks, optional network and console logs`

---

### Task 6: 보존 정리와 녹화 관리 명령

**Files:**
- Create: `crates/pegboard-core/src/recording/store.rs`, `recording/retention.rs`
- Modify: `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-core/src/service.rs`(저장소, 주기, RPC), `crates/pegboard-cli/src/main.rs`, `crates/pegboard-cli/src/mcp/tools.rs`
- Test: `store.rs`, `retention.rs` 단위 시험(임시 디렉터리), `e2e_recording.rs`, `crates/pegboard-cli/src/mcp/tests.rs`, `e2e_mcp.rs`

**Interfaces:**
- Consumes: 작업 2의 `Manifest`, `ManifestHandle`, `ScreenStatus`, `Sizes`, `RecordingConfig`, `read_manifest`, `write_manifest_file`, `recording_dir`.
- Produces (`pegboard-protocol`):

```rust
pub mod codes { pub const RECORDING_RUNNING: &str = "recording_running"; }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSummary {
    pub recording_id: String, pub session_id: String, pub session_name: String, pub profile: String,
    pub started_at: String, pub ended_at: Option<String>, pub running: bool, pub keep: bool,
    pub screen: String,            // "none" | "recorded" | "capped" | "pruned"
    pub screen_bytes: u64, pub total_bytes: u64,
    pub commands: u64, pub failed: u64, pub denied: u64, pub human: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingList { pub recordings: Vec<RecordingSummary>, pub total_bytes: u64, pub kept_bytes: u64 }
```

  `failed`: `kind` 명령이고 `ok: false`이며 오류 코드가 `policy_denied`가 아닌 것. `denied`: 오류 코드가 `policy_denied`인 것. `human`: `kind: human`.
- Produces (`retention.rs`):

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Facts { pub id: String, pub started_at: OffsetDateTime, pub ended_at: Option<OffsetDateTime>,
                   pub running: bool, pub keep: bool, pub screen_bytes: u64, pub other_bytes: u64 }
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum PruneAction { Delete { id: String }, DeleteScreen { id: String } }
/// Pure. 1) not running, not kept, ended_at + max_age <= now => Delete.
/// 2) then, while the bytes of every recording that is not kept (running ones included) exceed
///    max_total_bytes: DeleteScreen of the not-running, not-kept recording with the oldest
///    ended_at that still has screen bytes. Action logs are never deleted for size.
pub fn plan(recs: &[Facts], now: OffsetDateTime, cfg: &RecordingConfig) -> Vec<PruneAction>;
```

- Produces (`store.rs`):

```rust
pub struct RecordingStore { /* paths, config, running: Mutex<HashMap<String, ManifestHandle>> */ }
impl RecordingStore {
    pub fn new(paths: Paths, config: RecordingConfig) -> Arc<RecordingStore>;
    pub fn open(&self, manifest: Manifest) -> std::io::Result<ActionLog>;          // registers it as running
    pub fn finish(&self, log: &mut ActionLog, ended_at: &str) -> std::io::Result<()>;  // writes ended_at and sizes, unregisters
    pub fn list(&self) -> std::io::Result<RecordingList>;
    pub fn set_keep(&self, id: &str, keep: bool) -> Result<RecordingSummary, RpcError>;   // running ones through their handle
    pub fn delete(&self, id: &str) -> Result<(), RpcError>;                               // recording_running for a running one
    /// Startup: a recording that is not running and has no ended_at gets the time of its last write.
    pub fn close_orphans(&self) -> std::io::Result<usize>;
    pub fn prune(&self, now: OffsetDateTime, dry_run: bool) -> std::io::Result<Vec<PruneAction>>;
}
```

- 저장소 하나의 잠금 아래에서 "진행 중" 판정과 지우기를 한다(지우는 사이에 그 녹화가 진행 중이 될 수 없게: 녹화 ID는 세션 ID에서 나오고 다시 쓰이지 않지만, 잠금으로 순서를 정한다). 지우는 것은 `recordings/` 바로 아래, 녹화 ID 꼴의 이름, `symlink_metadata`로 진짜 디렉터리인 것만. manifest를 읽을 수 없는 디렉터리는 계획에 넣지 않고 `daemon.log`에 한 줄. 화면을 지우면 `screen/`만 지우고 `manifest.screen = Pruned`, `sizes`를 다시 쓴다. `keep` 표시는 manifest에 쓴다.
- 데몬: `Daemon::assemble`에서 `RecordingStore::new`(설정은 작업 2의 `RecordingConfig::load`), 시작할 때 `close_orphans` 다음 `prune`(따로 띄운 작업에서), 1시간마다(`PRUNE_EVERY = 3600 s`, 시험 훅 `PEGBOARD_TEST_PRUNE_SECS`), `finish_session` 뒤마다 `prune`. `create_session_task`는 `ActionLog::open` 대신 `store.open`, `finish_session`과 `discard`는 `store.finish`.
- 코어 API: `recording.list` → `RecordingList`(지금의 `Vec<Manifest>`를 바꾼다). `recording.keep {recordingId, keep}` → `RecordingSummary`. `recording.delete {recordingId}`. `recording.prune {dryRun}` → `{"actions": [PruneAction]}`. `recording.get`은 그대로(`actions`에 사람 개입 항목과 프레임 이름이 실린다).
- CLI:
  - `recording list [--session <id>] [--failed] [--kept]`: 한 줄에 ID, 세션, 시작, 끝(진행 중이면 `running`), 이름, 크기, 화면(`screen`/`no screen`/`screen capped`/`screen pruned`), `kept`, 실패 수. 마지막 줄 `total <크기>, kept <크기>`. `--failed`는 실패나 거부가 하나라도 있는 것, `--kept`는 keep한 것, `--session`은 그 세션의 것(CLI가 거른다).
  - `recording show <id>`: 지금의 표에 사람 개입 줄(`<seq>  <ts>  human: <what>` 그리고 확실하지 않으면 ` (possibly)`)과, 프레임이 있으면 `frames <before> / <after>`.
  - `recording keep <id>`, `recording unkeep <id>`, `recording delete <id>`(진행 중이면 `recording_running`, 종료 코드 1), `recording prune [--dry-run]`(지운 것, 또는 지울 것을 한 줄씩).
- MCP: `pegboard_recording_list`가 화면 상태, 크기, keep을 보여 주고, `pegboard_recording_get`이 사람 개입 항목과 프레임 이름을 보여 준다(사람 개입의 `what`은 주소를 담으므로 실패 문구처럼 페이지 내용 표식 안에 넣는다). 운영자 모드에만 `pegboard_recording_keep {recordingId, keep: boolean}`과 `pegboard_recording_delete {recordingId}`. 기본 모드에는 도구 목록에 없고, 이름으로 불러도 없는 도구와 같은 오류.

- [ ] **Step 1: 실패하는 단위 시험** (임시 디렉터리에 가짜 녹화를 만든다).

```rust
// retention.rs
#[test] fn age_rule() { /* ended 14 days + 1 s ago => Delete; 14 days - 1 s => nothing; pre-2b (format 1, no screen) the same */ }
#[test] fn prune_never_deletes_a_running_or_kept_recording() {
    // a running one started 30 days ago (ended_at None, running true), a kept one ended 30 days ago with 10 GB of screen:
    // no action names either, whatever the limits
}
#[test] fn size_rule_deletes_screens_oldest_first_and_stops_under_the_limit() {
    // limit 5 GB; four finished recordings of 2 GB screen each, ended t1<t2<t3<t4: DeleteScreen t1, t2 only; no Delete
}
#[test] fn kept_bytes_do_not_count_and_running_bytes_do() {}
#[test] fn action_logs_are_never_deleted_for_size() { /* all screens gone and still over: only DeleteScreen actions */ }
// store.rs
#[test] fn an_orphan_counts_from_its_last_write() {
    // recording without ended_at, actions.jsonl mtime 20 days ago: close_orphans sets ended_at to that time; prune deletes it
}
#[test] fn an_unreadable_manifest_is_left_alone() { /* garbage manifest.json: prune keeps the dir */ }
#[test] fn only_real_recording_dirs_are_deleted() {
    // a symlink named like a recording id pointing outside, a dir "notes", a file "r_abcdefgh": all untouched; the target of the symlink untouched
}
#[test] fn delete_refuses_a_running_recording() { /* recording_running */ }
#[test] fn keep_on_a_running_recording_survives_its_finish() {}
#[test] fn pruning_the_screen_marks_the_manifest() { /* screen "pruned", sizes rewritten, actions.jsonl still there */ }
#[test] fn summary_counts() { /* failed / denied / human counted as defined */ }
// mcp/tests.rs
#[test] fn default_mode_has_no_keep_or_delete_tools() { /* not in tools/list; calling pegboard_recording_delete => unknown tool error */ }
#[test] fn operator_mode_keeps_and_deletes() { /* stub core sees recording.keep / recording.delete */ }
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험** (`e2e_recording.rs`).

```rust
#[test] #[ignore] fn old_recordings_are_pruned_at_start_and_kept_ones_stay() {
    // own Home: before `browser start`, plant three format-1 recordings (actions only): ended 15 days ago, ended 15 days ago
    // with keep true, ended 3 days ago; after start: the first is gone within 10 s, the other two stay
}
#[test] #[ignore] fn the_size_limit_deletes_finished_screens_first() {
    // limited_home: three sessions on /animated, closed one after another; one more session left running;
    // after `recording prune`: the oldest finished ones are "screen pruned" (actions kept), the running one keeps its screen
}
#[test] #[ignore] fn list_shows_sizes_screen_keep_and_filters() {
    // keep one, fail a command in another: `recording list --kept`, `--failed`, `--session` show the right ones; the total line
}
#[test] #[ignore] fn delete_refuses_a_running_recording_and_removes_a_finished_one() {}
#[test] #[ignore] fn mcp_default_mode_cannot_keep_or_delete() { /* e2e_mcp.rs: tools/list of `pegboard mcp` has neither */ }
```

- [ ] **Step 4: 변형을 적는다.** 예: `plan`의 진행 중 검사를 지움, 크기 규칙이 `Delete`를 냄, `keep`을 세지 않는 검사를 지움.
- [ ] **Step 5: 커밋.** `feat: recording retention (14 days, 5 GB, screens first) and keep, unkeep, delete, prune`

---

### Task 7: 재생 화면

**Files:**
- Create: `crates/pegboard-core/src/recording/replay/mod.rs`, `replay/model.rs`, `replay/assets/index.html`, `replay/assets/app.js`, `replay/assets/app.css`
- Modify: `crates/pegboard-core/src/service.rs`(재생 서버 시작, `recording.open`, 종료 때 `revoke_all`), `session.rs`(`SessionManager::new`가 `extra_own_ports: Vec<u16>`를 받아 `own_ports_fn`에 더한다), `crates/pegboard-core/tests/common/mod.rs`, `crates/pegboard-cli/src/main.rs`(`recording open`)
- Test: `replay` 단위 시험(브라우저 없이 서버를 띄운다), `crates/pegboard-core/tests/e2e_replay.rs`(새), `e2e_recording.rs`

**Interfaces:**
- Consumes: `actionlog::read`, `read_jsonl`, `IndexLine`, `NavLine`, `NetworkLine`, `ConsoleLine`, `Manifest`, `ScreenStatus`.
- Produces (`model.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident { pub seq: u64, pub ts: String, pub kind: &'static str /* "failed" | "denied" | "human" */,
                      pub target_id: Option<String>, pub frame_before: Option<String>, pub frame_after: Option<String> }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayModel {
    pub manifest: Manifest, pub actions: Vec<ActionEntry>, pub navigation: Vec<NavLine>,
    pub screens: BTreeMap<String, Vec<IndexLine>>,      // target id -> its index
    pub network: Vec<NetworkLine>, pub console: Vec<ConsoleLine>,
    pub incidents: Vec<Incident>,                         // in seq order
    pub start_seq: Option<u64>,                           // the first incident, else None (start at the beginning)
}
pub fn build(paths: &Paths, recording_id: &str) -> std::io::Result<ReplayModel>;
/// The frame to show for a tab at `ts`: the latest index line with a file at or before ts,
/// unless an Off marker or a navigation of that tab came after it (then None: "화면 기록 꺼짐" / "화면 없음").
pub fn frame_at<'a>(index: &'a [IndexLine], navigation: &[NavLine], target: &str, ts: &str) -> FrameAt<'a>;
pub enum FrameAt<'a> { Frame(&'a IndexLine), Off, Missing }
```

  `frame_at`은 Rust에서 시험하고, `app.js`는 같은 규칙을 그대로 옮긴다(규칙을 바꾸면 둘 다).
- Produces (`replay/mod.rs`):

```rust
pub const TOKEN_TTL: Duration = Duration::from_secs(30 * 60);
pub struct ReplayServer { /* port, grants: Mutex<HashMap<String, Grant { recording_id, expires: Instant }>>, paths */ }
impl ReplayServer {
    /// Binds 127.0.0.1:0 once, at daemon start.
    pub async fn start(paths: Paths) -> anyhow::Result<Arc<ReplayServer>>;
    pub fn port(&self) -> u16;
    /// A new token for one recording: "http://127.0.0.1:<port>/r/<token>/" and when it expires.
    pub fn grant(&self, recording_id: &str) -> (String, OffsetDateTime);
    pub fn revoke_all(&self);
    #[doc(hidden)]
    pub async fn start_with_ttl(paths: Paths, ttl: Duration) -> anyhow::Result<Arc<ReplayServer>>;  // tests
}
#[derive(Debug, PartialEq)]
pub enum Route { Page, Asset(&'static str), Data, Frame { target: String, file: String }, NotFound }
/// Pure: the path after /r/<token>/. Target ids are 32 hex characters, files 6 digits + ".jpg";
/// anything else (.., %2e, a slash too many, a query) is NotFound.
pub fn route(rest: &str) -> Route;
```

- 서버 규칙: `GET`과 `HEAD`만. `Host`가 정확히 `127.0.0.1:<포트>`가 아니면 거부. 토큰은 32바이트 난수(base32 소문자), 대조는 `subtle::ConstantTimeEq`. 없는 토큰, 만료된 토큰, 지워진 녹화, `NotFound`는 모두 같은 `404`(본문 `not found`). 프레임은 그 녹화의 `screen/<target>/<file>`만, `symlink_metadata`로 일반 파일인 것만. `Data`는 `model::build`의 JSON. 모든 응답 헤더: `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, 그리고 맞는 `Content-Type`. 시험 훅 `PEGBOARD_TEST_REPLAY_TTL_SECS`.
- 페이지(`assets/`, `include_str!`로 넣는다. 외부 자원 없음): 설계 4.8의 화면 구성(머리줄: 세션 이름, 시작–끝, 명령 수, 실패 수, 거부 수 / 화면 / 동작 목록 / 탭 고르기 / 직전·직후 / 시간축). 열면 `start_seq`의 동작을 고르고 그 직전 화면을 보인다. 시간축의 사고 표시를 누르면 직전, 한 번 더 누르면 직후. 화면 상태가 `pruned`면 "화면 지워짐", `capped` 뒤면 "화면 기록 멈춤(녹화 하나 1GB)", `FrameAt::Off`면 "화면 기록 꺼짐", `Missing`이면 "화면 없음". 모든 글은 `textContent`로 넣는다. `app.js`에 `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval(`, `new Function`, `setTimeout("`를 쓰지 않는다. 키보드: 왼쪽·오른쪽 화살표로 이전·다음 동작(세부는 맡긴다).
- 코어 API `recording.open {recordingId}` → `{"url", "expiresAt"}`(없는 녹화는 `recording_not_found`). CLI `recording open <id>`: 주소를 받아 `/usr/bin/open <url>`을 실행하고 `opened <id> in the default browser (the link works for 30 minutes)`를 찍는다. 실패하면 주소를 찍고 종료 코드 1. 시험 훅 `PEGBOARD_TEST_OPEN_LOG=<파일>`이면 `open` 대신 그 파일에 주소를 한 줄 쓴다.
- 재생 포트는 `own_ports`에 들어간다: 요청 가드와 `open`/`tab new`의 사전 판정이 에이전트 세션에서 거부한다(`address::OWN_PORT`).

- [ ] **Step 1: 실패하는 단위 시험** (브라우저 없음. 임시 홈에 가짜 녹화를 만들고 `ReplayServer::start`를 띄워 `std::net::TcpStream`으로 요청한다).

```rust
#[test] fn routes() {
    // "" => Page; "app.js" => Asset; "data" => Data; "frames/<32 hex>/000001.jpg" => Frame;
    // (target ids are 32 upper-case hex characters, as Chrome makes them)
    // "frames/../x", "frames/%2e%2e/x", "frames/<32 hex>/1.jpg", "frames/<32 lower-case hex>/000001.jpg",
    // "frames/<32 hex>/000001.jpg/x", "data?x=1" => NotFound
}
#[test] fn model_finds_the_first_incident() {
    // actions ok, failed(seq 3), denied(seq 5), human(seq 7): incidents [3,5,7], start_seq 3; all ok => start_seq None
}
#[test] fn frame_at_rules() { /* latest frame before ts; Off marker after it => Off; a navigation after it => Missing; nothing => Missing */ }
#[tokio::test] async fn a_token_opens_only_its_recording() {
    // grant(r_a): its page, data and a frame of r_a => 200; a frame path naming r_b's target (exists only in r_b) => 404;
    // data never contains r_b's actions; a made-up token => 404
}
#[tokio::test] async fn an_expired_token_is_refused() { /* start_with_ttl(1 s): 200 at once, 404 after 1.1 s */ }
#[tokio::test] async fn revoke_all_ends_every_token() {}
#[tokio::test] async fn paths_outside_the_recording_are_refused() {
    // a symlink screen/<target>/000002.jpg -> a file outside: 404; "frames/<target>/..%2f..%2fmanifest.json": 404
}
#[tokio::test] async fn a_foreign_host_header_is_refused() { /* Host: evil.test:<port>, Host: localhost:<port>, no Host => refused */ }
#[tokio::test] async fn only_get_and_head() { /* POST, PUT => 405 or 404, never 200 */ }
#[tokio::test] async fn responses_carry_the_security_headers() {}
#[test] fn assets_never_parse_html() { /* app.js and index.html contain none of the forbidden calls; index.html has no inline script or style and no http(s):// URL */ }
#[test] fn data_keeps_redacted_values_redacted() {
    // an action with args ["[redacted]"] and redacted true: the data JSON has "[redacted]" and nothing else in its place
}
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인.**
- [ ] **Step 3: 실제 브라우저 시험.**

```rust
// crates/pegboard-core/tests/e2e_replay.rs (shared core harness; the page is opened in a human-profile tab through the
// test's own browser connection; the guard does not judge the human profile)
#[test] #[ignore] fn replay_page_starts_at_the_first_incident() {
    // planted recording with frames: the selected action is the first incident; its "before" frame <img> has loaded
    // (naturalWidth > 0); clicking its timeline mark twice shows the "after" frame
}
#[test] #[ignore] fn replay_page_shows_page_text_as_text() {
    // recording whose session name, a URL, an error text and a human `what` hold `<img src=x onerror="window.pwned=1">`:
    // window.pwned is undefined, no <img> other than frames, the literal text is visible (document.body.innerText contains it)
}
#[test] #[ignore] fn replay_page_marks_screen_off_missing_and_pruned() {}
// e2e_recording.rs
#[test] #[ignore] fn recording_open_gives_a_loopback_link_for_that_recording() {
    // PEGBOARD_TEST_OPEN_LOG: `recording open <id>` exit 0, the logged URL is http://127.0.0.1:<port>/r/<token>/; GET of it => 200;
    // a second `recording open` gives another token; the default browser was not started
}
#[test] #[ignore] fn agent_session_cannot_reach_the_replay_server() {
    // with a live replay URL: `open <url>` => policy_denied; `eval fetch(<url>)` fails; the same in sessions with
    // --allow-private-network and with --allow-domain 127.0.0.1; through MCP (pegboard_open) too
}
#[test] #[ignore] fn tokens_end_with_the_daemon() { /* URL from `recording open`; browser stop; browser start: the old URL => 404 or refused */ }
```

- [ ] **Step 4: 변형을 적는다.** 예: `route`가 `..`를 허용, `grant`가 녹화 ID를 확인하지 않고 경로의 ID를 씀, `app.js`의 한 곳을 `innerHTML`로, `own_ports_fn`에서 재생 포트를 뺌.
- [ ] **Step 5: 커밋.** `feat: replay page on a loopback one-time link (pegboard recording open)`

---

### Task 8: 문서, 수동 확인, 전체 확인

**Files:**
- Modify: `README.md`, `docs/architecture.md`(8절, 11절, 12절의 미확인 가정 가운데 풀린 것), `docs/roadmap.md`(2단계, 2b), `docs/plans/2026-11-17-phase2b-recording-design.md`(상태와 바뀐 제안 값)
- Create: `docs/phase2b-manual-checks.md`

- [ ] **Step 1: README.**
  - "상태": 2b 완료 상태와 자동 시험 날짜.
  - "빠른 시작"의 CLI: `recording list`, `recording show`, `recording open`, `recording keep`.
  - 새 절 "녹화": 무엇을 남기는가(4.1 표), 저장 위치와 권한, `--record network,console`, `config.toml` 예와 기본값(키 네 개, GB는 10^9바이트, 바꾸면 데몬을 다시 띄워야 한다, 잘못 쓰면 `browser start`가 2로 끝난다), 보존 규칙(화면부터, `keep`은 한도 밖, 사람이 `recording delete`로 지운다), 재생(기본 브라우저, 30분 링크).
  - "아직 안 되는 것": 화면 녹화 줄을 지우고 `recording export`(동영상)와 네트워크 본문 기록을 적는다.
  - "알려진 한계"에 "녹화 (2b)" 절: **R10**(카드 번호·일회용 코드 입력란의 숫자, 사람이 에이전트 창에서 직접 로그인하는 과정, "비밀번호 보기"를 누른 화면은 그대로 찍힌다. 녹화는 로컬에만 있고 14일 뒤 지워진다), 2b를 처음 켜는 날 14일이 지난 기존 동작 기록이 지워진다, 화면 끄기는 주 프레임 주소 기준이라 다른 페이지 안의 iframe으로 뜬 그 도메인은 찍힌다, 화면을 끈 도메인에서도 동작 기록과 탐색 기록과(켰다면) 네트워크·콘솔 기록은 남는다, 콘솔 기록에는 페이지가 찍은 글이 그대로 남는다, 작업 1 F4가 남긴 "화면 없음" 상태, 사람 개입은 추정이다(F6), 코어 API의 `recording.keep/delete/open`은 토큰이 필요 없다(소켓 소유자 검사만, 2a 운영자 메서드와 같은 수준).
  - "시험": 새 명령(`cargo test -p pegboard-cli --test e2e_recording -- --ignored --test-threads=1`, `cargo test -p pegboard-core --test e2e_replay -- --ignored --test-threads=1`)과 새 시험용 환경 변수(`PEGBOARD_TEST_CAPTURE_DELAY_MS`, `PEGBOARD_TEST_PRUNE_SECS`, `PEGBOARD_TEST_REPLAY_TTL_SECS`, `PEGBOARD_TEST_OPEN_LOG`)와 시험용 메서드(`test.recordTarget`, `test.setScreen`, `test.dropRecorder`).
- [ ] **Step 2: architecture.md 8절.** "화면 기록 방식"에 녹화기의 자기 연결(F1)과 이유(프레임이 데몬 주 연결의 이벤트 방송을 채우지 않게), 묶기 규칙, 앞뒤 캡처의 제한 시간과 멈춘 탭 건너뛰기, 측정 결과 요약(F4, F7, F8). "위험"의 [미확인]을 측정 결과로 바꾼다. "저장 형식과 위치"를 형식 2로(`keep`, `screen`, `record`, `sizes`, 프레임 이름 `<targetId>/<file>`, 표식 줄 `off`/`capped`, `actions.jsonl`의 `kind: human`). `network.har`를 `network.jsonl`로. "재생"에서 `recording export`를 2b 밖으로. "보존 기간"에 화면부터 지우는 규칙, 녹화 하나 1GB, 고아 녹화, 설정 키. "민감 정보 처리"에 R10과 네트워크 헤더를 기록하지 않는 것.
- [ ] **Step 3: architecture.md 11절.** "위협과 대응"의 "녹화가 유출된다" 줄에 재생 서버의 토큰과 자기 포트. "녹화 데이터 접근 통제"에 재생 링크(녹화 하나, 30분, 루프백, `Host` 확인)와 에이전트가 화면 스트림 메서드를 쓸 수 없는 것. "남는 위험"에 R10과 Step 1의 한계 가운데 보안에 닿는 것.
- [ ] **Step 4: roadmap.md.** 2b 완료 상태(자동 시험 이름과 함께), 2b가 남긴 것(`recording export`, 네트워크 본문, 화면 기록 전체 끄기 설정이 필요한지), 진행도 줄 갱신.
- [ ] **Step 5: `docs/phase2b-manual-checks.md`** (2a 문서의 꼴: 한눈에 보기 표, 항목마다 목적·절차·기대 결과·실제 결과 칸).
  - P1. 사람이 `recording open`으로 실패가 있는 녹화를 열어 첫 사고 위치에서 시작하는지, 시간축의 사고 표시로 직전·직후 화면을 오가는지, 탭을 바꿀 수 있는지.
  - P2. 에이전트 창을 가리거나 최소화한 채 명령을 돌린 녹화의 화면.
  - P3. 화면을 잠근 채(그리고 다른 Space로 옮긴 채) 명령을 돌린 녹화의 화면과 명령 시간.
  - P4. 사람이 에이전트 창에서 주소창 입력, 링크 클릭, 탭 전환을 한 뒤 재생 화면의 사람 개입 표시.
  - P5. 화면을 끈 도메인을 설정하고 그 사이트에서 일한 녹화에 그 화면이 없는지 눈으로.
- [ ] **Step 6: 설계 문서.** 상태 줄에 구현 완료, 작업 1과 4에서 바꾼 제안 값(화질, 크기, 제한 시간 500ms씩, 사람 개입 판정표)과 이유를 해당 절에 적는다.
- [ ] **Step 7: 전체 확인.** `cargo fmt --check && cargo clippy --workspace --tests -- -D warnings && cargo test --workspace`, 그리고 컨트롤러가 `cargo test --workspace --no-fail-fast -- --ignored --test-threads=1`. 실패하는 시험은 느슨하게 고치지 않는다. 고칠 수 없는 한계면 README "알려진 한계"와 architecture.md 11절에 적고 `#[ignore = "known limit: ..."]`를 붙인 뒤 컨트롤러가 사용자에게 알린다. 빠른 시작의 명령은 컨트롤러가 한 번씩 실제로 돌려 본 것만 남긴다.
- [ ] **Step 8: 커밋.** `docs: recording and replay (phase 2b), manual checks`

---

## 작업 순서와 의존

```
1 (측정) ─> 2 (형식, 설정) ─> 3 (화면 스트림) ─> 4 (앞뒤 화면, 성능) ─> 5 (탐색, 사람 개입, 선택 기록) ─┐
                         └──────────────────────────────────────> 6 (보존, 관리 명령) ─────────────────┼─> 7 (재생) ─> 8 (문서)
```

- 1의 멈춤 조건(M1~M5)에 걸리면 사용자가 정할 때까지 3 이후를 시작하지 않는다. 2는 측정과 무관해 먼저 해도 된다.
- 6은 2만 있으면 시작할 수 있지만, 3~5와 같은 파일(`service.rs`)을 고치므로 차례로 한다.
- 7은 6의 요약(`RecordingList`)을 쓰지 않지만, 화면 상태 `pruned`의 표시를 시험하려면 6이 먼저다.
- 작업마다 컨트롤러가 두 번째 기기에서 `#[ignore]` 시험과 변형을 돌린 결과가 돌아와야 그 작업이 끝난다.

## 설계와의 차이, 이 계획이 정한 것

- **화면 스트림 메서드는 지금 에이전트에게 열려 있다.** 설계는 "에이전트 쪽 필터는 `Page.startScreencast`를 지금처럼 거부한다"고 적었지만 `proxy/filter.rs`의 `ALLOWED_METHODS`에는 0단계 목록을 따라 `Page.startScreencast`와 `Page.stopScreencast`가 있다. 작업 3이 둘을 거부 목록으로 옮긴다.
- **제한 시간.** 설계 4.2는 캡처 하나에 1초, 4.6은 명령 하나의 최악 1초다. 둘 다 지키려고 캡처 하나 500ms(앞뒤 합쳐 1초), 시간을 넘긴 탭은 30초 동안 건너뛰기로 정했다.
- **스트림을 거는 곳.** 설계 4.2는 포커스 유지용 세션에서 건다고 적었다. 그 세션은 데몬의 주 연결(`browser.conn`, 이벤트 방송 1024칸) 위에 있어 프레임이 세션 관리자의 이벤트 처리를 밀어낼 수 있으므로, 녹화기의 자기 연결을 기본으로 하고 작업 1(F1)에서 프레임이 오는지 확인한다.
- **프레임 이름**은 `screen/` 기준 `<targetId>/<file>`이다(설계 예시의 `"000031.jpg"`는 탭이 여럿일 때 모호하다).
- **`manifest.screen`**은 설계의 bool 대신 `none`/`recorded`/`capped`/`pruned`다("화면 지워짐", "화면 기록 멈춤" 표시에 필요).
- **네트워크 기록에는 헤더를 쓰지 않는다**(설계는 메타데이터를 주소, 상태, 크기, 시간으로 정하고 세 헤더는 늘 가린다고 했다. 헤더를 아예 쓰지 않으면 둘 다 지켜진다).
- **"녹화 켜짐/꺼짐" 시험**은 사용자 설정이 아니라 시험 훅(`test.setScreen`)으로 한다. 설계에는 화면 기록 전체를 끄는 설정이 없다.
- **`recording open`의 `open` 실행**은 데몬이 아니라 CLI가 한다(사람의 프로세스에서 사람의 기본 브라우저를 연다).
- **용량 한도에 세는 것**: `keep`하지 않은 모든 녹화(진행 중 포함). 지우는 대상은 끝난 것만.
