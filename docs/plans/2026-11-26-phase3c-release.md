# 3c단계: 배포 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 개발 기기의 `browser/scripts/release.sh canary <N>` 하나가 시험 훅 없는 배포 빌드를 서명·공증해 DMG로 깃허브 릴리스와 서명된 appcast에 올리고, 설치한 Pegboard.app이 Sparkle 2로 canary/stable 업데이트를 검증해 받아 데몬과 함께 다시 뜬다.

**Architecture:** 빌드·묶기·서명·공증·게시는 단계로 나뉜 셸 스크립트(`release.sh`)이고 단계마다 끝 표시를 남겨 이어서 할 수 있다. Chromium 배포 빌드는 `out/Release`(배포 GN 인자)에 따로 두고, 앱 사본에 `pegboard`(배포 판, 데몬 시험 훅 꺼짐), agent-browser, `Sparkle.framework`, 고지를 넣고 `Info.plist`를 고친 뒤 안쪽부터 서명한다. 업데이터는 `//pegboard/updater`가 실행 중에 Sparkle 번들을 읽어 `MainParts`에서 시작하고(업스트림 줄 0), appcast는 `gh-pages`의 파일 하나를 `appcast.py`가 고치고 Sparkle `sign_update`가 서명한다.

**Tech Stack:** bash, Python 3 표준 라이브러리(`xml.etree`, `plistlib`), Rust(기존 워크스페이스), Chromium Extended Stable `156.0.8078.109`(C++, Objective-C++, GN/ninja), Sparkle 2.9 이상, `codesign`/`notarytool`/`stapler`/`hdiutil`/`spctl`, `gh`, agent-browser 0.42.0.

**Spec:** [2026-11-26-phase3c-release-design.md](./2026-11-26-phase3c-release-design.md). 앞 문서: [3b 설계](./2026-11-25-phase3b-browser-ui-design.md)와 [3b 계획](./2026-11-25-phase3b-browser-ui.md), [3a 설계](./2026-11-21-phase3a-fork-skeleton-design.md), [architecture.md](../architecture.md) 5절 "빌드와 배포 파이프라인".

## Global Constraints

- 설계 2절의 실패 조건이 하나라도 생기면 그 작업은 끝난 것이 아니다: 서명·공증 안 된 바이트가 게시된다, 나쁜 업데이트(EdDSA 틀림, 다른 팀·임시 서명, 더 낮은 버전, HTTPS 아님, 서명 안 된 피드)를 받아들인다, 에이전트·페이지가 피드나 업데이트를 고른다, 배포 빌드에서 시험 훅이 켜진다, 업데이트 뒤 데몬이 없거나 사람 프로필 자료가 사라진다, 업데이트 뒤 키체인이 묻는다, 비밀이나 이름이 저장소에 들어간다.
- 3b 계획 Global Constraints의 보장 목록을 약하게 만들지 않는다(확인 창 규칙, 시험 훅은 `pegboard_test_hooks` 빌드에만, 데몬에는 자동 답 훅 없음, `argv::check`, 가림 규칙, 프록시 기본 거부, 요청 가드, 사람 프로필에 아무것도 걸지 않기).
- **에이전트는 Apple 자격 증명을 다루지 않는다.** `xcrun notarytool store-credentials`, Sparkle `generate_keys`, 깃허브 Pages 켜기, 키체인의 "항상 허용"과 키체인 암호 입력은 사용자가 한다. 그 단계에서 에이전트는 멈추고 정확한 명령을 사용자에게 주고 끝났다는 답을 기다린다. 공증 프로필 이름은 `pegboard-notary`.
- **저장소에 쓰지 않는 것**: 서명 ID의 이름(사람 이름이 들어 있다), 팀 ID, 깃허브 소유자 이름, Apple ID, EdDSA 비밀 키, 홈 디렉터리 경로, 사용자 이름, 기기 이름. 실행 때 찾는다(`security find-identity`, `gh repo view`). EdDSA **공개** 키만 `browser/release/sparkle-public-ed-key`에 커밋한다.
- 버전: 빌드 번호 `N`(1부터, 모든 채널이 함께 쓰고 늘기만 함), 보이는 버전 `<Chromium 버전>-pb.<N>`, 태그 `v<보이는 버전>`, `CFBundleVersion` = `<BUILD>.<PATCH>.<N>`(156.0.8078.109, N=3 → `8078.109.3`), `CFBundleShortVersionString`은 바꾸지 않는다.
- `Info.plist`의 Sparkle 키는 설계 4.2 표 그대로(`SUFeedURL` https Pages 주소, `SUPublicEDKey`, `SUEnableAutomaticChecks` YES, `SUAutomaticallyUpdate` YES, `SUScheduledCheckInterval` 86400, `SUVerifyUpdateBeforeExtraction` YES, `SURequireSignedFeed` YES). `SUAllowsDowngrades`, `SUEnableJavaScript`, `SUAllowedURLSchemes`는 없다.
- 채널: stable = Sparkle 기본 채널, canary = `canary`. 앱의 선택은 사용자 기본값 `PegboardUpdateChannel`(`canary`가 아니면 안정). appcast는 `gh-pages`의 `appcast.xml` 하나, DMG는 깃허브 릴리스 자산(canary는 프리릴리스).
- 배포 빌드는 `browser/args.release.gn` → `out/Release`. `args.gn`과 다른 줄은 `pegboard_test_hooks = false` 하나. 새 pegboard 홈 기본값 `~/Library/Application Support/Pegboard Agent`, 브라우저 사용자 데이터는 그대로 `~/Library/Application Support/Pegboard`.
- **빌드는 언제나 `-j 8`**(`build.sh` 경유, 더 높이면 개발 기기가 커널 패닉으로 멈췄다), 도구 시간 제한에 끊기지 않게 `nohup … &`로 떼어 돌리고 로그와 끝 표시를 본다. 빈 상태 빌드 약 2시간 45분. `release.sh`도 같다. 떼어 돌리기 전에 `release.sh preflight`를 앞에서 돌려 키체인 창에 사람이 답하게 한다.
- **C++ 작업(9~11)은 개발 기기의 Chromium 트리**(`156.0.8078.109`, 브랜치 `pegboard-work`)에서 `browser/scripts/{apply,export,build,package-dev,budget}.sh`로 한다. 패치 커밋을 고치기 전에 맨 끝의 생성 커밋 `rebrand-strings (generated)`를 빼고, 다시 만드는 것은 `apply.sh`다(`browser/README.md` "패치 고치기"). 바꾼 `//pegboard`는 `out/Pegboard`(개발)와 `out/Release`(배포) 둘 다 증분 빌드한다.
- **개발 기기에서 브라우저를 켜지 않는다.** 앱 실행과 실제 브라우저 시험은 시험 기기의 GUI 세션에서 한다: 원격 셸에서 바로 돌리지 않고 GUI 세션의 터미널 앱이 명령 파일을 실행하게 하는 방식(3a부터 쓴 것), 화면 잠금 풀림(잠겨 있으면 사용자에게 부탁), 원격 데스크톱 연결 끊음, 남은 Pegboard 인스턴스 끔, `--test-threads=1`. 화면을 보는 확인은 그 기기의 agent-desktop(화면 사진, 클릭, 접근성 값)으로 한다. 개발 빌드를 띄우는 시험은 언제나 `--use-mock-keychain`, 임시 `PEGBOARD_HOME`. 키체인을 재는 배포 빌드 실행만 예외다(작업 12).
- **Rust·파이썬·셸 작업의 구현 에이전트**는 코드, 브라우저 없는 시험(`cargo test --workspace`, `python3 browser/scripts/test_*.py`), `cargo fmt --check`, `cargo clippy --workspace --tests -- -D warnings`, `shellcheck browser/scripts/*.sh scripts/*.sh`까지 하고, 보고서에 시험 기기에서 돌릴 명령 그대로와 기대 결과, 새 실제 시험마다 변형 하나를 적는다. 컨트롤러가 시험 기기에서 돌린다.
- 모든 C++ 변경에는 변형 확인("이 줄을 이렇게 바꾸면 시험 X가 실패한다")을 적고 컨트롤러가 빌드해 확인한다. 작업 6, 8, 9는 구현 검토와 별도로 **보안 검토자**가 본다: 업데이트 경로의 무결성, 키 다루기, 내리기 공격, appcast·자산이 HTTPS인지, 배포 빌드에 자동 승인 길이 없는지.
- 업스트림 손 패치: 3b 끝 15개 파일, 120줄. 3c 증가 목표 15줄 이하(도움말 화면 패치 10줄 이하). 패치마다 `budget.sh` 값을 보고서에 적는다.
- 문서는 한국어. 커밋 메시지는 영어, 끝에 `Co-Authored-By: <구현한 모델 이름> <noreply@anthropic.com>`. 푸시와 main 병합은 컨트롤러가 한다. 깃허브 릴리스와 `gh-pages` 푸시는 계획의 게시 단계(작업 8의 시험 게시, 13, 14)에서만, 컨트롤러가 한다.

## Review Focus

설계가 암시하지만 시험이 놓치기 쉬운 것. 각 줄의 시험은 괄호 안 작업에 넣었다.

1. **나쁜 업데이트를 받아들인다.** 다른 키의 EdDSA, 맞는 EdDSA에 임시 서명 앱, 더 낮은 `sparkle:version`, 서명 틀린 피드, `http://` 주소 어느 것도 설치되지 않고, 게시 쪽도 그런 항목을 만들지 않는다(작업 8 `test_add_refuses_*`, 작업 13 네 리허설 항목).
2. **피드를 바꿀 수 있다.** `defaults write app.pegboard.browser SUFeedURL <다른 주소>`, 환경 변수, `--user-data-dir`로 띄운 인스턴스, CDP 어느 것으로도 업데이터가 다른 피드를 읽거나 업데이트를 시작하지 않는다(작업 9 `user_defaults_feed_url_is_ignored`, `no_updater_with_a_user_data_dir`).
3. **배포 빌드에 시험 훅이 남는다.** 배포 앱에 `PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_DIALOG_ANSWER=*=approve`와 데몬 훅(`PEGBOARD_TEST_IP_SPACE_OVERRIDES` 등)을 주어도 확인은 사람을 기다리고 사설망 차단은 그대로다(작업 2 `release_build_ignores_test_hooks`, 작업 11 `a_release_launcher_passes_no_test_env`, 작업 12 `e2e_release::release_app_has_no_dialog_hooks`).
4. **업데이트 뒤 데몬이 없다.** "설치 후 다시 시작"과 종료 때 설치 둘 다에서, 새 앱이 뜬 뒤 20초 안에 새 판 데몬(`browser.status.version`)이 답하고 종료 코드 4가 없다(작업 14 단계, 작업 9 `relaunch_sets_session_restore`).
5. **홈 옮기기가 자료를 잃거나 섞는다.** 옛 데몬이 도는 중, 새 홈이 이미 있음, `PEGBOARD_HOME`이 있음, 옮기다 중간에 실패(대상이 이미 있음)에서 아무것도 지우지 않고 다시 시도하면 끝난다. 브라우저의 항목(`Default`, `Local State`, `PegboardAgent*`)은 그대로다(작업 1 `migration_*`).

## 파일 구조

| 파일 | 할 일 | 작업 |
|---|---|---|
| `crates/pegboard-core/src/paths.rs`, `lock.rs`, `daemon.rs`(시작) | 새 홈 기본값, 옛 홈 옮기기, 업로드 거부 목록 | 1 |
| `crates/pegboard-cli/src/main.rs` | 옮기기 부름, `--version`, 심볼릭 링크로 부른 자기 위치 | 1, 2 |
| `crates/pegboard-core/src/testenv.rs`, `runner.rs`(`find_agent_browser`), `service.rs`(`browser.status`) | 배포 빌드의 시험 훅 끄기, 앱 안 agent-browser, 버전 | 2 |
| `crates/pegboard-protocol/src/lib.rs` | `BrowserStatus.version` | 2 |
| `browser/args.release.gn`, `browser/scripts/{build.sh,lib.sh}`, `browser/scripts/test_release_args.sh` | 배포 GN 인자, 산출 디렉터리 고르기 | 3 |
| `scripts/fetch-sparkle.sh`, `vendor/sparkle.lock`, `scripts/fetch-agent-browser.sh`, `.gitignore` | 동봉물 고정과 확인 | 4 |
| `browser/release/{config.sh,notices.sh,sparkle-public-ed-key,README.md}` | 설정, 고지 모으기, 공개 키, 사용자가 한 번 할 일 | 4, 5 |
| `browser/scripts/release.sh`, `browser/scripts/release-sign.sh`, `browser/scripts/test_release.sh` | 단계, 서명, 서명 없이 도는 검사의 시험 | 5, 6, 7, 8 |
| `browser/scripts/appcast.py`, `browser/scripts/test_appcast.py` | appcast 더하기·올리기·지우기·검사, 버전 계산 | 6, 8 |
| `browser/pegboard/updater/{BUILD.gn,updater.h,updater_mac.mm,sparkle_interface.h,version_updater_disabled.{h,cc}}` | Sparkle 업데이터, 메뉴, 채널, 도움말 화면 상태 | 9, 10 |
| `browser/pegboard/browser_parts/main_parts.{h,cc}`, `BUILD.gn` | 업데이터 시작·끝 | 9 |
| `browser/patches/pegboard-about-update.patch`(필요하면) | `VersionUpdater::Create`(맥) | 10 |
| `browser/pegboard/ui/confirm_dialog.cc`, `daemon_notice.cc`, `browser/pegboard/bootstrap/daemon_launcher.cc` | 버튼 모양, 배포 빌드의 데몬 환경 | 11 |
| `crates/pegboard-core/tests/{e2e_updater.rs,e2e_release.rs,e2e_branding.rs,e2e_confirm.rs,e2e_bootstrap.rs}` | 실제 시험 | 9~12 |
| `docs/…`, `README.md`, `browser/README.md` | 결과, 설치, 배포 | 14 |

---

### Task 1: pegboard 홈 나누기와 옛 홈 옮기기 (Rust)

**Files:**
- Modify: `crates/pegboard-core/src/paths.rs`, `crates/pegboard-core/src/lock.rs`, `crates/pegboard-cli/src/main.rs`(데몬·CLI가 홈을 만들기 전), `crates/pegboard-core/src/daemon.rs`(호스팅 데몬의 시작)
- Test: `paths.rs`의 `#[cfg(test)]`, `crates/pegboard-cli/tests/`(종료 코드)

**Interfaces:**
- Produces:

```rust
// paths.rs
pub const HOME_DIR_NAME: &str = "Pegboard Agent";       // ~/Library/Application Support/<this>
pub const LEGACY_HOME_DIR_NAME: &str = "Pegboard";      // the fork's user data directory
/// Names pegboard owned in the legacy home, moved one by one (design 4.8).
pub const LEGACY_ENTRIES: &[&str] = &["sessions", "recordings", "chrome", "agent-browser-home",
    "state.json", "config.toml", "daemon.log"];
pub enum Migration { NotNeeded, Moved { entries: Vec<String> }, LegacyDaemonRunning }
/// Only when PEGBOARD_HOME is unset (callers pass `None` then); `support` is ~/Library/Application Support.
pub fn migrate_legacy_home(support: &Path) -> std::io::Result<Migration>;
```

규칙(설계 4.8): 새 홈이 있거나 옛 위치에 `state.json`·`config.toml`·`recordings`·`sessions`가 하나도 없으면 `NotNeeded`. 옛 `daemon.lock`을 `flock(LOCK_EX|LOCK_NB)`로 못 잡으면 `LegacyDaemonRunning`(아무것도 바꾸지 않음). 잡으면 새 홈을 임시 이름(`Pegboard Agent.moving`, 0700)으로 만들어 항목을 `rename`하고, 끝나면 임시 이름을 새 홈으로 `rename`한다(중간에 죽으면 다음 시작이 `.moving`에서 이어 간다). `pegboardd.sock`, `daemon.lock`은 지운다. 옛 위치에 `pegboard-home-moved.txt`를 남긴다. 목록 밖의 이름은 건드리지 않는다. 대상에 같은 이름이 이미 있으면 그 항목을 옮기지 않고 오류(지우지 않는다).
- CLI: `LegacyDaemonRunning`이면 `browser_failed` "an older pegboard daemon is running; quit Pegboard and try again". 호스팅 데몬(`daemon --cdp-pipe`)은 종료 코드 4(3a의 "잠금을 쥔 다른 데몬", 알림 없음). `Moved`면 `daemon.log`에 한 줄.
- `upload_forbidden`: 새 홈, `chrome_udd`, 브라우저 사용자 데이터(`…/Pegboard`), 그 캐시 짝을 모두 담는다(지금 목록의 `support.join("Pegboard")`는 그대로).

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn default_home_is_pegboard_agent()                         // paths_for(HOME=/u, no PEGBOARD_HOME).home == /u/Library/Application Support/Pegboard Agent
#[test] fn migration_moves_only_pegboards_entries()                 // 옛 위치에 LEGACY_ENTRIES 전부 + Default/, Local State, PegboardAgent-x/, Crashpad/ → 앞의 것만 새 홈에, 뒤의 것은 그대로, 내용 같음, 새 홈 0700
#[test] fn migration_is_not_needed_without_pegboard_files()         // Default/, Local State만 → NotNeeded, 새 홈 없음
#[test] fn migration_waits_for_a_running_legacy_daemon()            // 옛 daemon.lock을 시험이 쥠 → LegacyDaemonRunning, 아무 파일도 안 움직임
#[test] fn migration_resumes_after_an_interrupted_move()            // "Pegboard Agent.moving"에 sessions만 있고 나머지는 옛 위치 → 다음 부름이 끝냄
#[test] fn migration_never_overwrites()                             // .moving에 이미 state.json, 옛 위치에도 state.json → Err, 두 파일 그대로
#[test] fn migration_leaves_a_note_and_drops_socket_and_lock()
#[test] fn upload_forbidden_covers_both_dirs_and_their_caches()
#[test] fn pegboard_home_env_skips_migration()                      // main.rs 쪽: PEGBOARD_HOME이 있으면 migrate_legacy_home을 부르지 않는다
#[test] fn hosted_daemon_exits_4_while_a_legacy_daemon_runs()       // CLI 통합: 임시 HOME, 옛 lock 쥠 → `pegboard daemon --cdp-pipe` 종료 코드 4
```

- [ ] **Step 2: `cargo test -p pegboard-core paths:: && cargo test -p pegboard-cli legacy` → FAIL 확인.**
- [ ] **Step 3: 구현한다.** 시험에서 `HOME`을 바꾸는 곳은 기존 `paths.rs` 시험의 방식(임시 디렉터리)을 따른다. `PEGBOARD_HOME`을 쓰는 기존 시험은 그대로 통과해야 한다.
- [ ] **Step 4: `cargo test --workspace`, clippy, fmt 통과.**
- [ ] **Step 5: 커밋** `feat: pegboard's home moves to its own directory, apart from the browser's user data`

---

### Task 2: 버전, 배포 빌드의 데몬 시험 훅, 앱 안의 CLI (Rust)

**Files:**
- Modify: `crates/pegboard-core/src/testenv.rs`, `crates/pegboard-core/src/runner.rs`(`find_agent_browser`), `crates/pegboard-core/src/service.rs`(`browser.status`), `crates/pegboard-protocol/src/lib.rs`, `crates/pegboard-cli/src/main.rs`(`--version`, `spawn_daemon`)
- Test: 같은 파일의 `#[cfg(test)]`, `crates/pegboard-cli/tests/`

**Interfaces:**
- Produces:

```rust
// pegboard-core
/// "<Chromium>-pb.<N>" when built with PEGBOARD_RELEASE_VERSION set (release.sh), else
/// "<CARGO_PKG_VERSION>-dev".
pub fn version() -> &'static str;
pub fn is_release_build() -> bool;            // option_env!("PEGBOARD_RELEASE_VERSION").is_some()
// testenv.rs: var(name) is None in a release build, whatever the environment says.
pub(crate) fn var_in(release: bool, hooks_on: bool, value: Option<String>) -> Option<String>;
// pegboard-protocol BrowserStatus
#[serde(default)] pub version: String,       // the daemon's version()
// runner.rs: also <canonical exe dir>/agent-browser when the canonical exe is
// <X>.app/Contents/Helpers/pegboard (the bundle layout); still no other directory.
```

- `pegboard --version`은 `pegboard <version()>`. `pegboard browser status`가 데몬 버전을 한 줄 더 보인다.
- `spawn_daemon`과 자기 위치를 쓰는 곳은 `current_exe()?.canonicalize()`를 쓴다(심볼릭 링크로 불렀을 때 앱 안을 가리키게).

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```rust
#[test] fn release_build_ignores_test_hooks()          // var_in(true, true, Some("1")) == None; var_in(false, true, Some("1")) == Some("1"); var_in(false, false, …) == None
#[test] fn dev_version_says_dev()                      // PEGBOARD_RELEASE_VERSION 없이 빌드한 시험에서 version().ends_with("-dev")
#[test] fn browser_status_carries_the_daemon_version() // 서비스 시험: status.version == version()
#[test] fn agent_browser_is_found_beside_pegboard_in_an_app_bundle() // 임시 X.app/Contents/Helpers/{pegboard,agent-browser}, 실행 파일 경로를 주입하는 내부 함수로
#[test] fn agent_browser_is_not_taken_beside_a_bare_binary()          // /tmp/x/{pegboard,agent-browser} (앱 번들이 아님) → 찾지 않음
#[test] fn cli_through_a_symlink_finds_its_bundle()    // CLI 통합: 임시 앱 번들 안 pegboard로의 링크로 `--version`과 자기 위치 출력(숨은 진단 하위 명령 없이, 내부 함수 시험으로 충분하면 그것)
```

- [ ] **Step 2: `cargo test --workspace` → FAIL 확인.**
- [ ] **Step 3: 구현한다.** `testenv::var`를 쓰는 모든 곳(`grep -rn "testenv::var" crates`)이 그대로 동작해야 한다. 배포 빌드 판정은 컴파일 시점 값 하나이고, 실행 때 끄고 켤 방법을 두지 않는다.
- [ ] **Step 4: 배포 판을 한 번 빌드해 확인:** `PEGBOARD_RELEASE_VERSION=156.0.8078.109-pb.0 cargo build --release -p pegboard-cli && target/release/pegboard --version` → `pegboard 156.0.8078.109-pb.0`. `PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_IDLE_SECS=1`을 주고 띄운 순정 데몬이 1초에 세션을 닫지 않는 것은 작업 12에서 시험 기기가 본다(보고서에 명령).
- [ ] **Step 5: `cargo test --workspace`, clippy, fmt, 커밋** `feat: versions, test hooks compiled out of release builds, the CLI inside the app`

---

### Task 3: 배포 빌드 설정과 첫 배포 빌드 (개발 기기, 떼어 돌림)

가장 긴 일(빈 상태 약 2시간 45분)이므로 계획의 맨 앞에서 시작해 작업 1, 2, 4~8과 겹쳐 돌린다. 작업 1, 2보다 먼저 해도 된다.

**Files:**
- Create: `browser/args.release.gn`, `browser/scripts/test_release_args.sh`
- Modify: `browser/scripts/build.sh`, `browser/scripts/lib.sh`, `browser/README.md`(배포 빌드 한 단락)

**Interfaces:**
- Produces: `build.sh <src> [dev|release]`(기본 `dev`). `dev` = `browser/args.gn` → `out/Pegboard`(지금 그대로), `release` = `browser/args.release.gn` → `out/Release`. 마지막 줄은 지금처럼 `== <app 경로>`. `lib.sh`의 `OUT_DIR`은 기본값으로 남기고 `out_dir_for <kind>`, `args_for <kind>`를 더한다. `PEGBOARD_BUILD_JOBS`가 8보다 크면 거부한다(지금은 양수만 본다).
- `test_release_args.sh`: 두 인자 파일의 주석을 뺀 줄을 비교해 다른 줄이 정확히 `pegboard_test_hooks = true` ↔ `pegboard_test_hooks = false` 하나인지, 두 파일 모두 `disable_fieldtrial_testing_config = true`, `dcheck_always_on = false`, `is_debug = false`, `target_cpu = "arm64"`이고 `is_official_build`가 없는지.

- [ ] **Step 1: `test_release_args.sh`를 쓰고 `args.release.gn`이 없어 실패하는지 본다.**
- [ ] **Step 2: `args.release.gn`, `build.sh`, `lib.sh`를 고친다. `bash browser/scripts/test_release_args.sh` PASS, `shellcheck` 통과, `PEGBOARD_BUILD_JOBS=12 build.sh <src> release` → 거부.**
- [ ] **Step 3: 배포 빌드를 떼어 시작한다:** `nohup browser/scripts/build.sh <src> release > <로그> 2>&1 &`. 15분마다 로그 끝을 보고(`ninja` 진행 수), 끝나면 `== built in …`과 단계 수를 기록한다. 빌드 중 개발 기기에서 다른 Chromium 빌드를 돌리지 않는다(`out/Pegboard` 증분은 이 빌드가 끝난 뒤).
- [ ] **Step 4: 끝나면 확인:** `out/Release/args.gn`에 `pegboard_test_hooks = false`, `strings "<앱>/Contents/Frameworks/Pegboard Framework.framework/Pegboard Framework" | grep -c PEGBOARD_TEST_DIALOG_ANSWER` → 0. 같은 명령이 `out/Pegboard`에서는 1 이상(변형: 훅이 빠졌음을 이 비교가 잡는다). 디스크(`du -sh out/Release`) 기록.
- [ ] **Step 5: 커밋** `build(browser): release build arguments and output directory (no test hooks)`

---

### Task 4: 동봉물 고정과 라이선스 고지

**Files:**
- Create: `scripts/fetch-sparkle.sh`, `vendor/sparkle.lock`, `browser/release/notices.sh`, `browser/release/config.sh`
- Modify: `scripts/fetch-agent-browser.sh`, `.gitignore`(`vendor/sparkle/`), `browser/scripts/test_release.sh`(새로)

**Interfaces:**
- Produces:
  - `scripts/fetch-sparkle.sh`: `SPARKLE_VERSION`(스크립트 맨 위 상수, 2.9 이상 중 2주 이상 지난 가장 새 판을 이 작업에서 정한다)의 깃허브 릴리스 `Sparkle-<판>.tar.xz`를 받아 `vendor/sparkle.lock`(SHA-256 한 줄)과 비교하고 `vendor/sparkle/`에 푼다(`Sparkle.framework`, `bin/`, `LICENSE`). 잠금 파일이 비어 있으면 값을 적고 **"review and commit"**을 출력하며 0이 아닌 코드로 끝난다(처음 고정은 사람이 본다).
  - `fetch-agent-browser.sh --locked`: 잠금 파일이 비었으면 실패(배포는 이 꼴로만 부른다). 패키지의 `LICENSE`와 함께 `NOTICE*`, `THIRD_PARTY*`, 라이선스 디렉터리(있는 것 전부, 내용은 읽지 않는다)를 `vendor/agent-browser/licenses/`에 둔다.
  - `browser/release/notices.sh <out-dir>`: `<out-dir>/Notices/`에 `Pegboard-LICENSE.txt`(저장소 `LICENSE`), `agent-browser/`(위 파일 전부), `Sparkle/LICENSE`, `README.txt`(Chromium과 그 제3자 코드의 고지는 `chrome://credits`에 있다는 한국어·영어 한 줄씩).
  - `browser/release/config.sh`: `NOTARY_PROFILE=pegboard-notary`, `CANARY_CHANNEL=canary`, `CHECK_INTERVAL=86400`, `APPCAST_FILE=appcast.xml`, `APPCAST_KEEP=20`, `PAGES_WAIT_SECS=600`, `MAX_BUILD_JOBS=8`.

- [ ] **Step 1: `test_release.sh`에 시험을 쓴다**(셸 함수 하나에 하나, 네트워크 없이 가짜 파일로): `sparkle_lock_mismatch_fails`, `empty_sparkle_lock_records_and_fails`, `agent_browser_locked_refuses_an_empty_lock`, `notices_has_every_license`(가짜 vendor 디렉터리 → 네 항목). 돌려 실패 확인.
- [ ] **Step 2: 구현한다.** 받기는 `curl -fsSL --proto '=https'`, 압축 풀기는 새 빈 임시 디렉터리에서.
- [ ] **Step 3: 실제로 한 번 받는다:** `scripts/fetch-sparkle.sh`(잠금 기록) → 받은 판, SHA-256, 깃허브 릴리스 페이지의 값(있으면)과 Sparkle 개발자 서명(`codesign -dv vendor/sparkle/Sparkle.framework`의 팀)을 보고서에 적는다. 컨트롤러가 보고 잠금 파일을 커밋한다. `fetch-agent-browser.sh --locked`가 지금 잠금으로 통과.
- [ ] **Step 4: `bash browser/scripts/test_release.sh`, `shellcheck` 통과. `chrome://credits`가 배포 빌드에 있는지는 작업 12가 본다.**
- [ ] **Step 5: 커밋** `build: pinned Sparkle, a locked agent-browser fetch, license notices for the app`

---

### Task 5: 사용자가 한 번 할 일과 `release.sh preflight` (멈추고 묻기)

**Files:**
- Create: `browser/scripts/release.sh`(뼈대와 `preflight`), `browser/release/README.md`, `browser/release/sparkle-public-ed-key`(사용자 단계 뒤)
- Modify: `browser/scripts/test_release.sh`

**Interfaces:**
- Produces: `release.sh <canary|stable> <N>`와 `release.sh preflight`. 단계 이름 `preflight build package notarize dmg publish promote`, 작업 디렉터리 `<repo>/target/release-pegboard/<N>/`, 끝 표시 `stages/<단계>.done`, `PEGBOARD_RELEASE_FROM=<단계>`, `PEGBOARD_SRC`(없으면 멈춤), `PEGBOARD_SIGN_IDENTITY`(선택, SHA-1). 함수 `sign_identity()`(Developer ID Application이 정확히 하나면 그 SHA-1, 아니면 멈춤. 이름을 출력하거나 파일에 쓰지 않는다), `repo_slug()`(`gh repo view --json nameWithOwner`), `feed_url()`(`https://<owner>.github.io/<repo>/appcast.xml`, 소유자는 소문자로).
- `preflight`가 보는 것(하나라도 안 되면 무엇을 해야 하는지 한 줄씩 말하고 실패): 서명 ID 하나, 임시 파일에 `codesign --timestamp -o runtime -s <SHA-1>` 성공, `xcrun notarytool history --keychain-profile pegboard-notary` 성공, `vendor/sparkle/bin/sign_update`로 임시 파일 서명 성공(키체인의 EdDSA 키), `browser/release/sparkle-public-ed-key`가 그 키의 공개 키와 같음(`generate_keys -p`), `gh auth status`, Pages가 `gh-pages` 브랜치에서 켜져 있고 HTTPS 강제(`gh api repos/<slug>/pages`), 동봉물 잠금, `N`이 정수이고 지금 appcast의 가장 큰 `N`보다 큼(appcast가 아직 없으면 1부터).

- [ ] **Step 1: 시험을 쓴다**(`test_release.sh`, 가짜 `security`/`gh`/`xcrun`을 `PATH` 앞에 둬서): `two_identities_need_PEGBOARD_SIGN_IDENTITY`, `identity_name_is_never_printed`(가짜 `security` 출력의 이름 문자열이 로그에 없다), `stage_order_and_resume`(`PEGBOARD_RELEASE_FROM=dmg`면 앞 단계의 `.done`이 있어야 함), `n_must_grow`, `build_jobs_above_8_are_refused`, `stable_only_runs_promote`. 실패 확인.
- [ ] **Step 2: 뼈대와 `preflight`를 구현한다. `test_release.sh`, `shellcheck` 통과.**
- [ ] **Step 3: 멈추고 사용자에게 부탁한다**(컨트롤러가 그대로 전한다. 에이전트는 이 명령을 대신 돌리거나 그 출력의 비밀을 받지 않는다):
  1. `xcrun notarytool store-credentials pegboard-notary` — Apple ID와 앱 암호(또는 App Store Connect API 키)와 팀 ID를 묻는다. 프로필은 로그인 키체인에 저장된다.
  2. `vendor/sparkle/bin/generate_keys` — EdDSA 키를 로그인 키체인에 만들고 공개 키를 출력한다. 공개 키 한 줄을 알려 주면 된다(비밀이 아니다). 백업은 `generate_keys -x <파일>`로 사용자가 안전한 곳에(정하는 것은 사용자).
  3. 깃허브 저장소 설정 → Pages → 원본 `gh-pages` 브랜치(루트), "Enforce HTTPS" 켜기. 빈 `gh-pages` 브랜치는 컨트롤러가 먼저 만들어 푸시한다(`appcast.xml` 없이 `index.html` 한 줄).
  4. `browser/scripts/release.sh preflight`를 터미널에서 직접 한 번 돌려 키체인 창(코드 서명 키, Sparkle 키)에 "항상 허용"을 고른다.
- [ ] **Step 4: 사용자가 준 공개 키를 `browser/release/sparkle-public-ed-key`에 넣고, 사용자가 다시 `preflight`를 돌려 PASS(키체인 창 없이)인지 확인한다. `browser/release/README.md`에 위 네 단계와 키를 잃었을 때의 뜻(설계 8절)을 적는다.**
- [ ] **Step 5: 커밋** `build: release.sh preflight and the one-time steps a person does (notary profile, update key, Pages)`

---

### Task 6: 묶기와 서명 (`package`, 보안 검토 대상)

**Files:**
- Create: `browser/scripts/release-sign.sh`, `browser/scripts/appcast.py`(이 작업에서는 `version` 하위 명령만), `browser/scripts/test_appcast.py`
- Modify: `browser/scripts/release.sh`(`package`), `browser/scripts/test_release.sh`

**Interfaces:**
- Consumes: 작업 2의 `PEGBOARD_RELEASE_VERSION`, 작업 3의 `out/Release`, 작업 4의 동봉물·고지, 작업 5의 `sign_identity`, `feed_url`
- Produces:
  - `appcast.py version --chromium 156.0.8078.109 --n 3` → `{"display": "156.0.8078.109-pb.3", "bundle_version": "8078.109.3", "tag": "v156.0.8078.109-pb.3"}`(JSON 한 줄).
  - `release-sign.sh <app> <identity-sha1>`: 설계 4.3의 순서로 서명. Chromium 부분은 트리의 서명 도구(빌드의 `Pegboard Packaging/sign_chrome.py` 또는 같은 부품 목록, 설계 3절 접근 방식 A/B)로, 우리 것(`Sparkle.framework`의 `Autoupdate`·`Updater.app`·프레임워크, `Helpers/agent-browser`, `Helpers/pegboard`)은 그 전에. 모두 `--timestamp -o runtime`, `--deep` 없음. 엔타이틀먼트는 트리의 파일에서 프로비저닝 프로필이 필요한 것을 뺀 사본(작업 디렉터리에 만들고 뺀 키를 로그에 적는다).
  - `package` 단계: 앱 복사(`ditto`) → `Helpers/pegboard`(`PEGBOARD_RELEASE_VERSION=<display> cargo build --release -p pegboard-cli`), `Helpers/agent-browser`, `Frameworks/Sparkle.framework`(`XPCServices` 지움), `Resources/Notices` → `Info.plist`(설계 4.2 표) → `release-sign.sh` → 검증(설계 4.2 끝: `codesign --verify --strict --deep -vv`, 모든 Mach-O의 팀 ID·runtime 플래그, `Info.plist` 표, 금지 키 없음, `SUFeedURL`이 `https://`, 훅 문자열 없음).
- `--feed rehearsal`(작업 13 전용): `SUFeedURL`을 `…/rehearsal/appcast.xml`로. 작업 디렉터리 이름에 `rehearsal-`가 붙는다.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```python
def test_version_scheme()                  # 156.0.8078.109, 3 → 위 JSON; n=0, n="3a", 버전 꼴이 아님 → 오류
def test_bundle_version_grows_with_n_and_chromium()   # (8078.109, 3) < (8078.109, 4) < (8078.120, 5) < (8150.20, 6) — Sparkle 기본 비교(점으로 나눈 수)로
```

`test_release.sh`: `plist_has_exactly_the_sparkle_keys`(가짜 앱 `Info.plist`에 `package`의 plist 함수만 돌림 → 표 그대로, 금지 키 없음), `plist_refuses_a_non_https_feed`, `verify_rejects_an_unsigned_helper`(가짜 앱 안 실행 파일 하나를 서명 안 한 채 → 검증 실패, 임시 서명 `-s -`로 만든 앱으로), `verify_rejects_missing_runtime_flag`.
- [ ] **Step 2: 돌려 FAIL 확인.**
- [ ] **Step 3: 구현한다.** 트리의 서명 도구가 156에서 Chromium 이름 빌드에 맞는지 먼저 본다(`out/Release/Pegboard Packaging/` 생성 타깃과 설정 클래스). 맞지 않으면 B로 가고 이유를 보고서에. 트리의 앱 엔타이틀먼트를 읽어 프로필이 필요한 키 목록을 보고서에 적는다(설계 5절 위험 1).
- [ ] **Step 4: 개발 기기에서 실제로 서명한다**(브라우저는 켜지 않는다): `PEGBOARD_SRC=<src> release.sh canary 1`을 `PEGBOARD_RELEASE_FROM=package`와 함께 `package`까지만(`PEGBOARD_RELEASE_TO=package`, 이 작업에서 더한다). 배포 빌드가 아직이면 `out/Pegboard`의 사본으로 서명 경로만 확인하고(그 앱은 게시하지 않는다), 배포 빌드가 끝나면 다시. 검증 출력, 서명에 걸린 시간, 뺀 엔타이틀먼트를 기록. 서명한 앱을 시험 기기로 옮겨 GUI 세션에서 한 번 켜(`--user-data-dir=<임시> --use-mock-keychain`) 뜨는지와 데몬이 답하는지(공증 전이라 격리 속성 없이 옮긴다). 안 뜨면 `log show --predicate 'process == "amfid" OR subsystem == "com.apple.securityd"'`의 거부 이유를 보고 고친다.
- [ ] **Step 5: 보안 검토자가 본다**(서명 ID를 다루는 법과 출력, 안쪽부터 순서, `--deep` 없음, 엔타이틀먼트를 넓히지 않았는지, `Info.plist`의 키, 피드가 HTTPS, 리허설 피드가 운영 빌드에 섞이지 않는지). 반영 뒤 시험 통과, 커밋 `build: release package stage — bundle, Info.plist and inside-out Developer ID signing`

---

### Task 7: 공증, DMG, 스테이플 (`notarize`, `dmg`)

**Files:**
- Modify: `browser/scripts/release.sh`, `browser/scripts/test_release.sh`

**Interfaces:**
- Consumes: 작업 6의 서명한 `Pegboard.app`
- Produces: `notarize` 단계(설계 4.4: zip → `notarytool submit --keychain-profile "$NOTARY_PROFILE" --wait --output-format json` → 제출 ID를 `notary/app.json`에 기록 → `Accepted`가 아니면 `notarytool log <id>`를 `notary/app-log.json`에 받고 멈춤 → `stapler staple` → `stapler validate`, `spctl -a -t exec -vv`의 출처 `Notarized Developer ID`), `dmg` 단계(`dmg-root/`에 앱과 `Applications` 링크 → `hdiutil create -volname Pegboard -format ULFO` → `codesign --timestamp -s` → 공증(`notary/dmg.json`) → 스테이플 → `spctl -a -t open --context context:primary-signature -vv`). 결과 `Pegboard-<display>.dmg`와 `Pegboard-<display>.dmg.sha256`.
- 다시 할 때: 기록된 제출 ID가 이미 `Accepted`면 다시 내지 않고 스테이플부터.

- [ ] **Step 1: 시험을 쓴다**(`test_release.sh`, 가짜 `xcrun`): `notarize_stops_on_invalid_and_keeps_the_log`, `notarize_does_not_resubmit_an_accepted_id`, `dmg_has_the_app_and_an_applications_link`(가짜 앱으로 `hdiutil`을 실제로 돌려 붙이고 목록 확인. 서명·공증은 가짜). 실패 확인.
- [ ] **Step 2: 구현, 시험·`shellcheck` 통과.**
- [ ] **Step 3: 실제 공증**(배포 빌드로 묶은 `N=1` 작업 디렉터리에서, 떼어 돌림): `nohup … PEGBOARD_RELEASE_FROM=notarize PEGBOARD_RELEASE_TO=dmg release.sh canary 1 &`. 공증 시간 두 번, DMG 크기, `spctl` 출력을 기록. 이 DMG는 아직 게시하지 않는다(작업 14가 같은 단계를 다시 쓴다).
- [ ] **Step 4: 시험 기기에서 Gatekeeper를 본다:** DMG를 옮기고 격리 속성을 붙여(`xattr -w com.apple.quarantine "0081;<16진 시각>;Safari;" <DMG>`) GUI 세션에서 열고, 앱을 `/Applications`로 복사해 켠다. agent-desktop 화면 사진으로 "확인되지 않은 개발자"나 "손상됨" 창이 없고 macOS의 보통 "인터넷에서 받은 앱" 확인만 있는지. `spctl -a -vv /Applications/Pegboard.app` 결과 기록. 끝나면 지운다(작업 12가 다시 설치한다).
- [ ] **Step 5: 커밋** `build: notarize, staple and a signed, notarized DMG`

---

### Task 8: appcast와 게시 (`publish`, `promote`, 보안 검토 대상)

**Files:**
- Modify: `browser/scripts/appcast.py`, `browser/scripts/test_appcast.py`, `browser/scripts/release.sh`, `browser/scripts/test_release.sh`

**Interfaces:**
- Consumes: 작업 7의 DMG, 작업 5의 `repo_slug`, `feed_url`, 작업 4의 `config.sh`
- Produces:

```text
appcast.py add     --appcast F --n N --chromium V --bundle-version B --display D --url U --length L
                   --ed-signature S --min-system M --channel {stable,canary} --app-feed-url A --feed-url P
appcast.py promote --appcast F --n N
appcast.py remove  --appcast F --n N
appcast.py check   --appcast F --feed-url P --slug O/R     # 모든 항목이 규칙에 맞는지, 0/1
appcast.py max-n   --appcast F                            # 없으면 0
```

규칙(설계 4.5): `add`는 같은 `N`, 기존 어느 항목보다 크지 않은 `sparkle:version`(채널과 상관없이), `https://github.com/<O>/<R>/releases/download/`로 시작하지 않는 `--url`, 빈 서명·길이, `--app-feed-url ≠ --feed-url`(리허설 빌드)을 거부한다. 항목 꼴은 설계 4.5(canary만 `sparkle:channel`, `description`에 릴리스 링크). 최근 `APPCAST_KEEP`(20)개만. `promote`는 채널 표시만 지운다(다른 값은 바이트까지 같음). 출력 XML은 `sign_update`가 서명할 수 있게 UTF-8, 끝 줄 바꿈 하나. `N`은 각 항목의 `pegboard:n` 요소(자기 네임스페이스 `https://pegboard.invalid/appcast`)로 둔다.
- `publish` 단계: 설계 4.5의 다섯 단계. `gh release create <tag> <DMG> --prerelease --title <display> --notes-file <작업 디렉터리의 notes.md>`(버전, Chromium 버전, SHA-256, 고지 목록) → 자산 다시 받아 SHA-256 비교 → `gh-pages` 작업 사본(`<작업 디렉터리>/pages`, `git worktree`)에서 `appcast.py add` → `sign_update appcast.xml` → `appcast.py check` → 커밋 `appcast: <display> (<channel>)`·푸시 → Pages 주소를 `PAGES_WAIT_SECS`까지 다시 받아 바이트 비교.
- `promote`: `appcast.py promote` → 서명 → `check` → 푸시 → 다시 받기 → `gh release edit <tag> --prerelease=false --latest`.

- [ ] **Step 1: 실패하는 시험을 쓴다.**

```python
def test_add_writes_a_canary_item()                   # 항목의 모든 요소와 속성, sparkle:channel=canary
def test_add_writes_a_stable_item_without_channel()
def test_add_refuses_a_lower_or_equal_version()       # 기존 8078.109.3이 있으면 8078.109.3, 8078.109.2 거부 (canary 위에 stable도)
def test_add_refuses_a_repeated_n()
def test_add_refuses_non_https_or_foreign_urls()      # http://…, https://example.com/…, 다른 소유자의 github 주소
def test_add_refuses_a_rehearsal_build()              # --app-feed-url …/rehearsal/appcast.xml
def test_add_refuses_missing_signature_or_length()
def test_promote_only_drops_the_channel()             # 나머지 바이트 같음
def test_remove_drops_the_item()
def test_keeps_the_newest_twenty()
def test_check_finds_a_hand_edited_bad_item()         # http 주소를 손으로 넣은 파일 → 1
def test_max_n()
```

`test_release.sh`: `publish_stops_when_the_downloaded_asset_differs`(가짜 `gh`/`curl`), `publish_does_not_push_a_feed_that_fails_check`, `promote_needs_a_canary_item`.
- [ ] **Step 2: FAIL 확인.**
- [ ] **Step 3: 구현, 시험 통과.**
- [ ] **Step 4: 서명된 appcast 하나를 실제로 만들어 본다**(개발 기기, 게시하지 않음): 임시 파일에 가짜 항목 하나 → `sign_update` → 파일 끝의 서명 주석 확인 → `appcast.py check` 통과 → 한 글자 고친 뒤 Sparkle의 검증 도구가 있으면 그것으로(없으면 작업 13의 리허설이) 실패를 본다. 서명 주석이 붙은 뒤 `appcast.py`가 다시 쓰면 주석을 지우고 다시 서명해야 함을 `publish`가 지키는지 시험으로 고정(`publish_resigns_after_any_change`).
- [ ] **Step 5: 보안 검토자가 본다**(내리기 방지가 채널을 넘어 성립하는지, 주소 검사, 게시 순서와 실패 때 멈춤, 서명 키 사용이 `sign_update`뿐인지, 리허설 분리, Pages가 HTTPS인지). 반영 뒤 커밋 `build: appcast tooling and the publish and promote stages`

---

### Task 9: Sparkle 업데이터 (C++, 보안 검토 대상)

배포 빌드(작업 3)가 끝난 트리에서 한다. 바꾼 `//pegboard`는 `out/Pegboard`과 `out/Release` 둘 다 증분 빌드한다.

**Files:**
- Create: `browser/pegboard/updater/{BUILD.gn,updater.h,updater_mac.mm,sparkle_interface.h}`, `crates/pegboard-core/tests/e2e_updater.rs`
- Modify: `browser/pegboard/browser_parts/{main_parts.h,main_parts.cc,BUILD.gn}`, `browser/pegboard/BUILD.gn`(주석), `browser/scripts/package-dev.sh`(`PEGBOARD_DEV_SPARKLE=1`이면 개발 앱에도 Sparkle을 넣고 임시 서명. 시험용)

**Interfaces:**
- Consumes: 작업 4의 `vendor/sparkle/Sparkle.framework`, 작업 6의 `Info.plist` 키
- Produces:

```cpp
namespace pegboard {
// Sparkle 2 for the app (design 4.6). Starts only when <app>/Contents/Frameworks/Sparkle.framework
// exists and the command line has no --user-data-dir (test hooks builds: PEGBOARD_TEST_UPDATER=1
// lifts the second rule). Loads the framework at run time; adds "업데이트 확인…" and
// "업데이트 채널" (안정 / 카나리) to the application menu; channel from the user default
// PegboardUpdateChannel ("canary" → {"canary"}, else {}); never supplies a feed URL.
class Updater {
 public:
  static std::unique_ptr<Updater> MaybeStart();   // nullptr when it does not start
  ~Updater();                                      // stops the updater, removes the menu items
};
}  // namespace pegboard
// MainParts: PostBrowserStart → updater_ = Updater::MaybeStart(); PostMainMessageLoopRun → reset.
```

- 위임자 `updaterWillRelaunchApplication:`: Local State의 "종료 때 지난 세션 다시 열기" 값(156의 이름을 트리에서, Chrome의 `chrome::AttemptRestart`가 켜는 것)을 참으로 하고 바로 쓴다(`CommitPendingWrite`).
- 시험 훅(시험 훅 빌드에만): `PEGBOARD_TEST_UPDATER_LOG=<파일>`에 한 줄 JSON씩 `{"kind":"updater","started":bool,"reason"?, "feedURL", "channels":[…], "menu":[항목 제목들], "automaticChecks", "automaticDownloads", "interval"}`(시작할 때와 메뉴·채널이 바뀔 때). Sparkle이 피드를 실제로 받으러 가지 않게 시험은 `SUScheduledCheckInterval`을 크게 둔 앱을 쓰고, 피드 주소는 기록으로만 본다.

- [ ] **Step 1: 시험을 쓴다** (`e2e_updater.rs`, `#[ignore]`, `PEGBOARD_TEST_APP=<PEGBOARD_DEV_SPARKLE=1로 묶은 개발 앱>`, 앱의 `Info.plist`에 시험이 작업 6의 키(운영과 다른 `https://pegboard.invalid/appcast.xml`)를 넣고 임시 서명):

```rust
#[tokio::test] #[ignore] async fn updater_starts_with_sparkle_and_reads_the_plist() // PEGBOARD_TEST_UPDATER=1 → started, feedURL == Info.plist 값, automaticChecks/Downloads true, interval 86400
#[tokio::test] #[ignore] async fn no_updater_with_a_user_data_dir()                  // 훅 없이(--user-data-dir 줌) → 기록 started:false reason "user-data-dir", 메뉴 항목 없음
#[tokio::test] #[ignore] async fn no_updater_without_sparkle()                       // package-dev 기본(Sparkle 없음) → started:false reason "no-framework", 앱은 보통대로 뜨고 데몬이 답한다
#[tokio::test] #[ignore] async fn user_defaults_feed_url_is_ignored()                // 앱의 기본값 영역에 SUFeedURL=https://evil.invalid → 기록 feedURL은 Info.plist 값. 시험이 끝나면 기본값을 지운다
#[tokio::test] #[ignore] async fn channel_menu_sets_the_default_and_the_channels()   // 기본값 없음 → channels [] ; PegboardUpdateChannel=canary로 띄우면 ["canary"]; 메뉴에 "업데이트 확인…", "업데이트 채널"
#[tokio::test] #[ignore] async fn relaunch_sets_session_restore()                    // 시험 훅이 위임자의 relaunch 처리 함수를 부른다(PEGBOARD_TEST_UPDATER_RELAUNCH=1) → Local State의 그 값이 true
```

기본값을 쓰는 시험은 시험 기기의 `app.pegboard.browser` 기본값을 바꾸므로 시작 전 값을 저장하고 끝에 되돌린다.
- [ ] **Step 2: 컴파일 확인(`cargo test --no-run`).**
- [ ] **Step 3: 트리에서 구현하고 두 산출 디렉터리를 증분 빌드한다(떼어 돌림, `-j 8`).** Sparkle을 읽는 것이 hardened runtime 라이브러리 검증에 걸리는지는 작업 12가 서명한 배포 앱에서 본다. 메뉴를 156이 다시 만드는 것으로 드러나면(설계 5절 위험 6) 다시 더하는 길을 찾고, 업스트림 줄이 필요하면 멈추고 보고한다.
- [ ] **Step 4: `package-dev.sh`(`PEGBOARD_DEV_SPARKLE=1`과 기본 둘), 컨트롤러가 시험 기기에서 `e2e_updater`와 퇴행으로 `e2e_bootstrap`.** 기대: PASS. 변형: `--user-data-dir` 검사 제거 → `no_updater_with_a_user_data_dir` 실패; `feedURLStringForUpdater:`가 기본값의 `SUFeedURL`을 돌려주게 → `user_defaults_feed_url_is_ignored` 실패; 채널 위임자가 언제나 `{"canary"}` → `channel_menu_sets_the_default_and_the_channels` 실패; relaunch 처리에서 값 쓰기 제거 → `relaunch_sets_session_restore` 실패.
- [ ] **Step 5: 보안 검토자가 본다**(피드 주소가 `Info.plist` 하나인지, 배포 빌드에서 시험 훅이 컴파일되지 않는지, 메뉴·채널 선택이 업데이트 검증을 약하게 하지 않는지, 에이전트(CDP, 에이전트 프로필 창)가 업데이터에 닿을 길이 없는지). 반영 뒤 `export.sh`, `budget.sh`(증가 0), 커밋 `feat(browser): Sparkle updates, started from MainParts, with a canary channel`

---

### Task 10: 도움말 화면의 업데이트 줄 (C++)

**Files:**
- Create(2의 길이면): `browser/pegboard/updater/version_updater_disabled.{h,cc}`, `browser/patches/pegboard-about-update.patch`
- Modify: `crates/pegboard-core/tests/e2e_branding.rs`

**Interfaces:**
- Produces: 설계 4.7. (1) 기존 스위치·정책·빌드 인자로 도움말 화면의 업데이트 상태가 "사용 안 함"이 되면 그것(`args*.gn` 한 줄이면 두 파일에 같이, `test_release_args.sh`의 허용 목록은 그대로 하나만 다르게). (2) 아니면 맥의 `VersionUpdater::Create`가 `pegboard::DisabledVersionUpdater`(`CheckForUpdate`가 곧 `DISABLED`를 알림)를 돌려주는 패치, 10줄 이하. 넘으면 멈추고 보고.

- [ ] **Step 1: 시험을 쓴다** (`e2e_branding.rs`, 3b의 도움말 화면 시험 옆):

```rust
#[tokio::test] #[ignore] async fn help_page_has_no_update_error_in_korean()   // --lang=ko: chrome://settings/help 본문에 "업데이트 확인 중 오류" 없음, "Pegboard 정보" 있음
#[tokio::test] #[ignore] async fn help_page_has_no_update_error_in_english()  // --lang=en-US: "error" 들어간 업데이트 줄 없음
```

- [ ] **Step 2: 시험 기기에서 지금 앱으로 돌려 둘 다 FAIL인지 본다(3b 확인의 그 줄).**
- [ ] **Step 3: 트리에서 (1)을 찾고, 없으면 (2)를 구현한다. 두 산출 디렉터리 증분 빌드.**
- [ ] **Step 4: `package-dev.sh`, 시험 기기에서 `e2e_branding` 전체 PASS.** 변형: 패치(또는 인자)를 되돌리면 두 시험이 실패한다.
- [ ] **Step 5: `export.sh`, `budget.sh`(패치면 줄 수), 커밋** `feat(browser): no Chromium update status on the help page (Sparkle updates the app)`

---

### Task 11: 확인 창 모양과 배포 빌드의 데몬 환경 (C++)

**Files:**
- Modify: `browser/pegboard/ui/confirm_dialog.cc`, `browser/pegboard/ui/daemon_notice.cc`(같은 문제일 때), `browser/pegboard/bootstrap/daemon_launcher.cc`, `crates/pegboard-core/tests/{e2e_confirm.rs,e2e_bootstrap.rs}`

**Interfaces:**
- Produces:
  - 확인 창: 기본 버튼(거부)에 강조 모양을 직접 건다(156의 `views::MdTextButton`/`ui::ButtonStyle` 이름은 트리에서). 잠긴 동안 세 버튼에 흐린 모양을 직접 건다(`SetEnabled(false)`만으로는 키 창이 아닌 창에서 그려지지 않았다, 3b 확인). 풀리면 되돌린다. 시험 훅 기록에 `"defaultButtonStyle"`(강조 이름), `"buttonsLookLocked"`(띄운 직후), `"buttonsLookUnlocked"`(풀린 뒤)를 더한다(그린 모양을 정하는 값 그대로: 스타일, 글자색 알파 등).
  - `DaemonEnvironment`: `!BUILDFLAG(PEGBOARD_TEST_HOOKS)`이면 `PEGBOARD_TEST_HOOKS`와 모든 `PEGBOARD_TEST_*`를 언제나 지운다. 훅 빌드는 3a·3b 그대로.

- [ ] **Step 1: 시험을 쓴다.**

```rust
// e2e_confirm.rs (포크, 시험 훅 빌드)
#[tokio::test] #[ignore] async fn deny_is_drawn_as_the_default_button()  // 기록 defaultButtonStyle == 강조, 거부 버튼의 것
#[tokio::test] #[ignore] async fn locked_buttons_look_locked()           // ANSWER=none: buttonsLookLocked가 흐림, 1.7초 뒤 기록의 buttonsLookUnlocked가 보통
// e2e_bootstrap.rs: 배포 인자로 빌드한 앱이 필요하다 → 작업 12의 e2e_release에 둔다:
//   a_release_launcher_passes_no_test_env  (아래 작업 12)
```

- [ ] **Step 2: 구현하고 두 산출 디렉터리 증분 빌드. `package-dev.sh`, 시험 기기에서 `e2e_confirm` 전체.** 기대: PASS. 변형: 강조 지정 제거 → `deny_is_drawn_as_the_default_button` 실패; 흐린 모양 제거 → `locked_buttons_look_locked` 실패.
- [ ] **Step 3: 사람이 본다(시험 기기, agent-desktop 화면 사진):** 3b `manual-check.md`의 2번 순서로 확인 창을 띄워 "거부"가 구별되고, 뜬 직후 버튼이 흐리고 약 2초 뒤 보통이 되는지. 사진을 보고서에 붙인다.
- [ ] **Step 4: `export.sh`, `budget.sh`(증가 0), 커밋** `fix(browser): the confirmation dialog draws its default and locked buttons; release builds pass no test env to the daemon`

---

### Task 12: 서명한 배포 빌드의 측정과 퇴행 (시험 기기)

**Files:**
- Create: `crates/pegboard-core/tests/e2e_release.rs`
- Modify: `docs/spike-findings.md`("3c 작업 12")

**Interfaces:**
- Consumes: 작업 3~11이 들어간 배포 빌드를 작업 6, 7의 단계로 서명·공증한 DMG(`N=1`, 아직 게시하지 않음)
- Produces: `e2e_release.rs`(`#[ignore]`, `PEGBOARD_TEST_RELEASE_APP=<설치한 앱>`일 때만, 시험 훅 없는 앱이므로 시험 훅에 기대지 않는다. 데몬에는 API로 말한다)

```rust
#[tokio::test] #[ignore] async fn release_app_is_signed_notarized_and_versioned() // spctl 출처 Notarized Developer ID, CFBundleVersion·PegboardVersion, browser.status.version == PegboardVersion
#[tokio::test] #[ignore] async fn release_app_has_no_dialog_hooks()   // PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_DIALOG_ANSWER=*=approve PEGBOARD_TEST_DIALOG_LOG=<f>로 open → cookies set을 묻게 하고 API로 답하지 않음 → 60초 뒤 timed_out, 기록 파일 없음
#[tokio::test] #[ignore] async fn a_release_launcher_passes_no_test_env() // 같은 환경으로 띄운 데몬의 환경(`ps eww <pid>`)에 PEGBOARD_TEST_ 없음, PEGBOARD_TEST_IDLE_SECS=1을 줘도 세션이 2초 뒤에도 있음
#[tokio::test] #[ignore] async fn release_app_opens_a_page_and_keeps_the_guard() // session create → open 로컬 시험 서버 페이지 성공, 사설 주소는 여전히 거부
#[tokio::test] #[ignore] async fn notices_and_credits_are_in_the_app()  // Contents/Resources/Notices의 네 항목, chrome://credits에 "Chromium" 항목이 있음(사람 탭을 API로 열 수 없으면 이 줄은 손 확인)
```

- [ ] **Step 1: 시험을 쓰고 컴파일 확인.**
- [ ] **Step 2: 배포 빌드를 증분 빌드하고(작업 9~11 반영) `PEGBOARD_RELEASE_TO=dmg release.sh canary 1`(떼어 돌림). 시험 기기로 DMG를 옮겨 격리 속성을 붙이고 `/Applications`에 설치.**
- [ ] **Step 3: `e2e_release` 전체.** 기대: PASS. 변형: 작업 11의 `DaemonEnvironment` 변경을 되돌린 배포 빌드에서 `a_release_launcher_passes_no_test_env`가 실패한다(컨트롤러가 한 번 빌드해 확인, 그 빌드는 게시하지 않는다).
- [ ] **Step 4: 키체인을 잰다(R15, 사용자와 함께).** 시험 기기의 GUI 세션에서, 시험이 아닌 보통 실행(Dock, `--use-mock-keychain` 없음)으로:
  1. 개발 빌드가 만든 `Pegboard Safe Storage` 항목이 있는 채 첫 실행 → 창이 뜨는지, 뜨면 사용자가 "항상 허용"(키체인 암호는 사용자가 친다. 에이전트는 누르지 않는다). agent-desktop 화면 사진.
  2. 다시 켜기 → 창이 없어야 한다.
  3. 사용자가 그 항목을 키체인 접근에서 지운 뒤 첫 실행 → 창이 뜨는지(새 항목 만들기).
  4. 같은 `Info.plist`에 `N=2`로만 다시 묶고 서명한 앱으로 바꿔 끼워 켜기(업데이트 흉내) → 창이 없어야 한다. 뜨면 **멈추고** `codesign -d -r- `의 지정 요구 사항을 두 판에서 비교해 보고한다.
- [ ] **Step 4b: 코드 서명 사본이 남지 않는지 본다.** Chromium의 `MacAppCodeSignClone`(기본 켜짐)은 켤 때마다 앱 번들을 `$TMPDIR/../X/app.pegboard.browser.code_sign_clone/`에 복제하고 보통 종료 때만 지운다(시험 실행은 끄지 않고 죽이므로 시험은 `--disable-features=MacAppCodeSignClone`로 끈다, `browser::fork_test_args`). 배포 앱은 그대로 켠다: 보통 실행 → 그 디렉터리에 사본 하나 → 메뉴의 "종료"(⌘Q)로 끔 → 사본이 없어졌는지(`ls`·`du -sh`). 남으면 멈추고 보고한다.
- [ ] **Step 5: `AperitifHelpers`를 잰다(R14):** 같은 앱을 `--user-data-dir=<임시> --enable-features=AperitifHelpers`로 켜고 로컬 시험 서버로 HTTP 요청 하나, `Pegboard Helper (Aperitif).app`이 떴는지와 `aperitif: Failed to initialize sandbox`가 로그에 있는지. 결과를 적는다(`disable_fieldtrial_testing_config`는 결과와 상관없이 둔다, 설계 R14).
- [ ] **Step 6: 퇴행:** 같은 커밋의 개발 앱(`package-dev.sh`, 시험 훅 빌드)으로 3b 작업 11의 두 명령 그대로(포크 `pegboard-core` 전체, 순정 워크스페이스 전체 `--skip diag_`). 기대: 3b 끝과 같은 수 + 3c에서 더한 시험, 실패 0. 녹화 지연 중앙값 60ms 이하.
- [ ] **Step 7: 측정을 spike-findings "3c 작업 12"에 적고 커밋** `docs: phase 3c release build measurements (keychain, Aperitif, hooks)`

---

### Task 13: 리허설 피드 — 거부해야 할 업데이트 (시험 기기)

**Files:**
- Modify: `docs/spike-findings.md`("3c 작업 13"), 필요하면 `browser/scripts/release.sh`(`--feed rehearsal` 다듬기)

**Interfaces:**
- Consumes: 작업 6의 `--feed rehearsal`, 작업 8의 도구(리허설 피드는 `gh-pages`의 `rehearsal/appcast.xml`, `appcast.py --feed-url <리허설 주소>`)
- Produces: 리허설 기록. 리허설 자산은 이름이 `rehearsal-`로 시작하는 프리릴리스에만 두고 끝나면 `gh release delete --cleanup-tag`로 지우며 `rehearsal/` 디렉터리도 지운다.

- [ ] **Step 1: 리허설 앱 `R1`(`--feed rehearsal`, `N=1`)을 묶어 서명·공증하고 시험 기기에 설치한다(운영 `N=1`이 설치되어 있으면 지우고).** 채널 카나리.
- [ ] **Step 2: 나쁜 항목을 하나씩 리허설 피드에 올리고**(각각 `appcast.py`로 만들되 나쁜 부분은 시험이 직접 만든다) "업데이트 확인…"(agent-desktop)으로 확인한다. 기대는 모두 **설치되지 않음**과 Sparkle 기록(`log show --predicate 'subsystem BEGINSWITH "org.sparkle-project"' --last 10m`)의 거부 이유:
  1. `N=2` DMG, EdDSA를 **다른 키**(`generate_keys --account pegboard-rehearsal`로 만든 시험용 키. 키체인 창은 사용자가 답한다)로 서명.
  2. `N=3` DMG, 맞는 EdDSA, 안의 앱은 **임시 서명**(`codesign --force --deep -s -`로 다시 서명. `--deep`은 이 나쁜 시료를 만들 때만).
  3. 더 낮은 버전(`CFBundleVersion` `8078.109.0`) 항목, 맞는 서명 → "최신 판" 표시.
  4. 맞는 `N=4` 항목이지만 **appcast 서명이 틀림**(서명 뒤 한 글자 고침) → 업데이트를 보이지 않음.
  5. 대조: 맞는 `N=5`(모두 맞음) → 설치되고 다시 켜진다(리허설 피드에서 왕복이 된다는 것).
- [ ] **Step 3: 화면 사진과 기록의 거부 이유를 spike-findings에 적고, 리허설 자산과 `rehearsal/`을 지운다. 시험 기기에서 리허설 앱을 지운다.**
- [ ] **Step 4: 보안 검토자가 결과를 본다**(넷 다 막혔는지, 5가 통과한 것이 검증이 꺼져서가 아닌지: 같은 앱이 1~4를 거부한 뒤의 일). 커밋 `docs: phase 3c rehearsal — the updater refuses bad updates`

---

### Task 14: 첫 canary, 업데이트 왕복, 문서

**Files:**
- Modify: `docs/spike-findings.md`("3c 작업 14"), 설계 문서(만든 그대로, "처음 제안은 …", 9절 결과와 10절 열린 항목을 더함), `docs/architecture.md`(5절 파이프라인: CI가 아니라 개발 기기 스크립트, 패치 표의 "업데이트" 줄, 라이선스 고지 [확인], 11절 남는 위험), `docs/roadmap.md`(할 일 8, 9, 진행 현황), `README.md`(설치, CLI를 `PATH`에, MCP 설정의 절대 경로, 채널, 업데이트, 첫 실행의 키체인), `browser/README.md`(배포 빌드, `release.sh` 단계와 다시 하기, 사용자가 한 번 할 일은 `browser/release/README.md`로)

- [ ] **Step 1: canary `N=1`을 게시한다**(작업 12의 작업 디렉터리에서 `PEGBOARD_RELEASE_FROM=publish`, 컨트롤러). 깃허브 릴리스(프리릴리스)와 Pages의 `appcast.xml`을 받아 `appcast.py check` 통과.
- [ ] **Step 2: 시험 기기에서 깃허브 릴리스 페이지의 DMG를 브라우저로 받아(격리 속성이 붙는 길) 설치하고 켠다.** 앱 메뉴 → 업데이트 채널 → 카나리(agent-desktop). 사람 프로필에 북마크 하나와 시험 사이트의 쿠키 하나를 만든다. 탭 두 개를 연다. `pegboard`를 문서의 심볼릭 링크로 걸고 `pegboard --version`, `pegboard browser status`(데몬 `version` = `156.0.8078.109-pb.1`).
- [ ] **Step 3: canary `N=2`를 게시한다**(개발 기기, Chromium 같음: `PEGBOARD_RELEASE_FROM=package release.sh canary 2`, 떼어 돌림. 걸린 시간 기록).
- [ ] **Step 4: 왕복 — 사람이 고르는 길:** 시험 기기에서 "업데이트 확인…" → `N=2` 발견 → 설치 후 다시 시작. 확인: `CFBundleVersion` `8078.109.2`, 20초 안에 `pegboard browser status`가 `156.0.8078.109-pb.2`를 보이고 데몬 로그에 종료 코드 4가 없음, `pegboard session create` → `open <시험 페이지>` 성공, 키체인 창 없음(화면 사진), 북마크·쿠키 그대로, 탭 둘 복원.
- [ ] **Step 5: 왕복 — 자동 길:** canary `N=3`을 게시하고, 시험 기기의 앱이 자동 확인으로 받게(시간을 줄이려면 앱을 다시 켜 Sparkle의 시작 확인을 부르거나, 사용자 기본값 `SULastCheckTime`을 하루 전으로 — 그 값은 Sparkle 상태일 뿐 검증을 바꾸지 않는다) 내려받은 뒤 Cmd-Q로 끄고 다시 켜면 `N=3`. 같은 확인.
- [ ] **Step 6: stable 올리기:** `release.sh stable 3` → appcast의 `N=3`에 채널 표시 없음, 릴리스가 "Latest". 시험 기기의 채널을 안정으로 바꿔도 `N=3`에 머문다(내려가지 않음).
- [ ] **Step 7: 기록한다:** 배포 빌드 빈 상태 시간, `package`·공증·DMG·게시 시간, DMG 크기, 앱 크기, 패치별 업스트림 줄 수(3c 증가), 새 `//pegboard` 줄 수. 저장소에 비밀·이름이 없는지 마지막으로 본다: `git grep -nE "Developer ID Application:|notarytool store-credentials .*--password|BEGIN PRIVATE|/Users/"`가 문서의 일반 설명 말고는 없음.
- [ ] **Step 8: 문서를 고치고 커밋** `docs: phase 3c done (release pipeline, Sparkle updates), measurements`

---

## 작업 순서와 의존

3(배포 빌드 시작)을 가장 먼저 떼어 돌리고, 그동안 1 → 2(Rust), 4 → 5(사용자 단계) → 6 → 7 → 8(스크립트)을 이 기기와 개발 기기에서 진행한다. 6의 실제 서명은 배포 빌드가 끝나기 전에는 개발 산출물의 사본으로 경로만 본다.
9, 10, 11(C++)은 3이 끝난 뒤(같은 트리의 빌드가 겹치지 않게) 차례로, 각자 두 산출 디렉터리를 증분 빌드한다.
12는 2, 6, 7, 9, 10, 11 뒤. 13은 8, 9, 12 뒤. 14는 마지막.
사용자가 해야 하는 것(작업 5의 네 단계, 작업 12의 키체인 답)이 늦어지면 그 뒤 작업(6의 실제 서명부터)이 기다린다.

## 설계와의 차이, 이 계획이 정한 것

- `release.sh`에 `PEGBOARD_RELEASE_TO=<단계>`(그 단계까지만)를 더했다. 시험 게시 없이 서명·공증까지 재기 위해서다.
- appcast 항목의 빌드 번호를 `pegboard:n` 요소(자기 네임스페이스)로 둔다. `sparkle:version`에서 거꾸로 뽑지 않게.
- `package-dev.sh`의 `PEGBOARD_DEV_SPARKLE=1`(시험용 개발 앱에 Sparkle을 넣음)과 업데이터 시험 훅 `PEGBOARD_TEST_UPDATER`, `PEGBOARD_TEST_UPDATER_LOG`, `PEGBOARD_TEST_UPDATER_RELAUNCH`(시험 훅 빌드에만)를 정했다.
- 배포 빌드 판정(`is_release_build`)은 `PEGBOARD_RELEASE_VERSION`을 주고 빌드했는지 하나다. 버전 값과 시험 훅 끄기를 한 값에 묶어, 버전이 박힌 빌드에 훅이 남는 조합을 만들 수 없게 했다.
- 홈 옮기기의 임시 이름(`Pegboard Agent.moving`)과 중간에 멈춘 뒤 이어 가기를 정했다.
