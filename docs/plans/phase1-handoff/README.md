# 1단계 인계 문서

- 작성일: 2026-11-11
- 브랜치: `phase1-local-mvp` (main에서 갈라짐, 아직 병합하지 않음)
- 계획: [../2026-11-11-phase1-local-mvp.md](../2026-11-11-phase1-local-mvp.md)
- 이 폴더: 다른 기기에서 작업을 이어가는 데 필요한 것. `ledger.md`는 진행 원장(판정, 미룬 항목 전부), `task-N-report.md`는 작업별 구현 보고서다.

## 왜 멈췄나

실제 브라우저를 띄우는 시험이 많아, 작업 중인 화면에 창이 계속 떴다. 다른 기기에서 이어서 한다.

## 현재 상태

| 작업 | 상태 | 커밋 범위 |
|---|---|---|
| 1. 작업공간, 프로토콜 타입 | 완료, 검토 통과 | `adef854..cb5203c` |
| 2. CDP 연결, 브라우저 실행 | 완료, 수정 1회 | `cb5203c..698936f` |
| 3. 브리지 확장 프로그램 | 완료, 수정 1회 | `698936f..6202bbe` |
| 4. 프록시 통과 판정 | 완료, 수정 2회 | `6202bbe..4cf7650` |
| 5. 세션 CDP 엔드포인트 | 완료, 수정 1회 | `4cf7650..b2af113` |
| 6. 프로필과 세션 관리자 | 완료, 수정 1회 | `b2af113..35ccfcd` |
| 7. agent-browser 실행기와 명령 필터 | 완료, 수정 1회 | `35ccfcd..606ea35` |
| **8. 동작 로그와 가림** | **구현됨, 검토 실패, 수정 전** | `606ea35..e154a84` |
| 9. 코어 API, 데몬, CLI | 시작 전 | |
| 10. 격리 시험, 수동 확인, README | 시작 전 | |
| 최종 전체 검토 | 시작 전 | |

**알려진 결함 (지금 코드에 있음)**: 작업 8의 동작 로그는 비밀번호를 평문으로 남길 수 있다. 입력이 실패했을 때, 입력과 동시에 폼이 제출될 때, 한 글자씩 누를 때, `eval` 식 안에 값이 있을 때, 쿠키와 저장소 값을 쓸 때다. 아래 "작업 8 수정"이 이것을 고친다. 아직 CLI가 없어(작업 9) 사용자가 실행할 경로는 없다.

## 다른 기기 준비

요구 사항: macOS arm64, Rust stable(`cargo`), git, Claude Code와 superpowers 플러그인. Chromium 소스나 Xcode는 필요 없다.

```bash
git clone https://github.com/<소유자>/pegboard-browser
cd pegboard-browser
git checkout phase1-local-mvp

# agent-browser 0.42.0 바이너리를 받아 체크섬을 대조한다
scripts/fetch-agent-browser.sh

# Chrome for Testing을 받는다 (agent-browser가 ~/.agent-browser/browsers 아래에 설치한다)
vendor/agent-browser/agent-browser install

# 확인: 브라우저 없이 도는 시험
cargo test --workspace

# 확인: 실제 브라우저를 띄우는 시험 (창이 뜬다)
cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1
```

브라우저 경로를 직접 지정하려면 `PEGBOARD_CHROME=<실행 파일 경로>`.

기대 결과(이 기기에서 마지막으로 확인한 값): 단위 시험 85개 통과. 실제 브라우저 시험은 `e2e_browser` 6, `e2e_bridge` 5(2분 유휴 시험 포함), `e2e_proxy` 18, `e2e_session` 11, `e2e_runner` 17, `e2e_actionlog` 8.

그 기기에서 이미 agent-browser를 쓰고 있다면: 시험은 전용 소켓 디렉터리(`/tmp/pegboard-<uid>/ab`)와 `peg-`로 시작하는 세션 이름만 쓰므로 기존 데몬을 건드리지 않는다.

## 이어서 하는 방법

Claude Code를 저장소에서 열고 이렇게 요청한다.

> `docs/plans/phase1-handoff/README.md`를 읽고 1단계를 이어서 진행해 줘. 작업마다 별도 에이전트 방식(subagent-driven-development)으로, 작업 8의 수정부터.

실행 방식은 그대로다: 작업마다 구현 에이전트 하나, 독립 검토자 하나, 문제가 나오면 수정 후 범위를 좁힌 재검토. 커밋은 이 브랜치에 작업마다 한다. 푸시와 main 병합은 사용자가 요청할 때만 한다.

이어받는 쪽이 할 일:

1. 계획서, 이 문서, `ledger.md`를 읽는다. 원장의 `Ruling:` 줄은 계획서보다 우선한다(계획의 결함을 고친 판정들이다).
2. 플러그인의 작업 공간을 새로 만들고(`sdd-workspace`), `ledger.md`의 내용을 새 원장의 출발점으로 복사한다.
3. 작업 8의 수정을 새 구현 에이전트에게 맡긴다. 아래 "작업 8 수정"을 그대로 전달하고, `task-8-report.md`와 계획서의 작업 8을 읽게 한다.
4. 재검토가 통과하면 작업 9, 10, 최종 검토로 간다. 아래 "작업 9에 넘길 것", "작업 10에 넘길 것"을 각 작업의 지시에 넣는다.

## 계획서와 달라진 것 (요약)

자세한 근거는 `ledger.md`의 `Ruling:` 줄에 있다. 뒤 작업이 알아야 할 것만 추렸다.

- **프록시 규칙 (작업 4, 5)**
  - 브라우저 수준 `Target.setAutoAttach`는 `waitForDebuggerOnStart`를 false로 바꿔 넘기고, 프록시가 숨긴 자동 연결 세션은 재개한 뒤 끊는다. 사람 탭이 멈추지 않게 하려는 것이다.
  - `Target.activateTarget`과 `Page.bringToFront`는 허용하지 않는다(창 포커스를 빼앗는다).
  - `Page.navigate`와 탭 생성은 `http`, `https`, `data`, `about:blank`만 받고, 정규화한 주소만 넘긴다. `file:`, `chrome:` 등은 거부한다.
  - 브리지도 같은 주소 검사를 한다.
- **세션 (작업 6)**
  - `ProxyServer::start(upstream)`은 브리지를 받지 않는다. `register(id, context, group, bridge, initial_targets)` 하나뿐이다.
  - 닫힌 세션은 표에서 지운다. 닫은 뒤 `info()`는 `session_not_found`다.
  - 프로필은 `SessionManager::list_profiles / create_profile / delete_profile`로 다룬다.
  - 세션의 첫 탭도 `browserContextId`를 확인한 뒤에 세션에 넣는다.
- **명령 필터 (작업 7)**
  - 금지 목록을 버리고 **허용 목록 문법**으로 바꿨다. `argv::check(argv, files_dir)`. 명령은 반드시 첫 낱말이어야 하고, 선언된 하위 명령과 플래그와 인자만 통과한다.
  - 파일 인자는 이름만 받고 `<PEGBOARD_HOME>/sessions/<세션 ID>/files/` 아래로 바뀐다. 로컬 파일을 읽는 형태는 없다.
  - 1단계에서 빠진 명령: `read`, `flow`, `inspect`, `trace`, `profiler`, `webmcp`, `diff`, `network route/unroute/har`, `wait --download`, `upload`, `download`, `clipboard`, `react`, `pushstate`, `batch`, 그리고 pegboard가 맡는 명령 전부. `cookies clear`, `set media/geo/offline`, `tab <번호>`도 안 된다.
  - agent-browser는 환경을 비운 채, 전용 HOME과 작업 디렉터리에서 실행한다. 토큰은 환경 변수로 넘긴다.
  - `tab new <주소>`는 agent-browser를 **거쳐** 실행한다(프록시가 브리지로 탭을 만든다). `SessionManager::new_tab`은 탭이 하나도 없을 때의 복구용이다.
- **동작 로그 (작업 8)**: `run_command`는 `&Paths`를 추가로 받는다. 가림 규칙은 아래 수정에서 바뀐다.

## 작업 8 수정 (다음에 할 일)

대상: `crates/pegboard-core/src/daemon.rs`, `actionlog.rs`, `argv.rs`(문법 표시만), 시험과 픽스처. 프록시와 세션 코드는 건드리지 않는다. 시험을 먼저 쓰고 실패를 확인한 뒤 구현한다.

**검토에서 나온 문제**

- C1 (치명): agent-browser가 실패를 보고한 경우(낡은 `@ref`, 틀린 선택자. Runner는 `Ok`를 돌려주고 결과의 `success`가 false)를 실패로 보지 않는다. 입력이 칸에 닿지 못해 포커스가 `body`에 있으니 "민감하지 않음"으로 판정되고, `["fill","@e7","hunter2"]`가 평문으로 남는다.
- I2: `cookies set`, `storage (local|session) set`의 값이 평문으로 남는다(거부 경로 포함).
- I3: 검사가 명령 뒤에만 돈다. `type "#pw" "hunter2\n"`처럼 제출되어 페이지가 넘어가면 평문으로 남는다. 자동으로 다음 칸으로 넘어가는 일회용 코드 칸, 포커스를 빼는 스크립트도 같다.
- I4: `press h`, `press u`처럼 한 글자씩 누르면 글자마다 남는다.
- I5: `eval`의 처음 200자가 평문이다. 예: `document.querySelector('#pw').value='hunter2'`.
- M6: `error`에 CLI 출력에서 온 글이 들어갈 수 있다. 입력값이나 페이지 값을 인용할 수 있다.
- M10: 오타 난 명령(`fil #pw hunter2`)은 평문으로 남는다.
- M14: `autocomplete`의 `cc-exp-month`, `cc-exp-year`가 빠져 있다.

**판정 (그대로 구현)**

- R1. 입력한 글자는 다음이 **모두** 참일 때만 평문으로 남긴다. 아니면 글자 위치를 전부 `REDACTED`로 바꾸고 `redacted: true`.
  - (a) 명령이 성공했다(Runner가 `Ok`이고 CLI 결과도 성공).
  - (b) 명령 **전** 검사에서 대상이 민감하지 않았다.
    - 선택자를 받는 형태(`fill`, `type`, 글자로 분류된다면 `select`, `find … fill/type`): 명령 전에 pegboard가 `get attr <선택자> type`과 `get attr <선택자> autocomplete`를 Runner로 실행한다(내부 호출, 로그에 남기지 않음). 둘 다 성공해야 한다. `type`이 `password`이거나 `autocomplete` 토큰에 `current-password`, `new-password`, `one-time-code`, `cc-number`, `cc-csc`, `cc-exp`, `cc-exp-month`, `cc-exp-year` 중 하나가 있으면 민감. `find …` 형태를 같은 위치 지정으로 `get attr`에 옮길 수 없으면 사전 검사 실패로 본다.
    - 선택자가 없는 형태(`keyboard type`, `keyboard inserttext`, R2의 `press`): 명령 전에 포커스된 요소를 검사한다.
  - (c) 명령 **후** 포커스된 요소 검사에서도 민감하지 않았다.
  - 검사가 오류를 내거나, 시간을 넘기거나(검사 2초, `get attr`는 최대 5초), 실행할 수 없으면 가린다.
- R2. `press <키>`는 키가 인쇄 가능한 한 글자이거나 `Shift+<한 글자>`일 때 입력 글자로 본다. 이름 있는 키(`Enter`, `Tab`, `Control+a` 등)는 아니다.
- R3. `cookies set`과 `storage (local|session) set`의 값 인자는 항상 가린다. 이름과 키는 남긴다. 문법에 "이 인자는 항상 비밀"이라는 표시를 둔다.
- R4. `eval`: 문자열 리터럴(작은따옴표, 큰따옴표, 백틱, 백슬래시 이스케이프 처리)의 내용을 `…`로 바꾼 뒤 200자로 자른다. 닫히지 않은 리터럴이 있으면 `[eval <n> chars]`만 남긴다.
- R5. 가려진 항목의 `error`는 `<코드>: details withheld (entry redacted)`로 바꾼다. 가려지지 않은 항목의 `error`는 CLI 결과의 `error` 문자열 필드나 pegboard 자신의 오류에서만 온다. 표준 출력이나 결과 전체로 대신하지 않는다.
- R6. 거부된 명령줄: 첫 낱말이 문법에 없는 명령이면 뒤의 낱말을 전부 가린다.
- R7. 주석을 실제 규칙에 맞게 고친다.

**시험 이름**

단위:
- `eval_string_literals_are_masked`
- `cookie_and_storage_values_are_always_redacted`
- `press_single_characters_count_as_typed_text`
- `unknown_command_words_are_redacted`
- `autocomplete_tokens_include_card_expiry_parts`
- `redacted_entries_withhold_error_text`

실제 브라우저(`tests/e2e_actionlog.rs`. 픽스처 추가: `/form-submit`은 `#pw`가 있고 Enter로 `/counter`에 제출, `/form-otp`는 `autocomplete="one-time-code"` 칸이 입력 시 스스로 포커스를 뺌):
- `failed_fill_into_password_field_is_redacted`
- `type_with_newline_that_submits_is_redacted`
- `keyboard_type_into_focused_password_that_submits_is_redacted`
- `otp_field_that_blurs_is_redacted`
- `press_key_by_key_into_password_is_redacted`
- `non_sensitive_text_is_still_logged`
- `cookie_value_never_reaches_the_log`
- 기존 8개도 통과

돌릴 것: `cargo test -p pegboard-core --lib`, `cargo test -p pegboard-core --test e2e_actionlog -- --ignored --test-threads=1`, 그리고 `argv.rs`가 바뀌므로 `e2e_runner`도 같은 방식으로.

## 작업 9에 넘길 것

- `SessionId`는 `Deserialize`를 파생해서 `SessionId::parse`를 거치지 않는다. RPC 경계에서 `parse`를 거치게 한다. ID가 파일과 소켓 이름이 된다. 녹화 ID도 같은 검증을 한다.
- `SessionManager::create`는 도중에 버려지면 정리하지 못한다. 요청 연결이 끊겨도 끝까지 도는 태스크에서 실행한다.
- `daemon::run_command`의 실제 시그니처를 쓴다(`&Paths` 포함). 세션별 `tokio::sync::Mutex`로 한 세션의 명령을 직렬화한다.
- Runner의 세션 타깃 제공자는 데몬을 만들 때 한 번만 설정한다.
- 데몬이 시작할 때 `ab_socket_dir`에 남은 `peg-*` 데몬을 정리한다. pegboard가 죽은 뒤 남은 것들이다.
- 세션을 닫을 때 세션 파일 디렉터리를 어떻게 할지 정한다(지금은 남는다. CLI가 쓴 파일은 0700 디렉터리 안의 0644).
- `SessionManager::evaluate_in_targets`는 프록시 허용목록을 거치지 않는 내부 API다. RPC로 노출하지 않는다.
- 오류 코드 `timeout`이 있다. 시간 초과 뒤 agent-browser 데몬이 약 9초 바쁘다. 시간 초과 시 그 데몬을 죽일지 정한다.
- 코어 API 메서드와 CLI는 계획서의 작업 9 그대로. `profile.*`는 위의 좁은 메서드를 쓴다.

## 작업 10에 넘길 것

- **필수 시험**: 프록시가 `Page.createIsolatedWorld`의 `grantUniveralAccess`를 그대로 넘긴다(agent-browser의 `a11y`가 쓴다). 세션 페이지의 그런 격리 세계가 `file://`을 읽을 수 없고 다른 프로필에 닿지 않는지 확인한다. 읽을 수 있으면 프록시에서 그 값을 false로 강제한다.
- README의 "아직 안 되는 것"에 위의 빠진 명령 목록을 넣는다.
- README의 "알려진 한계"에 넣을 것:
  - 1~2단계는 루프백 디버깅 포트를 쓴다. 이 브라우저에 실제 계정을 넣지 않는다.
  - 에이전트는 `localhost`와 사설 IP의 http(s)에 접근할 수 있다.
  - `eval`로 시스템 클립보드에 쓸 수 있을 가능성이 있다(확인 전).
  - 연결이 폭주하다 끊기는 마지막 약 500ms에 열린 사람 쪽 팝업은 멈춘 채 남을 수 있다.
- 수동 확인 M1~M7(화면 잠금, 다른 데스크톱 공간, 절전, 설정 누출, 다운로드 경로, 탭 끌기, 창 닫기)은 사용자가 한다.

## 최종 검토에서 볼 것

- `ledger.md`의 `minor (deferred)` 줄 전부. 병합 전에 고칠 것을 고른다.
- 설계 대안: 브라우저 수준 자동 연결을 넘기지 않고 프록시가 흉내 내는 방식. 사람 프로필의 타깃이 그 연결에 붙는 일 자체가 없어진다. 지금 방식(넘기고 풀어 주기)은 측정으로 동작을 확인했지만 복잡하다.
- 2단계(MCP) 전에 정할 정책: 클립보드, `localhost` 접근.

## 사용자에게 알린 것, 아직 답이 없는 것

- 에이전트가 열 수 있는 주소를 `http`, `https`, `data`, `about:blank`로 제한했다. 로컬 파일 자동화가 필요하면 옵션으로 열 수 있다.
- 파일 업로드와 다운로드는 1단계에서 안 된다.
