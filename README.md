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

pegboard files list           # 세션 파일 디렉터리의 파일 (받은 파일, 스크린숏)
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
  - `read`, `flow`, `diff`, `download`, `network route`, `network har`, `trace`, `profiler`, `inspect`, `webmcp`, `clipboard`, `react`, `pushstate`, `wait --download`: 세션 밖(로컬 파일, CLI 자체의 네트워크 접근, 인증 없는 DevTools 프록시)에 닿는다.
  - `upload <선택자> <파일이름>...`: 사람이 `--allow upload --upload-dir <폴더>`로 만든 세션에서만 된다. 파일은 그 폴더(또는 세션 파일 디렉터리)의 맨 파일 이름으로만 고르며, 심볼릭 링크와 폴더 밖의 이름은 거부한다. 프록시도 `DOM.setFileInputFiles`의 경로가 그 두 폴더 안인지 다시 검사한다.
  - `batch`: 명령 여러 개를 묶어 한 번에 보내므로 명령별 필터를 지나치게 된다.
  - `set credentials`, `set headers`: 인자에 비밀이 들어가거나 요청 전체에 영향을 준다. 허용목록에 없다.

## 알려진 한계

- 1~2단계는 루프백 디버깅 포트를 쓴다. 같은 사용자로 도는 로컬 프로세스는 브라우저 전체에 붙을 수 있다. 격리가 막는 것은 제공된 도구를 통해 조종당한 에이전트이고, 셸을 가진 악의적 프로세스가 아니다. 이 브라우저의 데이터 디렉터리에는 실제 계정을 넣지 않는다.
- 주소 규칙(2a): 에이전트 프로필의 요청은 데몬이 판정한다. pegboard 자신의 포트(디버깅 포트, 세션 프록시)는 늘 막히고, 사설망은 `--allow-private-network`가 있어야, localhost는 `--no-localhost`가 없어야 열린다. `--allow-domain`을 주면 목록의 도메인과 그 하위 도메인만 열린다. 이때 localhost는 목록에 있어야 열리고(`--no-localhost`는 허용목록이 있어도 localhost를 닫는다), 목록의 이름이 루프백 주소로 풀려도 `localhost`(또는 `*.localhost`나 루프백 주소)가 함께 목록에 없으면 막힌다. 막힌 탐색은 `policy_denied`와 이유로 끝나고, 페이지 안의 막힌 요청은 그 요청의 실패로 보인다. 아래는 이 규칙이 닿지 않는 곳이다.
- 호스트 이름은 데몬이 따로 풀어서 판정한다. 브라우저가 다르게 풀면(DNS-over-HTTPS, 두 번째 조회가 다른 주소를 주는 DNS rebinding) 공개 이름으로 사설 주소에 닿을 수 있고, 그 요청이 내는 부작용은 막지 못한다. 데몬이 2초 안에 풀지 못한 이름은 막는다.
- 웹소켓과 EventSource는 요청 가로채기에 보이지 않아, 문서에 붙이는 CSP(`connect-src`)로만 막는다. 허용목록이 없으면 `wss:`는 어디로든 열려 있어 유효한 인증서가 있는 사설망 이름에는 연결되고, 평문 `ws:`는 자기 출처와 localhost(또는 `--allow-private-network`이면 어디로든)로만 열린다(공개 사이트의 평문 `ws:`도 막힌다).
- 정책이 허용한 루프백이나 사설망 주소에서 온 문서(localhost 개발 서버, 사내 http 사이트)는 CSP 없이 그대로 넘긴다. 다시 내려 준 문서는 주소 공간을 잃어, Chrome이 보안 출처가 아닌 문서의 로컬 요청을 아예 거절하기 때문이다. 그 문서의 `fetch`, 이미지 같은 요청은 그대로 판정되지만, 그 문서가 연 웹소켓에는 주소 규칙이 적용되지 않는다.
- 최상위 `data:` 문서와 `about:blank` 탭(에이전트가 연 것 포함), 서비스 워커가 응답한 문서, 스크립트 주소로 만든 전용 워커에는 이 CSP가 붙지 않아, 거기서 연 웹소켓(에이전트가 `eval`로 연 것 포함)에는 주소 규칙이 적용되지 않는다. WebTransport와 WebRTC는 재지 않았다.
- 공개 주소에서 온 문서는 형식과 상관없이(그림, 동영상, PDF, 평문 포함) 다시 내려 주며, 16 MiB(압축을 푼 크기)까지만 다시 내려 줄 수 있다. 그보다 큰 문서는 열리지 않는다. 모든 탭에서 동시에 다시 내려 주는 본문은 합쳐 128 MiB, 한 프로필에서는 32 MiB까지이고 하나를 읽는 데 20초까지다. 넘으면 그 문서가 열리지 않는다.
- 그래서 한 페이지가 큰 문서를 여럿 열거나 본문을 느리게 보내면, 같은 프로필의 다른 공개 주소 문서를 그 몫(32 MiB, 20초)만큼 늦추거나 열리지 않게 할 수 있다(가용성의 문제이고, 막히는 쪽으로 실패한다). 다른 프로필의 문서에는 닿지 않는다. 첨부 파일과 `application/octet-stream`, 압축 파일 같은 다운로드는 해당하지 않는다.
- 에이전트 쪽 연결이 새 팝업에 붙어 가드가 가로채기를 켜기 전에 탐색을 보낼 수 있는지(경주)는 재지 않았다.
- 서비스 워커의 주 스크립트 요청은 판정되지 않는다(측정에서 10번 중 0~1번만 잡혔다). 서비스 워커의 `importScripts`와 `fetch`는 판정된다.
- 공개 주소의 페이지는 localhost와 사설망에 닿지 못한다(정책이 허용해도). Chrome 자체의 로컬 네트워크 보호가 그대로 걸리기 때문이며, 순정 Chrome과 같다.
- 사람 프로필의 새 탭, 팝업, 워커도 시작할 때 데몬이 한 번 응답할 동안(보통 1ms 안팎) 멈춘다. 사람의 요청은 판정하지도 바꾸지도 않는다.
- 공유 워커와 서비스 워커 안에서 다시 만든 워커는 따로 붙지 않아, 그 워커가 낸 요청은 판정되지 않을 수 있다.
- 판정하는 브라우저 연결이 끊기면 Chrome이 가로채기를 푼다. 다시 붙을 때까지 데몬은 명령을 거부하지만, 이미 열린 페이지의 스크립트는 그동안 판정 없이 요청을 낼 수 있다.
- 다운로드(2a): 에이전트 프로필의 다운로드는 그 다운로드를 시작한 탭의 세션 파일 디렉터리(`<PEGBOARD_HOME>/sessions/<id>/files`, 파일은 0600)로 간다. 받는 동안에는 데몬만 아는 프로필별 임시 폴더(`<PEGBOARD_HOME>/downloads/<프로필 디렉터리>`)에 있다. 어느 세션의 탭에서 시작됐는지 알 수 없는 다운로드, `--deny download` 세션의 다운로드, 파일 하나가 200 MiB를 넘거나 세션 합계가 1 GiB를 넘는 다운로드, 받는 사이 세션이 닫힌 다운로드는 지운다. 이름은 경로 구분자와 제어 문자를 뺀 원래 이름이고(최대 200바이트), 같은 이름이 있으면 `-1`, `-2`를 붙인다. 결과는 그 세션의 다음 명령 결과의 `downloads`(`--json`, 아니면 표준 오류에 한 줄씩)와 동작 기록(`download` 항목)에 한 번 남는다. 받은 파일은 `pegboard files list`로 본다(세션 토큰 필요). 사람 프로필의 다운로드는 그대로다.
- 한도는 진행 이벤트(약 0.5초 간격)마다, 같은 세션에서 받는 중인 다른 다운로드와 이미 받은 파일을 합쳐 검사하므로 한도를 넘은 뒤 그 간격만큼 더 받을 수 있다(파일은 지워진다). 한 프로필에서 동시에 받는 중인 다운로드(모든 세션과 세션을 알 수 없는 것 합계)는 2 GiB를 넘지 못한다. 그래서 한 세션이 받는 중인 다운로드가 같은 프로필의 다른 세션 몫을 다 쓸 수 있다.
- 지우기로 한 다운로드는 그 자리에서 취소한다(세션을 알 수 없는 것 포함). 그러나 Chrome은 그 순간 기본 컨텍스트인 프로필의 다운로드만 취소한다(측정: 사람 프로필이 기본일 때 거절됨. 에이전트 프로필에 뒤에서 탭을 만드는 것도 거절됨). 에이전트 프로필을 기본으로 만드는 방법으로 측정된 것은 그 프로필의 탭을 활성화하는 것뿐인데, 그러면 에이전트 창이 앞으로 나와 사람이 치던 키를 가져갈 수 있다. 취소는 페이지가 마음대로 일으킬 수 있으므로 그렇게 하지 않는다. 그래서 사람 프로필이 기본일 때(사람이 자기 창을 쓰고 있을 때) 지우기로 한 다운로드는 임시 폴더에 끝까지 받아진 뒤 지워진다. 그동안 디스크와 대역폭을 쓰며, 프로필당 받는 중 2 GiB 한도 안이다.
- 다운로드 정책은 데몬이 프로필마다 쥐고 있는 브라우저 연결에 걸려 있다. 그 연결이 끊기면 데몬은 그 프로필의 세션 명령을 `browser_failed`로 거부하고, 정책을 다시 걸기 위해 그 프로필의 탭 하나를 활성화한다. **이때 에이전트 창이 앞으로 나와 사람의 키보드 초점을 가져갈 수 있다**(페이지가 일으킬 수 없는 드문 경우다). 그런 다음 받는 중이던 다운로드를 취소하고 정책을 다시 건다. 5초 안에 다시 걸지 못하면 그 프로필의 세션을 닫고 다시 걸기를 멈춘다. 그 프로필에 다음 세션을 만들 때 다시 건다. 그 사이 그 프로필에 남은 탭(세션의 탭은 닫힌다)이나 끊긴 사이에 이미 열린 페이지의 스크립트가 시작한 다운로드는 `~/Downloads`로 갈 수 있다(Chrome의 기본 동작, 측정 L5).
- 시험용 데몬 메서드 `test.dropDownloadPolicy`는 `PEGBOARD_TEST_HOOKS`를 준 데몬에만 있다(실제 사용에서는 주지 않는다). 그 프로필의 다운로드 정책을 끊어, 그 프로필의 세션을 닫게 하고 남은 탭의 다운로드가 `~/Downloads`로 갈 수 있는 틈을 연다.
- 세션의 탭이 하나도 남지 않으면 `open`과 `tab` 명령만 실행되고, 나머지 명령은 `tab_gone` 오류로 끝난다. `tab new <url>`이나 `open <url>`로 탭을 다시 연다.
- 클립보드(2a): `--allow clipboard`가 없는 세션은 시스템 클립보드를 읽지도 바꾸지도 못한다. 클립보드는 프로필 단위 설정이라 같은 프로필의 세션은 모두 같아야 한다(다르면 `profile_policy_conflict`). 막는 방법은 세 가지다. (1) 데몬이 프로필마다 쥔 전용 연결로 `clipboard-read`와 `clipboard-write` 권한을 거부한다(`navigator.clipboard`). (2) 요청 가드가 모든 탭과 프레임에 첫 스크립트보다 먼저 `copy`/`cut`/`paste` 캡처 리스너를 넣는다(`execCommand`, 키의 편집 명령, 페이지의 `copy` 처리기). (3) `press`의 복사, 잘라내기, 붙여넣기 조합(`Meta`/`Control` + `c`/`x`/`v`, `Control+Insert`, `Shift+Insert`, `Shift+Delete`, 대소문자와 순서 무관)은 `clipboard` 분류로 거부한다. 아래는 이것이 닿지 않는 곳이다.
- 권한 거부를 건 연결이 끊기면 그 프로필의 세션 명령을 `browser_failed`로 거부하고 다시 건다. 다시 걸 때와 이미 열린 프로필에서 세션의 클립보드 설정이 바뀔 때(허용하던 세션들 뒤에 거부하는 세션, 또는 그 반대), 그 프로필이 기본 컨텍스트가 아니면 그 프로필의 탭 하나를 활성화한다. **이때 에이전트 창이 앞으로 나와 사람의 키보드 초점을 가져갈 수 있다**(다운로드 정책과 같고, 페이지가 일으킬 수 없는 경우다). 5초 안에 다시 걸지 못하면 그 프로필의 세션을 닫는다.
- 가드의 브라우저 연결이 끊겼다 다시 붙으면, 클립보드를 거부하는 세션들의 에이전트 연결(세션 CDP 엔드포인트에 붙은 연결)을 먼저 끊고(에이전트는 다시 붙는다), 그 전부터 열려 있던 그 세션들의 탭에 리스너를 넣은 뒤 새로 고친다. 끊긴 연결이 탭에 남긴 세션은 가드보다 오래되어, 그 연결이 건 새 문서용 스크립트가 리스너보다 먼저 돌 수 있기 때문이다(측정 C12a). 그래서 그 탭의 입력 중이던 내용은 사라진다. 연결을 끊은 뒤 브라우저가 그 세션들을 떼어 내기를 0.3초 기다리는데, 그보다 늦으면 그 사이의 문서에는 이 보장이 없다.
- 에이전트 쪽 연결이 `Page.addScriptToEvaluateOnNewDocument`를 쓰는 것은 막지 않는다(agent-browser의 `vitals`가 쓴다). 새 탭과 프레임에서는 가드가 먼저 붙어 그 스크립트가 리스너보다 늦게 돈다. 가드가 다시 붙을 때는 위처럼 그 전의 에이전트 연결을 끊는다.
- 이미 실행 중인 채로 붙는 프레임(`sandbox`의 `srcdoc` iframe)은 그 문서의 스크립트가 리스너보다 먼저 돌아, 그 문서가 먼저 단 캡처 리스너가 이벤트를 멈추면 복사가 된다(측정). 그래서 가드는 그런 프레임에 리스너를 넣은 뒤 그 프레임 안에서 `location.reload()`를 불러 다시 읽게 한다(다음 문서에서는 리스너가 먼저 돈다). 그 프레임의 상태는 사라진다. 한 탭에서 이렇게 다시 읽게 하는 프레임은 8개까지이고, 넘으면 그 뒤의 프레임에는 이 보장이 없다.
- 오른쪽 클릭 메뉴의 복사/붙여넣기와 사람이 에이전트 창에서 직접 누른 키는 재지 않았다. 리스너는 사람의 복사도 막을 것으로 보인다.
- 연결이 폭주하다 끊기는 마지막 약 0.5초 사이에 사람 쪽에서 연 팝업은 멈춘 채로 남을 수 있다.
- `--session`으로 넘긴 핸들은 프로세스 목록에 보인다. `PEGBOARD_SESSION`을 쓴다.
- `session list`, 기록 조회(가려진 로그), `profile delete`, `browser stop`은 토큰이 필요 없다. 로컬 클라이언트라면 누구나 다른 세션의 (가려진) 기록을 읽거나 브라우저를 끌 수 있다.
- 크기 상한(인자 하나 64 KiB, 전체 256 KiB, 단어 256개)을 넘어 거부된 명령(`limit_exceeded`)은 동작 기록에 전혀 남지 않는다.
- 핸들을 잃어버린 세션은 마지막 명령(실행 중이면 그 끝) 뒤 1시간이 지나면 닫힌다(기록은 마감되고 탭과 파일은 지워진다). `session create --no-expire`로 만든 세션은 닫히지 않는다. 그 전에 닫으려면 `pegboard browser stop`을 쓴다.
- 세션 닫기가 실패하면 세션 없이 탭만 에이전트 창에 남을 수 있다.
- 동작 기록은 공격적으로 가린다. 실패한 입력 명령, 거부된 명령, `/`나 템플릿 치환이 든 `eval`은 내용 없이 기록된다. 반대로 `wait --url`과 `wait --text`의 인자는 준 그대로 기록되므로 비밀을 넣지 않는다.
- 브리지가 세션의 프로필이 아닌 곳에 탭을 만들면 프록시는 그 탭을 닫고 닫힌 것을 확인한 뒤 요청을 거부한다(2초까지 재시도). 그래도 닫히지 않으면 오류 메시지에 그렇게 적고 데몬 로그에 타깃 ID를 남기며, 다른 프로필에 탭이 남을 수 있다.
- `pegboard browser start`가 `pegboard browser stop`과 겹치면 브라우저가 다시 살아날 수 있다.
- 탭을 사람이 다른 세션의 그룹으로 끌어도 소속은 바뀌지 않는다. 그 탭은 원래 세션이 계속 조종한다(소속은 탭을 따라가지 그룹을 따라가지 않는다). 그 그룹의 세션을 닫아도 그 탭은 닫히지 않고 그룹에 남는다(2a부터).
- 세션의 마지막 탭이 닫힌 뒤 새 탭을 만들면 새 탭그룹이 생긴다. 에이전트 프로필 창이 여럿이면 원래 창이 아닌 창에 생길 수 있다.
- `screenshot`과 `pdf`가 쓴 파일과 받은 파일은 세션을 닫을 때 지워진다. 필요하면 닫기 전에 가져간다.
- `network requests --filter`의 인자는 준 그대로 동작 기록에 남는다. 비밀을 넣지 않는다.
- 데몬이 강제로 죽으면(`kill -9` 등) 브라우저는 그대로 떠 있을 것으로 보인다(코드를 읽고 한 판단이고 시험하지 않았다. Chrome 프로세스에 `kill_on_drop`만 걸려 있어 정상 종료 때만 정리된다). 같은 데이터 디렉터리로 다시 `browser start`를 하기 전에 그 Chrome을 직접 닫아야 한다.
- Chrome을 `--remote-allow-origins`로 띄우지 않는다. 열면 웹 페이지가 브라우저의 디버깅 포트에 붙을 수 있다.

## 시험

```bash
cargo test --workspace                                               # 브라우저 없이 도는 시험
cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1   # 격리 시험 묶음
cargo test --workspace -- --ignored --test-threads=1                 # 실제 브라우저를 띄우는 전체 시험 (몇 분, 창이 뜬다)
cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1  # 클립보드 시험: 시스템 클립보드를 저장하고 표식을 넣었다가 되돌린다(비어 있었으면 다시 비운다). 텍스트가 아닌 내용이 있으면 돌지 않는다
```

격리 시험 묶음의 `agent_actions_do_not_change_the_humans_active_tab`은 시험용 브라우저 창이 완전히 가려지거나, 최소화되거나, 다른 Space에 있거나, 화면이 잠긴 상태면 "cannot determine"으로 실패한다(활성 탭을 `visibilityState`로 판정하기 때문이다). 시험 중에는 그 창을 가리지 않는다.

실제 브라우저를 띄우는 시험은 `#[ignore]`가 붙어 있고, 임시 `PEGBOARD_HOME`에서 돌며 끝나면 브라우저를 정리한다. 격리 시험 묶음(`crates/pegboard-cli/tests/e2e_isolation.rs`)은 CLI만으로 에이전트가 할 수 있는 일을 시도해 사람 프로필과 다른 세션의 탭에 닿지 못함을 확인한다. 이후 모든 단계에서 유지한다. 손으로 하는 확인은 [docs/phase1-manual-checks.md](docs/phase1-manual-checks.md).

## 라이선스

이 저장소의 코드는 MIT([LICENSE](LICENSE)). 동봉하는 agent-browser는 Apache-2.0이다([THIRD_PARTY.md](THIRD_PARTY.md)).
