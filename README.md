# pegboard-browser

AI 에이전트가 브라우저를 쓰되, 사람이 쓰는 브라우저 데이터와 섞이지 않게 하는 도구다. 에이전트는 격리된 세션 안에서만 탭을 열고 조작한다. 사람은 같은 브라우저를 별도 프로필로 그대로 쓴다. 세션마다 CDP 프록시가 붙어 그 세션의 탭만 보이게 하고, 세션에서 한 동작은 비밀번호 같은 값을 가린 채 기록으로 남는다. 조작 엔진으로는 [agent-browser](https://github.com/vercel-labs/agent-browser) 0.42.0을 동봉해 그대로 쓴다.

설계는 [docs/architecture.md](docs/architecture.md), 단계별 계획은 [docs/roadmap.md](docs/roadmap.md).

## 상태

**2a단계(MCP와 세션 정책).** 1단계(로컬 MVP) 위에 `pegboard mcp`(MCP 서버), 세션 정책(동작 분류, 주소 규칙), 세션별 다운로드 폴더, 업로드, 클립보드 차단, 쉬는 세션 닫기를 더했다. 자동 시험은 끝났다(2026-11-14). 순정 Chrome for Testing 위에서 동작한다. 설치해서 쓰는 브라우저가 아니고, Chromium 포크는 3단계 계획이다. macOS(arm64)에서만 동작한다. 1단계의 손으로 하는 확인 9개(M1~M7, M8, M8b)는 2026-11-13에 수행했다([docs/phase1-manual-checks.md](docs/phase1-manual-checks.md)). 그때 다운로드 경로(M5) 하나가 기대와 달랐고(에이전트가 받은 파일이 `~/Downloads`에 저장됨) 2a에서 세션 폴더로 받게 고쳤다. 2a의 손으로 하는 확인은 [docs/phase2a-manual-checks.md](docs/phase2a-manual-checks.md)에 있다(아직 수행하지 않았다).

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

### CLI

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

pegboard files list           # 세션 파일 디렉터리의 파일 (받은 파일, 스크린숏)
pegboard session info "$PEGBOARD_SESSION"   # 세션과 정책
pegboard session list         # 세션 목록 (토큰은 나오지 않는다)
pegboard recording list       # 동작 기록 목록
pegboard recording show <기록 id>

pegboard session close "$PEGBOARD_SESSION"   # 탭을 닫고 기록을 마감한다
pegboard browser stop
```

`--session <핸들>` 플래그로도 줄 수 있지만 `ps`에 노출되므로 권하지 않는다. 핸들이 없으면 종료 코드 2, 틀린 핸들이면 `session_not_found`와 종료 코드 1이다. 종료 코드: 0 성공, 1 명령 실패(정책 거부 `policy_denied` 포함), 2 사용법 오류나 거부된 명령(프로필 설정 충돌 `profile_policy_conflict` 포함), 3 pegboard가 떠 있지 않음.

### MCP

MCP 클라이언트 설정에 `pegboard mcp`를 stdio 서버로 넣는다. `args`의 옵션이 그 서버가 만드는 모든 세션의 정책이다(아래 "세션 정책"). `pegboard`가 `PATH`에 없으면 `command`에 절대 경로를 쓴다.

```json
{
  "mcpServers": {
    "pegboard": {
      "command": "pegboard",
      "args": ["mcp", "--allow-domain", "example.com"]
    }
  }
}
```

데몬이 떠 있지 않으면 `pegboard mcp`가 띄운다. 설정하기 전에 손으로 확인하려면(세션 하나를 만들고, 입력이 끝나면 그 세션을 닫고 끝난다):

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pegboard_session_create","arguments":{"url":"https://example.com"}}}' | pegboard mcp --allow-domain example.com
```

- 에이전트는 `pegboard_session_create`로 세션을 만들고, 받은 세션 ID(`s_xxxxxxxx`)를 다른 도구의 `sessionId`에 준다. 토큰은 MCP 서버 프로세스만 쥐고 도구 결과에는 나오지 않는다.
- 기본 모드의 서버는 자기가 만들었거나 넘겨받은 세션만 보고 다룬다. 사람이 CLI로 만든 세션을 넘기려면 서버를 띄울 때 환경 변수 `PEGBOARD_SESSION`에 핸들을 준다(클라이언트 설정의 `env`). 넘겨받은 세션은 그 세션의 정책을 그대로 쓴다.
- 서버가 끝나면(stdin이 닫히거나 SIGTERM, SIGINT, SIGHUP) 자기가 만든 세션을 닫는다. 넘겨받은 세션은 닫지 않는다. 한 서버가 동시에 열어 둘 수 있는 세션은 16개다.
- 도구: 관리 도구(`pegboard_session_*`, `pegboard_recording_*`, `pegboard_files_list`, `pegboard_browser_status`)와, 허용 명령 문법의 첫 단어마다 하나씩인 브라우저 동작 도구(`pegboard_open`, `pegboard_snapshot`, `pegboard_click`, `pegboard_tab` 등). `--tools`로 동작 도구 묶음을 고른다: `core`(페이지 동작), `tabs`(`tab`), `state`(`cookies`, `storage`), `network`(`network`, `console`, `errors`, `vitals`, `a11y`), `all`. 기본은 `core,tabs`.
- 동작 도구는 인자를 CLI와 같은 명령줄로 바꿔 데몬에 보낸다. 허용목록 검사, 정책, 주소 규칙, 동작 기록이 CLI와 같다. 페이지에서 온 글은 결과에서 `--- AGENT_BROWSER_PAGE_CONTENT nonce=<난수> ---`와 `--- END_AGENT_BROWSER_PAGE_CONTENT nonce=<난수> ---` 사이에 들어간다(난수는 결과마다 pegboard가 새로 고른다). `pegboard_screenshot`은 그림도 돌려준다(5 MiB까지).
- `--operator`로 띄운 서버는 모든 세션을 보고, 닫고, 기록을 읽고, 프로필을 만들고 지우고, 브라우저를 끌 수 있으며, 세션을 만들 때 `policy` 인자로 정책을 줄 수 있다. 그래도 조종은 자기가 만들었거나 넘겨받은 세션만 한다(토큰이 필요하다). 에이전트에게 주는 설정에는 넣지 않는다.

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
- 마지막 명령(실행 중이면 그 끝) 뒤 1시간 동안 명령이 없는 세션은 닫힌다(기록은 마감되고 탭과 파일은 지워진다). `--no-expire`로 만든 세션은 닫히지 않는다.

## 세션 정책

세션마다 정책 하나가 붙는다. 세션을 만들 때 정해지고 바뀌지 않는다. 판정은 데몬이 한다. 정책을 주는 곳은 `pegboard session create`의 옵션, `pegboard mcp`의 옵션(그 서버가 만드는 모든 세션), `--operator` 서버의 `pegboard_session_create`의 `policy` 인자다. 기본 모드 MCP 서버의 도구로는 정책을 주거나 바꿀 수 없다. 옵션 이름은 `session create`와 `mcp`가 같다.

| 옵션 | 뜻 | 기본 |
|---|---|---|
| `--allow <분류,...>` | 기본이 거부인 분류를 연다: `upload`, `state`, `clipboard` | 없음 |
| `--deny <분류,...>` | 기본이 허용인 분류를 닫는다(예: `eval`, `download`) | 없음 |
| `--allow-domain <도메인,...>` | 주소 허용목록. 목록의 도메인과 하위 도메인만 열린다(루프백도 목록에 있어야 열린다) | 목록 없음 |
| `--allow-private-network` | 사설망 주소(RFC 1918, 링크 로컬, CGNAT, IPv6 ULA)를 연다 | 닫힘 |
| `--no-localhost` | localhost와 루프백 주소를 닫는다(허용목록에 있어도) | 열림 |
| `--upload-dir <폴더>` | 업로드할 파일이 있는 폴더(절대 경로, `--allow upload`와 함께만. pegboard 데이터 디렉터리와 겹치면 거부) | 없음 |
| `--no-expire` | 쉬어도 1시간 뒤에 닫지 않는다 | 닫음 |

| 동작 분류 | 기본 | 예 |
|---|---|---|
| `navigate`, `click`, `fill`, `snapshot`, `scroll`, `wait`, `read`, `get`, `interact` | 허용 | `open`, `click`, `fill`, `snapshot`, `get`, `tab`, `cookies`(읽기) |
| `eval` | 허용 | `eval`, `wait --fn` |
| `download` | 허용, 세션 파일 디렉터리로만 | 페이지에서 시작된 다운로드 |
| `upload` | 거부 (`--allow upload`와 `--upload-dir`이 둘 다 있어야 열린다) | `upload <선택자> <파일 이름>...` |
| `state` | 거부 (1단계와 다르다) | `cookies set`, `storage local set`, `storage session clear` |
| `clipboard` | 거부 | 페이지의 복사·잘라내기·붙여넣기, `navigator.clipboard`, `press Control+c` 같은 키 |

- 분류는 명령마다 붙는다. `eval`이 허용된 세션에서는 스크립트로 쿠키와 저장소를 쓰고(`document.cookie`, `localStorage`) 다른 주소로 탐색할 수 있다(주소 규칙은 그대로 적용된다). `state`를 거부하거나 `navigate`를 `--deny`해도 스크립트로 하는 같은 일은 막히지 않는다. 막으려면 `--deny eval`도 준다.

- 거부된 명령은 `policy_denied`와 함께 어떤 분류가 막혔는지, 사람이 어떤 옵션으로 열 수 있는지를 알려 준다.
- pegboard 자신의 포트(브라우저 디버깅 포트, 세션 프록시)로 가는 HTTP 요청은 어떤 정책에서도 요청 가드가 거부한다. CSP가 붙지 않는 문서(`data:`, `about:blank`, 정책이 허용한 로컬 주소의 문서)에서 연 웹소켓은 가드를 거치지 않고, 세션 프록시의 베어러 토큰과 Chrome 디버깅 포트의 Origin 검사가 막는다. 자세한 주소 규칙과 닿지 않는 곳은 아래 "알려진 한계"의 주소 규칙 항목.
- 클립보드 허용 여부와 주소 정책(`--allow-domain`, `--allow-private-network`, `--no-localhost`)은 프로필 단위로만 걸린다. 그 프로필에 열린 세션이 있는 동안 첫 세션의 값으로 고정되고, 다른 값을 원하는 세션은 `profile_policy_conflict`로 거부된다. 다른 값이 필요하면 다른 프로필(`--profile`)을 쓴다.
- 업로드는 `--allow upload`와 `--upload-dir`이 둘 다 있어야 열린다(하나만 주면 세션을 만들 때 거부된다). 파일은 맨 파일 이름으로만 고르며, `--upload-dir` 폴더에서 먼저 찾고 거기 없으면 세션 파일 디렉터리(받은 파일, 스크린샷)에서 찾는다. 경로, 심볼릭 링크, 폴더 밖의 파일은 거부한다. 프록시도 `DOM.setFileInputFiles`의 경로가 그 두 폴더 바로 안의 파일인지 다시 검사한다.
- 받은 파일은 `<PEGBOARD_HOME>/sessions/<id>/files`에 생기고(파일 하나 200 MiB, 세션 합계 1 GiB까지) `pegboard files list`나 `pegboard_files_list`로 본다. 세션을 닫으면 지워진다.

## 아직 안 되는 것

- 화면 녹화(지금 기록은 동작 목록뿐이다), 확인 요청(동작을 실시간으로 승인하기), 원격 제어, 프로필 복사(`profile create --from`, 임시 프로필).
- 허용목록에서 뺀 명령. 허용목록(`crates/pegboard-core/src/argv.rs`)에 없는 것은 모두 거부되며, 아래는 그중 알려 둘 만한 것이다.
  - `cookies clear`, `set media`, `set geo`, `set offline`: 필요한 CDP 메서드(`Network.clearBrowserCookies`, 에뮬레이션·네트워크 조건 계열)를 프록시가 허용하지 않는다.
  - `network route`: 요청 가로채기는 데몬의 요청 가드가 쓰므로, 에이전트용 규칙을 따로 설계해야 한다.
  - `read`, `flow`, `diff`, `download`, `network har`, `trace`, `profiler`, `inspect`, `webmcp`, `clipboard`, `react`, `pushstate`, `wait --download`: 세션 밖(로컬 파일, CLI 자체의 네트워크 접근, 인증 없는 DevTools 프록시)에 닿는다.
  - `batch`: 명령 여러 개를 묶어 한 번에 보내므로 명령별 필터를 지나치게 된다.
  - `set credentials`, `set headers`: 인자에 비밀이 들어가거나 요청 전체에 영향을 준다.

## 알려진 한계

### 연결과 권한

- 에이전트에 대한 정책 강제는 MCP 기본 모드에서만 성립한다. CLI(`pegboard`)를 쓸 수 있는 에이전트는 사람과 같은 권한을 가진다: 더 넓은 정책의 세션을 만들고, `browser stop`, `profile delete`, 운영자용 메서드를 부를 수 있다. 에이전트에게는 CLI가 아니라 MCP 서버(`pegboard mcp`, `--operator` 없이)를 준다.
- MCP 기본 모드의 에이전트는 `pegboard_session_create`의 `profile`로 이미 있는 에이전트 프로필을 이름으로 아무것이나 고를 수 있다(로그인해 둔 프로필 포함). 사람 프로필은 고를 수 없다. 로그인해 둔 프로필이 있으면 그 계정은 에이전트가 쓸 수 있는 것으로 본다.
- 세션 정책(`SessionPolicy`)은 모르는 필드를 거부한다. CLI(또는 MCP 서버)와 데몬은 같은 버전이어야 하고, 버전이 다르면 세션 만들기가 `invalid_params`로 거부될 수 있다. 바꾼 뒤에는 `pegboard browser stop`으로 옛 데몬을 끈다.
- 1~2단계는 루프백 디버깅 포트를 쓴다. 같은 사용자로 도는 로컬 프로세스는 브라우저 전체에 붙을 수 있다. 격리가 막는 것은 제공된 도구(CLI, MCP)를 통해 조종당한 에이전트이고, 셸을 가진 악의적 프로세스가 아니다. 이 브라우저의 데이터 디렉터리에는 실제 계정을 넣지 않는다.
- `--session`으로 넘긴 핸들은 프로세스 목록에 보인다. `PEGBOARD_SESSION`을 쓴다.
- `session list`, 기록 조회(가려진 로그), `profile delete`, `browser stop`, 운영자용 `session.info_any`/`session.close_any`는 토큰이 필요 없다. 소켓에 닿는 로컬 클라이언트는 누구나 다른 세션을 닫거나 그 (가려진) 기록과 정책(업로드 폴더, 허용 도메인 포함)을 읽거나 브라우저를 끌 수 있다. MCP 기본 모드는 이 메서드들을 자기 세션에만 쓴다.
- Chrome을 `--remote-allow-origins`로 띄우지 않는다. 열면 웹 페이지가 브라우저의 디버깅 포트에 붙을 수 있다.
- 데몬이 강제로 죽으면(`kill -9` 등) 브라우저는 그대로 떠 있을 것으로 보인다(코드를 읽고 한 판단이다). 같은 데이터 디렉터리로 다시 `browser start`를 하기 전에 그 Chrome을 직접 닫아야 한다.
- `pegboard browser start`가 `pegboard browser stop`과 겹치면 브라우저가 다시 살아날 수 있다.

### MCP

- 도구 호출은 한 번에 하나씩 처리한다. 오래 걸리는 명령은 다른 세션의 호출까지 늦춘다.
- 사람이 `browser stop`으로 데몬을 끈 뒤에는 MCP 서버가 다시 띄우지 않는다(`not_running`). 서버를 다시 띄운다.
- 강제로 죽은(`kill -9`) MCP 서버가 만든 세션은 닫히지 않고, 쉬는 세션 닫기(1시간)로 닫힌다(시험 `killed_mcp_server_leaves_sessions_that_expire`).
- `pegboard_session_close`의 `keepTabs`는 기본 모드에서 `invalid_params`로 거부된다(운영자 모드와 CLI의 `--keep-tabs`만 된다). 아래 "세션, 탭, 기록"의 남긴 탭 항목.
- 결과 안에서 세션 핸들처럼 생긴 글(`s_` 8자, 점, base32 26자)은 페이지의 글이라도 토큰 부분이 `[redacted]`로 바뀐다.
- 이름이 `list`, `new`, `close`인 탭은 `pegboard_tab`에서 이름으로 고를 수 없다(`tab list` 같은 다른 명령으로 읽히므로 거부한다). `t1` 같은 탭 ID를 쓴다.

### 주소 규칙 (2a)

- 에이전트 프로필의 요청은 데몬의 요청 가드가 판정한다. pegboard 자신의 포트는 늘 막히고, 사설망은 `--allow-private-network`가 있어야, localhost는 `--no-localhost`가 없어야 열린다. `--allow-domain`이 있으면 목록만 열리고, 목록의 이름이 루프백으로 풀려도 루프백 이름이나 주소가 함께 목록에 없으면 막힌다. 막힌 탐색은 `policy_denied`로 끝나고, 페이지 안의 막힌 요청은 그 요청의 실패로 보인다.
- 호스트 이름은 데몬이 따로 풀어 판정한다. 브라우저가 다르게 풀면(DNS-over-HTTPS, DNS rebinding) 공개 이름으로 사설 주소에 닿을 수 있고, 그 요청의 부작용은 막지 못한다. 데몬이 2초 안에 풀지 못한 이름은 막는다.
- 웹소켓과 EventSource는 요청 가로채기에 보이지 않아 문서에 붙이는 CSP(`connect-src`)로만 막는다. 허용목록이 없으면 `wss:`는 어디로든 열리고(유효한 인증서가 있는 사설망 이름 포함), 평문 `ws:`는 자기 출처와 localhost(`--allow-private-network`이면 어디로든)로만 열린다. 그래서 공개 사이트의 평문 `ws:`는 막힌다.
- 정책이 허용한 루프백이나 사설망 주소의 문서(localhost 개발 서버, 사내 http 사이트)는 CSP 없이 넘긴다(다시 내려 준 문서는 Chrome이 로컬 요청을 거절한다). 그 문서가 연 웹소켓에는 주소 규칙이 적용되지 않는다.
- 최상위 `data:` 문서와 `about:blank` 탭, 서비스 워커가 응답한 문서, 스크립트 주소로 만든 전용 워커에는 CSP가 붙지 않아, 거기서 연 웹소켓(에이전트가 `eval`로 연 것 포함)은 주소 규칙 밖이다. WebTransport와 WebRTC는 재지 않았다.
- 공개 주소의 문서는 형식과 상관없이 다시 내려 주며, 16 MiB(압축을 푼 크기)까지만 된다. 동시에 다시 내려 주는 본문은 전체 128 MiB, 프로필당 32 MiB, 문서당 20초까지이고 넘으면 그 문서가 열리지 않는다. 그래서 한 페이지가 같은 프로필의 다른 문서를 늦추거나 열리지 않게 할 수 있다(막히는 쪽으로 실패한다). 다운로드는 해당하지 않는다.
- 공개 주소의 페이지는 정책이 허용해도 localhost와 사설망에 닿지 못한다(Chrome 자체의 로컬 네트워크 보호, 순정 Chrome과 같다).
- 서비스 워커의 주 스크립트 요청은 판정되지 않는다(측정에서 10번 중 0~1번만 잡혔다). 그 워커의 `importScripts`와 `fetch`는 판정된다. 공유 워커와 서비스 워커 안에서 다시 만든 워커의 요청은 판정되지 않을 수 있다.
- 에이전트 쪽 연결이 새 팝업에 붙어 가드가 가로채기를 켜기 전에 탐색을 보낼 수 있는지(경주)는 재지 않았다.
- 판정하는 브라우저 연결이 끊기면 Chrome이 가로채기를 푼다. 다시 붙을 때까지 데몬은 명령을 거부하지만, 이미 열린 페이지의 스크립트는 그동안 판정 없이 요청을 낼 수 있다. 연결이 끊기는 마지막 약 0.5초 사이에 사람 쪽에서 연 팝업은 멈춘 채로 남을 수 있다.
- 사람 프로필의 새 탭, 팝업, 워커도 시작할 때 데몬이 한 번 응답할 동안(보통 1ms 안팎) 멈춘다. 사람의 요청은 판정하지도 바꾸지도 않는다.

### 다운로드 (2a)

- 받은 파일의 이름은 페이지가 정한다. MCP 결과(다운로드 알림, `pegboard_files_list`)에서는 페이지 내용 표식 안에 넣는다.
- 받는 동안에는 데몬만 아는 프로필별 임시 폴더(`<PEGBOARD_HOME>/downloads/<프로필 디렉터리>`)에 있다. 어느 세션의 탭에서 시작됐는지 알 수 없는 것, `--deny download` 세션의 것, 한도를 넘은 것, 받는 사이 세션이 닫힌 것은 지운다. 결과는 그 세션의 다음 명령 결과와 동작 기록에 한 번 남는다. 사람 프로필의 다운로드는 그대로다.
- 한도는 진행 이벤트(약 0.5초 간격)마다 검사하므로 넘은 뒤 그만큼 더 받을 수 있다(파일은 지워진다). 한 프로필에서 동시에 받는 중인 다운로드는 합쳐 2 GiB까지라, 한 세션이 같은 프로필의 다른 세션 몫을 다 쓸 수 있다.
- Chrome은 그 순간 기본 컨텍스트인 프로필의 다운로드만 취소한다. 에이전트 창을 앞으로 가져오지 않고 기본으로 만드는 방법을 찾지 못해, 사람이 자기 창을 쓰는 동안 지우기로 한 다운로드는 임시 폴더에 끝까지 받아진 뒤 지워진다(디스크와 대역폭을 쓴다).
- 다운로드 정책을 건 연결이 끊기면 데몬은 그 프로필의 명령을 `browser_failed`로 거부하고, 정책을 다시 걸려고 그 프로필의 탭 하나를 활성화한다. **이때 에이전트 창이 앞으로 나와 사람의 키보드 초점을 가져갈 수 있다**(페이지가 일으킬 수 없는 드문 경우다). 5초 안에 못 걸면 그 프로필의 세션을 닫는다. 그 사이 남은 탭이나 이미 열린 페이지가 시작한 다운로드는 `~/Downloads`로 갈 수 있다.
- 시험용 데몬 메서드 `test.dropDownloadPolicy`는 `PEGBOARD_TEST_HOOKS`를 준 데몬에만 있다(실제 사용에서는 주지 않는다).

### 업로드 (2a)

- 파일 이름은 영문자, 숫자, `.`, `_`, `-`만 된다(공백이 든 이름은 업로드할 수 없다).
- 프록시의 경로 검사는 맨 이름 규칙을 다시 적용하지 않는다. 세션 엔드포인트에 직접 붙은 클라이언트는 업로드 폴더 바로 안의 점 파일이나 공백이 든 이름의 파일을 올릴 수 있다(폴더 밖은 안 된다).
- 명령 결과에 나온 업로드 파일의 경로는 이름으로 바꾸지만, URL 인코딩되거나 이스케이프된 경로와 폴더만 가리키는 경로는 바꾸지 못한다. 업로드 폴더의 경로 자체는 비밀이 아니다(`session info`의 정책에 보인다). 에이전트가 넘길 수 있는 것은 맨 이름뿐이다.
- 업로드 폴더가 pegboard 데이터 디렉터리와 겹치는지는 세션을 만들 때 한 번 본다. 그 뒤 사람이 폴더의 심볼릭 링크를 바꾸면 넓어진다. 검사와 Chrome이 파일을 읽는 사이에 폴더의 파일이 바뀔 수도 있다(사람의 폴더다).

### 클립보드 (2a)

- `--allow clipboard`가 없는 세션의 프로필에는 세 가지를 건다: 데몬의 전용 연결로 `clipboard-read`/`clipboard-write` 권한 거부(`navigator.clipboard`), 요청 가드가 모든 탭과 프레임의 첫 스크립트보다 먼저 넣는 `copy`/`cut`/`paste` 캡처 리스너(`execCommand`, 키의 편집 명령, 페이지의 `copy` 처리기), `press`의 복사·잘라내기·붙여넣기 조합 거부(`Meta`/`Control` + `c`/`x`/`v`, `Control+Insert`, `Shift+Insert`, `Shift+Delete`). 방법은 architecture.md 11절.
- 클립보드 권한 거부를 걸거나 풀 때 그 프로필이 기본 컨텍스트가 아니면 그 프로필의 탭 하나를 활성화한다. **이때 에이전트 창이 앞으로 나와 사람의 키보드 초점을 가져갈 수 있다.** 그런 때는 넷이다: 거부를 건 연결이 끊겨 다시 걸 때, 세션을 만들 때 그 프로필에 거부가 아직 걸려 있지 않을 때(그 프로필의 첫 거부 세션, 앞의 거부를 잃은 뒤), 클립보드를 허용하는 세션을 만들 때 그 프로필에 거부가 걸려 있을 때, 처음 보는 프로필에 클립보드를 허용하는 첫 세션이 올 때(이전 데몬의 거부가 남았을 수 있어 초기화한다). 아직 창이 없는 프로필에 세션을 만들면 새 창이 열린다. 페이지는 이 중 어느 것도 일으킬 수 없지만, MCP 에이전트는 세션을 만들어 일으킬 수 있다(운영자 모드에서 클립보드를 허용하는 세션과 거부하는 세션을 번갈아 만들면 매번). 세션 만들기의 횟수 제한은 없다.
- 가드가 다시 붙으면 클립보드를 거부하는 세션들의 에이전트 연결을 먼저 끊고(에이전트는 다시 붙는다) 그 세션들의 옛 탭에 리스너를 넣은 뒤 새로 고친다. 그 탭의 입력 중이던 내용은 사라진다. 옛 탭의 처리가 끝날 때까지 그 프로필의 모든 세션(클립보드를 허용하는 세션 포함)의 명령은 거부된다.
- 클립보드를 거부하는 세션(기본)에서는 에이전트 연결의 `Page.addScriptToEvaluateOnNewDocument`(페이지보다 먼저 도는 스크립트)를 프록시가 거부한다. agent-browser의 `vitals`는 이것을 한 번 보내지만 거부되어도 결과를 낸다(측정). `--allow clipboard` 세션은 쓸 수 있다. 그래도 새 탭과 프레임에서는 가드가 먼저 붙어 리스너가 먼저 돈다(두 번째 방어).
- 실행 중인 채로 붙는 프레임(`sandbox`의 `srcdoc` iframe)은 리스너를 넣은 뒤 그 프레임을 다시 읽게 하므로 그 프레임의 상태가 사라진다. 같은 프레임이 3번을 넘거나 리스너를 넣지 못하면 그 탭을 닫는다.
- `document.open()`은 리스너를 지운다. 리스너 스크립트가 `open`/`write`를 감싸 다시 달고, 가드가 `Page.documentOpened`마다 첫 리스너가 자기 것인지 확인해 아니면 문서를 새로 고치거나 탭을 닫는다. 이 확인은 비동기라 그 사이(한 번의 `eval` 안에서 `open`, `write`, 복사를 다 하는 경우)는 감싼 함수만 지킨다. 읽기가 끝난 뒤에도 계속 `document.write`를 하는 페이지에서는 그때마다 클립보드를 거부하는 세션의 명령이 잠깐씩 거부된다.
- 가드는 새 탭과 팝업에 리스너를 넣는 명령을 재개 전에 보내고 답은 재개 뒤에 기다린다(15초 안에 답이 없으면 그 탭을 닫는다). 브라우저가 그 명령을 첫 스크립트보다 먼저 처리한다는 것은 시험 `new_tabs_popups_and_frames_are_covered`로 확인한다.
- 오른쪽 클릭 메뉴의 복사/붙여넣기와 사람이 에이전트 창에서 직접 누른 키는 재지 않았다(수동 확인 N1, N2). 리스너는 사람의 복사도 막을 것으로 보인다.
- 시험용 환경 변수 `PEGBOARD_TEST_NO_CLIPBOARD_SHIELD`는 `PEGBOARD_TEST_HOOKS`와 함께 줄 때만 리스너를 끈다(실제 사용에서는 주지 않는다).

### 세션, 탭, 기록

- 세션의 탭이 하나도 남지 않으면 `open`과 `tab` 명령만 실행되고, 나머지 명령은 `tab_gone` 오류로 끝난다. `tab new <url>`이나 `open <url>`로 탭을 다시 연다. 그때 새 탭그룹이 생기고, 에이전트 프로필 창이 여럿이면 원래 창이 아닌 창에 생길 수 있다.
- 탭을 사람이 다른 세션의 그룹으로 끌어도 소속은 바뀌지 않는다(소속은 탭을 따라간다). 그 그룹의 세션을 닫아도 그 탭은 닫히지 않고 그룹에 남는다.
- 주소 정책(`--allow-domain` 허용목록 포함)은 세션별이 아니라 프로필 단위로 걸린다. 그 프로필을 고정한(pin) 세션이 하나라도 열려 있는 동안만 그 값이고, 마지막 세션이 닫히면 그 프로필은 기본 주소 정책으로 돌아간다. 세션을 닫을 때는 탭을 다 닫거나(`--keep-tabs`면 그룹에서 뺀) 뒤에 고정을 푼다. 그래서 `--keep-tabs`(CLI, 운영자 모드 MCP)로 남긴 탭은 그 뒤 그 프로필의 기본 주소 정책을 따른다(허용목록 밖에 닿을 수 있다). 사람이 고르는 일이라 MCP 기본 모드는 `keepTabs`를 거부한다.
- 세션 닫기가 실패하면 세션 없이 탭만 에이전트 창에 남을 수 있다. 그 탭도 고정이 풀린 뒤에는 기본 주소 정책을 따른다. 두 세션을 동시에 닫으면 한쪽의 `--keep-tabs`가 지켜지지 않을 수 있다.
- 브리지가 세션의 프로필이 아닌 곳에 탭을 만들면 프록시는 그 탭을 닫고 확인한 뒤 요청을 거부한다(2초까지 재시도). 그래도 닫히지 않으면 오류 메시지와 데몬 로그에 남기며, 다른 프로필에 탭이 남을 수 있다.
- `screenshot`과 `pdf`가 쓴 파일과 받은 파일은 세션을 닫을 때 지워진다. 필요하면 닫기 전에 가져간다.
- 동작 기록은 공격적으로 가린다. 실패한 입력 명령, 거부된 명령, `/`나 템플릿 치환이 든 `eval`은 내용 없이 기록된다. 반대로 그 밖의 `eval`, `wait --url`, `wait --text`, `network requests --filter`의 인자는 준 그대로 기록되므로 비밀을 넣지 않는다.
- 크기 상한(인자 하나 64 KiB, 전체 256 KiB, 단어 256개)을 넘어 거부된 명령(`limit_exceeded`)은 동작 기록에 전혀 남지 않는다.

## 시험

```bash
cargo test --workspace                                               # 브라우저 없이 도는 시험
cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1       # 격리 시험 묶음 (CLI)
cargo test -p pegboard-cli --test e2e_mcp_isolation -- --ignored --test-threads=1   # 격리 시험 묶음 (MCP)
cargo test --workspace -- --ignored --test-threads=1                 # 실제 브라우저를 띄우는 전체 시험 (몇 분, 창이 뜬다)
cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1  # 클립보드 시험
```

클립보드 시험과 MCP 격리 시험 묶음의 `mcp_agent_cannot_write_the_clipboard`는 시스템 클립보드를 저장하고 표식을 넣었다가 되돌린다(비어 있었으면 다시 비운다). 텍스트가 아닌 내용이 있으면 돌지 않는다.

격리 시험 묶음의 `agent_actions_do_not_change_the_humans_active_tab`은 시험용 브라우저 창이 완전히 가려지거나, 최소화되거나, 다른 Space에 있거나, 화면이 잠긴 상태면 "cannot determine"으로 실패한다(활성 탭을 `visibilityState`로 판정하기 때문이다). 시험 중에는 그 창을 가리지 않는다. `mcp_agent_cannot_reach_pegboards_own_ports_or_the_private_network`는 기본 경로의 네트워크 인터페이스에 사설(RFC 1918) IPv4 주소가 있어야 돈다.

실제 브라우저를 띄우는 시험은 `#[ignore]`가 붙어 있고, 임시 `PEGBOARD_HOME`에서 돌며 끝나면 브라우저를 정리한다. 격리 시험 묶음 둘은 에이전트가 할 수 있는 일을 시도한다. CLI 묶음(`crates/pegboard-cli/tests/e2e_isolation.rs`)은 세션 핸들 하나로 agent-browser 명령만 쓰는 에이전트가 사람 프로필, 다른 세션, 세션 밖의 브라우저, 토큰에 닿지 못함을 확인한다(CLI 전체를 가진 에이전트는 사람의 권한이라 정책을 넓히는 시도는 다루지 않는다). MCP 묶음(`e2e_mcp_isolation.rs`)은 MCP 도구만으로 같은 것에 더해 더 넓은 정책과 세션 밖에 남는 탭에 닿지 못함을 확인한다. 시험마다 새 브라우저를 띄우며, 이후 모든 단계에서 유지한다. 손으로 하는 확인은 [docs/phase1-manual-checks.md](docs/phase1-manual-checks.md)와 [docs/phase2a-manual-checks.md](docs/phase2a-manual-checks.md).

시험용 환경 변수는 모두 `PEGBOARD_TEST_HOOKS`를 함께 줄 때만 읽는다(실제 사용에서는 주지 않는다): `PEGBOARD_TEST_HOST_RULES`, `PEGBOARD_TEST_BROWSER_HOST_RULES`, `PEGBOARD_TEST_IP_SPACE_OVERRIDES`, `PEGBOARD_TEST_LAUNCH_LOG`, `PEGBOARD_TEST_IDLE_SECS`, `PEGBOARD_TEST_DOWNLOAD_LIMITS`, `PEGBOARD_TEST_NO_CLIPBOARD_SHIELD`. `PEGBOARD_TEST_HOOKS`는 데몬의 시험용 메서드와 진단 기록(daemon.log)도 켠다. `PEGBOARD_TEST_MCP_SLOW_CREATE`는 디버그 빌드의 MCP 서버에만 있다.

## 라이선스

이 저장소의 코드는 MIT([LICENSE](LICENSE)). 동봉하는 agent-browser는 Apache-2.0이다([THIRD_PARTY.md](THIRD_PARTY.md)).
