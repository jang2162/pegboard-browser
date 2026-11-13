# pegboard-browser

AI 에이전트가 브라우저를 쓰되, 사람이 쓰는 브라우저 데이터와 섞이지 않게 하는 도구다. 에이전트는 격리된 세션 안에서만 탭을 열고 조작한다. 사람은 같은 브라우저를 별도 프로필로 그대로 쓴다. 세션마다 CDP 프록시가 붙어 그 세션의 탭만 보이게 하고, 세션에서 한 동작은 비밀번호 같은 값을 가린 채 기록으로 남는다. 조작 엔진으로는 [agent-browser](https://github.com/vercel-labs/agent-browser) 0.42.0을 동봉해 그대로 쓴다.

설계는 [docs/architecture.md](docs/architecture.md), 단계별 계획은 [docs/roadmap.md](docs/roadmap.md).

## 상태

**1단계(로컬 MVP).** 순정 Chrome for Testing 위에서 동작한다. 설치해서 쓰는 브라우저가 아니고, Chromium 포크는 3단계 계획이다. macOS(arm64)에서만 동작한다. 자동 시험은 끝났고 손으로 하는 확인 9개(M1~M7, M8, M8b)도 수행했다(2026-11-13, [docs/phase1-manual-checks.md](docs/phase1-manual-checks.md)). 8개는 기대대로였고 다운로드 경로(M5) 하나가 기대와 달랐다(에이전트가 받은 파일이 `~/Downloads`에 저장됨). 그 뒤 에이전트 프로필의 다운로드를 막아 고쳤다. 아래 "알려진 한계"의 다운로드 항목을 본다.

## 준비

- Rust (stable). `cargo build --release`로 `target/release/pegboard`를 만든다.
- agent-browser 0.42.0: `scripts/fetch-agent-browser.sh`가 npm 레지스트리에서 받아 `vendor/agent-browser/`에 두고 sha256을 확인한다. 라이선스는 [THIRD_PARTY.md](THIRD_PARTY.md).
- Chrome for Testing: `~/.agent-browser/browsers/chrome-*` 아래의 가장 새 것을 쓴다. 없으면 `vendor/agent-browser/agent-browser install`로 받거나, 실행 파일 경로를 `PEGBOARD_CHROME`에 준다.

```bash
cargo build --release
scripts/fetch-agent-browser.sh
export PATH="$PWD/target/release:$PATH"
```

데이터는 `~/Library/Application Support/Pegboard`에 쌓인다. 다른 곳을 쓰려면 `PEGBOARD_HOME`을 지정한다(소켓 경로 길이 때문에 짧아야 한다).

## 빠른 시작

```bash
pegboard browser start                                   # 브라우저와 pegboard 데몬 시작
pegboard session create --name "조사" --url https://example.com
```

`session create`는 첫 줄에 세션 핸들 `<id>.<토큰>`을 찍는다. 이 핸들이 그 세션의 열쇠다. 명령줄 인자는 같은 사용자의 `ps`에 보이므로 환경변수로 넘긴다.

```bash
export PEGBOARD_SESSION='s_xxxxxxxx.yyyyyyyyyyyyyyyyyyyyyyyyyy'   # 위에서 찍힌 핸들

pegboard snapshot -i          # 접근성 트리와 요소 ref
pegboard click @e1            # ref로 클릭
pegboard get url
pegboard eval "document.title"

pegboard session list         # 세션 목록 (토큰은 나오지 않는다)
pegboard recording list       # 동작 기록 목록
pegboard recording show <기록 id>

pegboard session close "$PEGBOARD_SESSION"   # 탭을 닫고 기록을 마감한다
pegboard browser stop
```

`--session <핸들>` 플래그로도 줄 수 있지만 `ps`에 노출되므로 권하지 않는다. 핸들이 없으면 종료 코드 2, 틀린 핸들이면 `session_not_found`와 종료 코드 1이다. 종료 코드: 0 성공, 1 명령 실패, 2 사용법 오류나 거부된 명령, 3 pegboard가 떠 있지 않음.

## 세션과 프로필

- 세션은 에이전트 프로필의 창에 만든 탭그룹 하나다. 세션의 명령은 그 탭그룹의 탭에만 닿는다. 다른 세션의 탭과 사람 프로필의 탭은 목록에도 나타나지 않고 타깃 ID로 지정해도 닿지 않는다.
- 사람의 프로필(`Default`)과 에이전트 프로필은 쿠키, 저장소, 설정이 나뉜다. 에이전트가 만든 탭은 항상 에이전트 프로필에 열린다.
- 같은 프로필의 세션끼리는 쿠키와 저장소를 공유한다(의도한 동작). 세션 사이는 탭 수준 격리다. 데이터까지 나누려면 프로필을 나눈다.

```bash
pegboard profile create work
pegboard session create --name 업무 --profile work     # 기본 프로필과 로그인 상태가 섞이지 않는다
pegboard profile list
pegboard profile delete work                            # 열린 세션이 있으면 거부된다
```

- 세션에서 쓸 수 있는 agent-browser 명령은 허용목록(`crates/pegboard-core/src/argv.rs`)에 있는 것뿐이다. 목록 밖의 명령, 연결을 바꾸는 플래그(`--cdp`, `--profile`, `--auto-connect` 등), 다른 세션을 가리키는 인자는 종료 코드 2로 거부된다. 거부된 시도도 기록에 남는다(인자는 가려진다).
- 열 수 있는 주소는 http(s), `data:`, `about:blank`뿐이다. `file:`, `chrome:` 같은 다른 스킴은 거부된다.

## 아직 안 되는 것

- MCP 서버, 화면 녹화(지금 기록은 동작 목록뿐이다), 동작 정책과 확인 요청, 원격 제어, 프로필 복사.
- 1단계 허용목록에서 뺀 명령. 허용목록(`crates/pegboard-core/src/argv.rs`)에 없는 것은 모두 거부되며, 아래는 그중 알려 둘 만한 것이다.
  - `cookies clear`, `set media`, `set geo`, `set offline`: 필요한 CDP 메서드(`Network.clearBrowserCookies`, 에뮬레이션·네트워크 조건 계열)를 프록시가 허용하지 않는다.
  - `read`, `flow`, `diff`, `upload`, `download`, `network route`, `network har`, `trace`, `profiler`, `inspect`, `webmcp`, `clipboard`, `react`, `pushstate`, `wait --download`: 세션 밖(로컬 파일, CLI 자체의 네트워크 접근, 인증 없는 DevTools 프록시)에 닿는다.
  - `batch`: 명령 여러 개를 묶어 한 번에 보내므로 명령별 필터를 지나치게 된다.
  - `set credentials`, `set headers`: 인자에 비밀이 들어가거나 요청 전체에 영향을 준다. 허용목록에 없다.

## 알려진 한계

- 1~2단계는 루프백 디버깅 포트를 쓴다. 같은 사용자로 도는 로컬 프로세스는 브라우저 전체에 붙을 수 있다. 격리가 막는 것은 제공된 도구를 통해 조종당한 에이전트이고, 셸을 가진 악의적 프로세스가 아니다. 이 브라우저의 데이터 디렉터리에는 실제 계정을 넣지 않는다.
- 에이전트는 localhost와 사설망의 http(s) 주소에 닿을 수 있다.
- 에이전트 프로필(기본과 이름 붙인 프로필)에서 시작된 다운로드는 1단계에서 막는다. 파일이 어디에도 저장되지 않고, 링크를 누르는 명령 자체는 멈추지 않고 끝난다. 세션별 다운로드 폴더는 2단계에서 정한다. 사람 프로필의 다운로드는 그대로다(수동 확인 M5에서 에이전트의 다운로드가 `~/Downloads`에 저장되는 것을 발견해 고쳤다. 자동 시험: `agent_download_writes_no_file`).
- 다운로드 차단은 데몬이 프로필마다 쥐고 있는 브라우저 연결에 걸려 있다. 그 연결이 끊기면 Chrome이 차단을 풀며, 데몬은 끊긴 것을 감지해 다시 걸지 않는다(브라우저가 살아 있는 채로 연결만 끊기는 경우는 관찰되지 않았다). 프로필의 창을 모두 닫았다가 다시 여는 경우의 재적용은 코드에 있지만 시험하지 않았다.
- 세션의 탭이 하나도 남지 않으면 `open`과 `tab` 명령만 실행되고, 나머지 명령은 `tab_gone` 오류로 끝난다. `tab new <url>`이나 `open <url>`로 탭을 다시 연다.
- `pegboard tab new 'data:...'`은 성공으로 끝나지만 탭은 `about:blank`로 열린다(수동 확인 중 관찰). data 주소는 `pegboard open`으로 연다.
- `eval`로 시스템 클립보드에 쓸 수 있을 것으로 보인다(확인하지 않았다).
- 연결이 폭주하다 끊기는 마지막 약 0.5초 사이에 사람 쪽에서 연 팝업은 멈춘 채로 남을 수 있다.
- `--session`으로 넘긴 핸들은 프로세스 목록에 보인다. `PEGBOARD_SESSION`을 쓴다.
- `session list`, 기록 조회(가려진 로그), `profile delete`, `browser stop`은 토큰이 필요 없다. 로컬 클라이언트라면 누구나 다른 세션의 (가려진) 기록을 읽거나 브라우저를 끌 수 있다.
- 핸들을 잃어버린 세션은 `pegboard browser stop`으로만 닫을 수 있다.
- 세션 닫기가 실패하면 세션 없이 탭만 에이전트 창에 남을 수 있다.
- 동작 기록은 공격적으로 가린다. 실패한 입력 명령, 거부된 명령, `/`나 템플릿 치환이 든 `eval`은 내용 없이 기록된다. 반대로 `wait --url`과 `wait --text`의 인자는 준 그대로 기록되므로 비밀을 넣지 않는다.
- 브리지가 세션의 프로필이 아닌 곳에 탭을 만들면 프록시는 그 탭을 닫고 닫힌 것을 확인한 뒤 요청을 거부한다(2초까지 재시도). 그래도 닫히지 않으면 오류 메시지에 그렇게 적고 데몬 로그에 타깃 ID를 남기며, 다른 프로필에 탭이 남을 수 있다.
- `pegboard browser start`가 `pegboard browser stop`과 겹치면 브라우저가 다시 살아날 수 있다.
- 탭을 사람이 다른 세션의 그룹으로 끌어도 소속은 바뀌지 않는다. 그 탭은 원래 세션이 계속 조종하고, 그 탭이 든 그룹의 세션을 닫으면 함께 닫힌다(소속은 탭을 따라가지 그룹을 따라가지 않는다. 그룹을 따르게 할지는 2단계에서 정한다).
- 사람이 세션의 탭을 닫아도 그 세션의 `tab list`에는 닫힌 탭이 남아 보일 수 있다. 그 탭을 쓰는 명령은 실패하고, 새 탭은 정상으로 만들어진다.
- 세션의 마지막 탭이 닫힌 뒤 새 탭을 만들면 새 탭그룹이 생긴다. 에이전트 프로필 창이 여럿이면 원래 창이 아닌 창에 생길 수 있다.
- `screenshot`과 `pdf`가 쓴 파일은 세션을 닫을 때 지워진다. 필요하면 닫기 전에 가져간다.
- `network requests --filter`의 인자는 준 그대로 동작 기록에 남는다. 비밀을 넣지 않는다.
- 데몬이 강제로 죽으면(`kill -9` 등) 브라우저는 그대로 떠 있을 것으로 보인다(코드를 읽고 한 판단이고 시험하지 않았다. Chrome 프로세스에 `kill_on_drop`만 걸려 있어 정상 종료 때만 정리된다). 같은 데이터 디렉터리로 다시 `browser start`를 하기 전에 그 Chrome을 직접 닫아야 한다.
- Chrome을 `--remote-allow-origins`로 띄우지 않는다. 열면 웹 페이지가 브라우저의 디버깅 포트에 붙을 수 있다.

## 시험

```bash
cargo test --workspace                                               # 브라우저 없이 도는 시험
cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1   # 격리 시험 묶음
cargo test --workspace -- --ignored --test-threads=1                 # 실제 브라우저를 띄우는 전체 시험 (몇 분, 창이 뜬다)
```

격리 시험 묶음의 `agent_actions_do_not_change_the_humans_active_tab`은 시험용 브라우저 창이 완전히 가려지거나, 최소화되거나, 다른 Space에 있거나, 화면이 잠긴 상태면 "cannot determine"으로 실패한다(활성 탭을 `visibilityState`로 판정하기 때문이다). 시험 중에는 그 창을 가리지 않는다.

실제 브라우저를 띄우는 시험은 `#[ignore]`가 붙어 있고, 임시 `PEGBOARD_HOME`에서 돌며 끝나면 브라우저를 정리한다. 격리 시험 묶음(`crates/pegboard-cli/tests/e2e_isolation.rs`)은 CLI만으로 에이전트가 할 수 있는 일을 시도해 사람 프로필과 다른 세션의 탭에 닿지 못함을 확인한다. 이후 모든 단계에서 유지한다. 손으로 하는 확인은 [docs/phase1-manual-checks.md](docs/phase1-manual-checks.md).

## 라이선스

이 저장소의 코드는 MIT([LICENSE](LICENSE)). 동봉하는 agent-browser는 Apache-2.0이다([THIRD_PARTY.md](THIRD_PARTY.md)).
