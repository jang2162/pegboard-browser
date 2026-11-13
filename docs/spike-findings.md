# 0단계 검증 스파이크 결과

- 계획: [plans/2026-11-10-phase0-spikes.md](./plans/2026-11-10-phase0-spikes.md)
- 가정의 출처: [architecture.md 12절](./architecture.md#12-미확인-가정-모음)
- 상태: 완료 (2026-11-10). 가정 13개 모두 판정. 미실시 항목은 아래 "미실시"에 모았다.
- 표기: [확인]은 이 스파이크에서 직접 관찰한 것, [미확인]은 그렇지 않은 것. 원자료는 `spikes/out/*.json` (깃 제외).

## 환경

| 항목 | 값 |
|---|---|
| 기기 | Apple Silicon (arm64), 15코어, 메모리 48GB |
| macOS | 27.2 |
| 남은 디스크 (시작 시) | 257GB |
| Node.js | 24.21.0 |
| agent-browser | 0.42.0 |
| 브라우저 | Google Chrome for Testing 157.0.8103.50 |
| 브라우저 경로 | `~/.agent-browser/browsers/chrome-157.0.8103.50/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing` |

실험 중 알게 된 것:

- Chrome 실행 인자로 `http://127.0.0.1:<포트>/...` 주소를 주면 타깃의 URL만 바뀌고 문서는 `about:blank`에 머물렀다 [확인]. 원인은 조사하지 않았다. 모든 실험은 CDP `Page.navigate`로 페이지를 연다.
- agent-browser는 `--cdp`로 붙을 때 이미 있는 탭을 채택한다. 접속 뒤 우리가 CDP로 새로 만든 탭을 따라가지 않는다 [확인].
- agent-browser의 유닉스 소켓 경로는 길이 제한이 있어, 실험에서는 소켓 디렉터리를 `/tmp/pegspike-ab`로 두었다.
- 이 기기에는 사용자가 쓰는 agent-browser 데몬(`default` 세션)이 이미 떠 있다. 실험은 소켓 디렉터리와 세션 이름을 분리해 건드리지 않았다.

## 요약

| # | 가정 | 판정 | 근거 | 설계 영향 |
|---|---|---|---|---|
| A1 | 가려진 탭에서도 CDP 자동화와 화면 캡처가 쓸 만하게 동작한다(또는 플래그나 패치로 그렇게 만들 수 있다) | **조건부** | CDP 호출 두 개를 건 세션을 유지하면 전 시나리오 통과 (`01-occluded-cdp.json`) | 플래그도 패치도 필요 없다. `pegboardd`가 에이전트 탭마다 CDP 세션을 유지해야 한다 |
| A2 | CDP 프록시로 타깃을 걸러도 agent-browser가 정상 동작한다 | **맞다** | 13개 단계 중 동작 8종 모두 성공, 사람 탭 안 보임 (`02-proxy.json`) | 결정 2 유지 |
| A3 | agent-browser가 전체 ws URL과 `--cdp-headers`만으로 우리 엔드포인트에 붙는다 | **맞다** | HTTP 탐색 요청 0건. 토큰이 틀리면 401로 실패하고 자체 브라우저를 띄우지 않음 | 프록시에 HTTP 탐색 경로가 필요 없다 |
| A4 | 최신 CDP에도 탭그룹 제어가 없다 | **맞다** | Chrome 157 프로토콜 59개 도메인에 탭그룹 관련 항목 0 (`03-protocol-groups.txt`) | `Pegboard` 도메인 또는 확장 프로그램이 필요하다 |
| A5 | 확장 프로그램의 `chrome.tabGroups`와 타깃 ID 대응으로 포크 없이 탭그룹을 제어할 수 있다 | **조건부** | 그룹 생성, 대응, 추가 모두 성공. 단 서비스 워커가 45초 유휴 뒤 사라져 호출 불가 (`03-tabgroups.json`) | 1단계 브리지는 워커를 깨워 두는 연결이 필요하다 |
| A6 | 브라우저가 자식 프로세스에 파이프 CDP를 넘기는 패치가 작다 | **맞다** (소스 읽기 기준) | 파이프 핸들러가 임의의 fd 쌍을 받는다. 공개 함수만 fd 3, 4로 고정 | 업스트림 파일 2개에 십여 줄 추가로 가능해 보인다. 빌드로 검증하지는 않았다 |
| A7 | CDP로 만든 별도 브라우저 컨텍스트는 같은 창의 탭그룹에 넣을 수 없다 | **맞다** | 별도 창에 열리고 확장 프로그램에 보이지 않음 | 세션별 쿠키 격리는 탭그룹 안에서 불가. 결정 6 유지 |
| A8 | agent-browser의 동작 정책·확인이 `--cdp` 접속에서 동작한다 | **조건부** | 동작은 하나 내부 동작명으로만 매칭. 문서의 `deny:["eval"]`은 `eval`을 막지 못함 (`02b-policy.json`) | 정책 판정은 `pegboardd`가 직접 한다 |
| A9 | agent-browser의 세션별 스트림 서버를 끌 수 있다 | **조건부** | `stream disable`로 꺼짐. 그 전까지는 인증 없이 접속됨. 시작부터 끄는 설정은 못 찾음 | 세션 생성 직후 `stream disable`을 실행한다. 짧은 노출 구간이 남는다 |
| A10 | Chromium 전체 빌드를 가진 장비로 감당할 수 있다 | **맞다** | 전체 빌드 2시간 47분, 증분 89초, 소스와 산출물 61GB (`06-build-log.txt`) | 빈 상태에서 약 3시간 20분. 빌드 중에는 기기를 쓰기 어려워 전용 빌드 장비나 캐시가 필요하다 |
| A11 | 브라우저 수준 CDP 연결에서 타깃이 어느 프로필 것인지 확실히 구분할 수 있고, 특정 프로필에 탭과 창을 만들 수 있다 | **조건부** | 구분은 확실. 그러나 `Target.createTarget`이 프로필의 컨텍스트 ID를 거부한다 (`04-profiles-a11.json`) | **프록시 설계 변경 필요.** 탭 생성을 프록시가 가로채 다른 경로로 만들어야 한다 |
| A12 | 프로필별로 쿠키, 저장소, 확장 프로그램, 사이트 권한, 설정이 나뉜다. 나뉘지 않는 항목의 목록을 만든다 | 목록 작성 (일부 미실시) | 쿠키·저장소·권한은 분리. 새는 것 2건 (`04-profiles-a12-a13.json`) | 브라우저 수준 쿠키 메서드 차단은 필수 |
| A13 | 열려 있는 프로필의 디렉터리를 복사해 새 프로필로 등록하면 로그인 상태가 유지된 채 동작한다. 브라우저를 끄지 않고 프로필을 만들고 지울 수 있다 | **맞다** (주의 1건) | 복사 6회 모두 성공, 삭제 후 브라우저 정상. 쿠키가 디스크에 쓰이기까지 30초 | 복사 직전 30초 안의 로그인은 복사본에 없을 수 있다 |

**판정 집계**: 맞다 7 (A2, A3, A4, A6, A7, A10, A13), 조건부 5 (A1, A5, A8, A9, A11), 목록 작성 1 (A12), 틀리다 0.

**1단계 착수 판정: 착수 가능.** 조건인 A1, A2, A5, A11 중 "틀리다"가 없다. 다만 A11과 A5의 결과로 설계를 고쳐야 한다(아래 "설계 변경 제안").

## 가정별 상세

### A1. 가려진 창과 탭에서의 자동화

스크립트: `spikes/01-occluded.mjs`, `spikes/01b-raise.mjs`. 대상은 100ms마다 배경색이 바뀌는 페이지. agent-browser가 `--pin-tab`으로 연 자기 탭을 측정했다.

**기본 상태에서 숨은 탭에 일어나는 일** [확인]. 같은 창의 뒤쪽 탭, 최소화한 창, 다른 창에 가려진 창 모두 같은 양상이다(`visibilityState`가 `hidden`).

| 측정 | 보이는 탭 | 숨은 탭 |
|---|---|---|
| 스냅샷, `eval`, `get url`, `fill`, `open`, `tab new` | 36~300ms | 36~300ms (정상) |
| 스크린샷 (`Page.captureScreenshot`) | 약 25ms | 28~255ms (정상) |
| `click`, `hover` | 35~60ms | **약 5.1~5.5초** (성공은 한다) |
| 화면 캡처 (`Page.startScreencast`, 10초) | 약 100프레임 | **0~1프레임** |
| `setInterval(100ms)` | 초당 10회 | 초당 1회 |
| `requestAnimationFrame` | 초당 약 75회 | 0회 |

**구성별 결과** (통과 기준: 스냅샷·클릭 성공, 스크린샷 2초 이내, 화면 캡처 10프레임 이상. 기준은 계획서에서 정한 값이다)

| 구성 | 숨은 탭 | 최소화 | 가려진 창 | 비고 |
|---|---|---|---|---|
| 기본 | 실패 | 실패 | 실패 (`01b`) | 클릭 5초, 화면 캡처 0 |
| 플래그 3개 | 실패 | 실패 | 무효 | 타이머만 회복. 클릭 5초, 화면 캡처 0 그대로 |
| CDP 호출 2개 | **통과** | **통과** | **통과** | 클릭 32~92ms, 화면 캡처 98~101프레임. 5행 모두 유효 |
| 플래그 + CDP | **통과** | **통과** | **통과** | CDP만 쓴 것과 차이 없음 |

- 플래그 3개: `--disable-backgrounding-occluded-windows --disable-renderer-backgrounding --disable-background-timer-throttling`
- CDP 호출 2개: 대상 탭에 붙은 세션에서 `Emulation.setFocusEmulationEnabled { enabled: true }`와 `Page.setWebLifecycleState { state: "active" }`. 이 세션은 agent-browser의 세션과 별개이고, 실험 동안 계속 열어 두었다.

**판정: 조건부.** 플래그도 Chromium 패치도 없이 CDP 호출 두 개로 해결된다. `pegboardd`가 에이전트 탭마다 자기 CDP 세션을 유지하는 구조(녹화용으로 이미 필요하다)와 맞는다.

**한계와 미실시**
- 화면 잠금, 다른 데스크톱 공간: 0단계에서는 **미실시.** 사람이 조작해야 하고, 화면 잠금은 작업 중인 기기를 잠그는 일이라 임의로 하지 않았다. 1단계 수동 확인에서 수행했다(2026-11-13, [phase1-manual-checks.md](./phase1-manual-checks.md)의 M1, M2, M3): 화면 잠금, 전체 화면 Space로 옮긴 창, 디스플레이 절전 모두에서 실패 0, 명령 소요 시간 7~18ms였다.
- 실험 중 사용자가 같은 화면에서 작업하고 있어 창 순서가 흔들렸다. 화면 픽셀로 가림 여부를 검증했고, 검증에 실패한 행은 무효로 처리했다. "가려진 창"의 기본 동작은 `01b`에서 `visibilityState`가 끝까지 `hidden`이었던 것으로 판단했고, 픽셀로는 확인하지 못했다.
- 포커스 에뮬레이션을 켜면 페이지는 항상 보이고 포커스가 있다고 믿는다. 가려진 동안에도 초당 75프레임으로 그린다. 전력 소모, 자동 재생 영상 같은 부작용은 재지 않았다.
- agent-browser가 창을 앞으로 끌어올리는지: `01b`에서 9개 명령을 실행하는 동안 `visibilityState`가 `hidden`으로 유지됐다. 끌어올리지 않는 것으로 보이나, 통제된 화면에서 다시 확인할 가치가 있다.

### A2, A3. 세션 범위 CDP 프록시

스크립트: `spikes/02-proxy.mjs`. 브라우저에 "사람 탭" 2개를 열어 두고, 허용 목록이 빈 프록시를 통해 agent-browser를 붙였다. 모든 명령에서 프록시 로그에 트래픽이 남는 것을 확인했다.

| 단계 | 결과 [확인] |
|---|---|
| `open` | 자기 탭을 새로 만들었다. 사람 탭 2개의 주소는 그대로 |
| `snapshot`, `click`, `fill`, `get url`, `screenshot`, `eval` | 모두 성공 |
| `tab list` | 자기 탭만 보인다. 사람 탭은 보이지 않는다 |
| `tab new` 뒤 `window.open` | 팝업 탭이 `tab list`에 보인다. 프록시가 `openerId`로 허용했다 |
| 틀린 토큰 | `401 Unauthorized`로 실패. Chrome 프로세스 수 변화 없음 (자체 브라우저를 띄우지 않았다) |
| HTTP 탐색 (`/json/version` 등) | 요청 0건 |
| `close` | **CDP 메시지를 하나도 보내지 않았다.** 브라우저와 사람 탭은 살아 있고, 에이전트가 연 탭 3개도 그대로 남았다 |

**판정: A2 맞다, A3 맞다.**

알게 된 것:
- `close`는 탭을 닫지 않는다. 세션 종료 때 탭을 닫는 일은 `pegboardd`가 해야 한다.
- 프록시가 막은 것은 사람 탭에 대한 `Target.targetCreated` 9건이었다. agent-browser는 `Browser.close`를 보내지 않았다.
- agent-browser가 쓴 CDP 메서드는 23종이다. 1단계 프록시 허용목록의 출발점이다.

```
Accessibility.enable, Accessibility.getFullAXTree, Browser.getVersion, DOM.enable,
Input.dispatchMouseEvent, Input.insertText, Network.enable, Page.captureScreenshot,
Page.enable, Page.getFrameTree, Page.navigate, Page.startScreencast, Page.stopScreencast,
Runtime.callFunctionOn, Runtime.enable, Runtime.evaluate, Runtime.runIfWaitingForDebugger,
Target.attachToTarget, Target.createTarget, Target.getTargets, Target.setAutoAttach,
Target.setDiscoverTargets, WebMCP.enable
```

이 목록은 실험한 명령 범위의 것이다. 네트워크 가로채기, 쿠키, 다운로드 같은 명령은 돌리지 않았으므로 더 있다.

**1단계 Task 7에서 덧붙인 메서드** (`crates/pegboard-core/src/proxy/filter.rs`의 `ALLOWED_METHODS`). 실제 세션에서 agent-browser 0.42.0을 돌려 프록시가 거부한 것 가운데, 보낸 CDP 세션의 페이지(또는 그 페이지의 브라우저 컨텍스트)에만 작용하는 것만 넣었다. 판정 근거는 `tests/e2e_runner.rs`의 `compatibility_matrix`.

| 메서드 | 쓰는 명령 | 넣은 이유 |
|---|---|---|
| `Network.setCookies` | `cookies set` | 페이지 세션에서는 그 탭의 프로필 쿠키에만 쓴다. 이미 허용한 `Network.setCookie`의 여러 개 판. 브라우저 수준 호출은 규칙 5가 그대로 막는다. 사람 프로필에 안 새는 것을 `cookies_set_stays_in_the_agent_profile`로 확인 |
| `Page.handleJavaScriptDialog` | `dialog accept`, `dialog dismiss` | 그 페이지에 뜬 대화상자에만 답한다. 막으면 `confirm`이 떠 있는 동안 탭의 다음 명령이 모두 멈춘다 |
| `Page.printToPDF` | `pdf` | 그 페이지를 PDF로 그려 base64로 돌려준다. 스트림(`IO.read`)을 쓰지 않는 것을 확인 |
| `DOM.getFrameOwner` | `frame` 뒤 `fill` 등 | 그 페이지 안 프레임의 소유 요소를 찾는다 |
| `Page.getLayoutMetrics` | `screenshot --full` | 그 페이지의 크기만 읽는다 |
| `Page.createIsolatedWorld` | `a11y` | 그 페이지 프레임 안에 격리된 JS 세계를 만든다 |
| `Page.addScriptToEvaluateOnNewDocument` | `vitals` | 그 페이지의 다음 문서에 스크립트를 건다. `eval`로 이미 할 수 있는 것보다 넓지 않다 |

**1단계 명령 집합** (`crates/pegboard-core/src/argv.rs`의 `GRAMMAR`, 허용목록 문법). 첫 낱말은 아래 명령이어야 하고, 그 형태에 선언된 플래그와 인자만 받는다. 전역 플래그는 `screenshot --annotate` 하나만 받는다. 파일 인자는 경로 없는 이름만 받아 `<PEGBOARD_HOME>/sessions/<세션 id>/files/` 아래로 바꾼다. URL은 프록시와 같은 `normalize_url`로 판정하고 정규화한 문자열을 넘긴다.

`open`, `back`, `forward`, `reload`, `snapshot`, `get url|title|text|html|value|attr|count|box|styles`, `is visible|enabled|checked`, `click`, `dblclick`, `hover`, `focus`, `check`, `uncheck`, `scrollintoview`, `highlight`, `fill`, `type`, `press`, `keyboard type|inserttext`, `select`, `scroll`, `drag`, `mouse move|down|up|wheel`, `find`, `wait`(`--download` 제외), `eval`, `screenshot`, `pdf`, `tab [list|new|close|<id>]`, `frame`, `dialog accept|dismiss|status`, `cookies [get|set]`, `storage local|session ...`, `console`, `errors`, `network requests`, `a11y`, `vitals`, `set viewport`

**1단계에서 지원하지 않는 명령** (README "아직 안 되는 것"에 옮겨 적는다)

| 명령 | 이유 |
|---|---|
| `upload` | `DOM.setFileInputFiles`는 브라우저가 로컬 파일을 읽어 페이지에 넘긴다. 엔드포인트만 가진 쪽이 사용자의 파일을 빼낼 수 있다 |
| `download`, `wait --download` | agent-browser가 `--cdp` 접속에서 스스로 거부하고, 저장 위치를 정하는 메서드는 브라우저 수준이다 |
| `read`, `flow` | CLI가 프록시를 거치지 않고 직접 네트워크에 나가거나 사용자의 저장소를 쓴다 |
| `inspect` | 인증 없는 루프백 DevTools 프록시를 열고 사용자의 기본 브라우저를 띄운다 |
| `diff`, `network har`, `trace`, `profiler`, `cookies set --curl`, `webmcp` | 호출자가 고른 로컬 파일을 읽거나 아무 경로에나 쓴다 |
| `network route`, `clipboard`, `react`, `pushstate` | 1단계 범위 밖. 허용목록에 없는 메서드나 페이지 밖 상태가 필요하다 |
| `cookies clear`, `set media`, `set geo`, `set offline` | 허용하지 않은 메서드(`Network.clearBrowserCookies`, `Emulation.*`, `Network.emulateNetworkConditions`)가 필요하다 |
| `session`, `close`, `connect`, `install`, `upgrade`, `record`, `stream`, `dashboard`, `mcp`, `chat`, `profiles`, `state`, `auth`, `plugin`, `doctor`, `skills`, `confirm`, `deny`, `batch` | `pegboard`가 맡는 것이거나 다른 명령을 감싸 문법 검사를 건너뛴다 |
| `tab <숫자>` | 0.42.0이 숫자 위치를 받지 않는다. `tab t1`처럼 탭 id를 쓴다 |

참고: `tab <id>` 전환은 `Page.bringToFront`를 보내고 거부당하지만 명령은 성공한다. `set viewport`는 `Browser.getWindowForTarget`을 거부당해도 성공한다. 두 메서드는 넣지 않았다.

### A8. 동작 정책과 확인

스크립트: `spikes/02b-policy.mjs`. `--cdp`로 붙은 세션에서 실험했다.

| 정책 | `eval` 결과 [확인] |
|---|---|
| `{"default":"allow","deny":["eval"]}` (문서의 예) | **실행됨. 막지 못했다** |
| `{"default":"allow","deny":["evaluate"]}` | 거부: `Action 'evaluate' denied by policy` |
| `{"default":"deny","allow":["navigate","snapshot"]}` | 거부. `get url`도 `Action 'url'`로 거부 |
| `--confirm-actions eval` | 실행됨. 확인을 요구하지 않았다 |
| `--confirm-actions evaluate` | 결과가 돌아오지 않았다 (확인 대기로 보인다) |

**판정: 조건부.** 정책은 `--cdp` 접속에서도 집행된다. 그러나 0.42.0은 문서의 분류명(`eval`, `get`)이 아니라 내부 동작명(`evaluate`, `url`)으로 매칭한다. 대조 실험으로 `--cdp` 없이도 같은 것을 확인했으므로 접속 방식과 무관하다.

설계 영향: 문서를 믿고 정책 파일을 쓰면 막히지 않는다. 정책 판정은 `pegboardd`가 명령을 넘기기 전에 직접 하고, agent-browser의 정책에는 기대지 않는다.

### A9. 세션별 스트림 서버

| 관찰 [확인] | |
|---|---|
| 기본 상태 | 세션마다 켜져 있다. `stream status`가 포트를 돌려준다 |
| 인증 없는 접속 | 루프백 WebSocket이 열리고 3초 안에 메시지 3개를 받았다 |
| `stream disable` | 성공. 이후 `enabled: false`, `port: null` |
| 시작부터 끄는 환경 변수 | 소스에서 `AGENT_BROWSER_STREAM_PORT`, `_QUALITY`, `_MAX_WIDTH`, `_MAX_HEIGHT`만 찾았다. 끄는 것은 못 찾았다 |

**판정: 조건부.** 끌 수는 있지만 세션의 첫 명령부터 `stream disable`까지는 열려 있다. 같은 기기의 다른 프로세스가 그 사이에 접속해 화면을 보고 입력을 넣을 수 있다.

1단계 Task 7에서 알게 된 것 [확인]: 스트림 서버는 데몬이 뜨자마자, 브라우저에 붙기 전에 열린다. `AGENT_BROWSER_STREAM_PORT`에 `off`, `-1`, `99999`를 줘도 무시되고 임의 포트가 열린다. 그래서 `Runner`는 데몬을 `--cdp` 없이 `stream disable`로 먼저 띄우고, 다음 명령에서야 세션 엔드포인트에 붙인다. 붙은 뒤에도 스트림은 꺼진 채다. 남는 노출은 브라우저가 연결되지 않은 데몬의 포트가 열려 있는 수십 ms(측정 약 60ms 시점에 한 번 관찰, `attach` 전체 약 130~180ms)이고, 그동안 보여 줄 화면도 넣을 입력 대상도 없다. 데몬이 죽었다 다시 뜨는 경우에도 같은 순서를 밟는다(`exec`가 데몬이 살아 있는지 매번 확인한다).

### A4, A5, A7. 탭그룹

스크립트: `spikes/03-tabgroups.mjs`, 확장 프로그램 `spikes/ext-tabgroups/`. 확장의 서비스 워커를 CDP `Runtime.evaluate`로 직접 불렀다.

| # | 실험 | 결과 [확인] |
|---|---|---|
| 1 | 프로토콜에서 탭그룹 찾기 | Chrome 157, 도메인 59개, 관련 항목 0 |
| 2 | `--load-extension`으로 로드 | 성공 (`--disable-features=DisableLoadExtensionCommandLineSwitch`와 함께) |
| 3 | 타깃 ID와 탭 ID 대응 | `chrome.debugger.getTargets()`로 대응됨 |
| 4 | 그룹 생성 | "조사", 파란색 그룹에 탭 2개 |
| 5 | 그룹 안 탭의 `window.open` | **자동으로 같은 그룹에 들어갔다** |
| 6 | CDP로 만든 탭 | 그룹 밖에 생김. 확장으로 53ms 안에 넣음 |
| 7 | 그룹에서 빠지는 것 감지 | `chrome.tabs.onUpdated`의 `groupId` 변화로 감지됨 (드래그 대신 `ungroup` 호출로 확인) |
| 8 | CDP 브라우저 컨텍스트의 탭 | 별도 창. 확장에 보이지 않아 그룹에 넣을 수 없음 |
| 9 | 45초 유휴 뒤 서비스 워커 | **타깃 목록에서 사라졌다. 호출 불가** |

**판정: A4 맞다, A5 조건부, A7 맞다.**

A5의 조건: 서비스 워커가 유휴로 내려가면 CDP로 부를 수 없고, 메모리에 있던 이벤트 기록도 사라진다. 1단계 브리지는 워커가 내려가지 않게 하거나(확장이 `pegboardd`에 연결을 유지), 내려가도 깨울 수 있는 경로(네이티브 메시징, 확장 페이지)가 있어야 한다. 어느 쪽이 되는지는 확인하지 않았다 [미확인].

**1단계 확인** [확인]: `Bridge::attach`가 에이전트 프로필 서비스 워커 타깃에 CDP 세션을 붙여 두면 워커가 내려가지 않는다. 그룹을 만든 뒤 120초 동안 아무 호출도 하지 않았는데 `members` 호출이 그대로 응답했다(`bridge_still_answers_after_two_minutes_idle` 통과). 따라서 워커를 살려 두는 것은 붙어 있는 CDP 세션이고, 확장 쪽 루프백 WebSocket 대체 경로는 필요 없어 만들지 않았다. 세션이 끊긴 경우를 대비해 호출이 '세션 없음'으로 실패하면 워커를 다시 찾아 붙고 한 번 재시도하며, 워커가 목록에 없으면 `bridge_asleep` 오류를 낸다.

### A11. 프로필 구분

스크립트: `spikes/04-profiles.mjs`. `Human` 프로필로 띄운 뒤 `Agent` 프로필 창을 추가로 열었다.

| # | 실험 | 결과 [확인] |
|---|---|---|
| 6 | 두 번째 프로필 창 열기 | 같은 사용자 데이터 디렉터리와 `--profile-directory=Agent`로 실행 파일을 한 번 더 부르면 실행 중인 브라우저에 창이 열린다. 약 0.45초 |
| 1 | 타깃별 `browserContextId` | 프로필마다 다르고, 같은 프로필의 탭끼리는 같다 |
| 2 | `Target.getBrowserContexts` | 목록은 비어 있고 `defaultBrowserContextId`만 나온다 |
| 3 | 프로필의 컨텍스트 ID로 `Target.createTarget` | **실패: `Failed to find browser context with id ...`** Human과 Agent 모두 |
| 3 | 대안: 그 프로필 탭에서 `window.open` | 성공. 같은 프로필, 같은 창에 열림 |
| 4 | Agent 탭의 팝업 | Agent 컨텍스트 유지 |
| 5 | 어느 컨텍스트가 어느 프로필인지 | 그 컨텍스트의 탭에서 `chrome://version`을 열어 프로필 경로를 읽으면 식별됨 |
| 5 | 컨텍스트 없이 `Target.createTarget` | "기본 컨텍스트"에 생긴다. **기본이 실행마다 달랐다** (한 번은 Agent, 한 번은 Human) |

**판정: 조건부.** 구분은 확실하다. 그러나 CDP로 "이 프로필에 탭을 만들어라"를 직접 지시할 수 없다.

**이것이 가장 중요한 발견이다.** agent-browser는 탭을 만들 때 컨텍스트 없이 `Target.createTarget`을 보낸다. 그대로 통과시키면 탭이 사람의 프로필에 생길 수 있다.

### A12. 프로필 사이에 새는 것

| 항목 | 결과 [확인] | 대응 |
|---|---|---|
| 쿠키, `localStorage` | 분리됨. Human에서 설정한 값이 Agent에서 안 보임 | 없음 |
| 사이트 권한 | 분리됨. Human에 준 위치 권한이 Agent에서는 `prompt` | 없음 |
| 페이지 세션의 `Network.getAllCookies` | 그 탭의 프로필 쿠키만 나옴 | 없음 |
| **브라우저 수준 `Storage.getCookies` (컨텍스트 없음)** | **기본 컨텍스트의 쿠키가 나온다. 실험에서는 Human의 쿠키가 나왔다** | 프록시가 반드시 막는다 |
| **`--load-extension`으로 올린 확장** | **두 프로필 모두에 로드된다** | 1단계 브리지 확장이 사람 프로필에도 올라간다. 확장은 에이전트 프로필의 탭만 건드리게 만들고, 3단계에서 내장 브리지로 바꾸며 없앤다 |
| 브라우저 환경설정 | 0단계 미실시. 1단계 M4(2026-11-13): 홈 버튼 표시, 기본 검색엔진, 다운로드 위치 세 가지는 프로필별로 나뉜다(사람 프로필에서 바꿔도 에이전트 프로필은 그대로) | 없음 |
| **다운로드 경로** | 0단계 미실시. 1단계 M5(2026-11-13): **에이전트 세션이 받은 파일이 macOS 사용자의 `~/Downloads`에 생긴다.** 에이전트 프로필에 다운로드 위치가 지정되어 있지 않다 | 세션별 디렉터리로 보내는 구현이 없다. 2단계에서 정한다 |

### A13. 프로필 복사

| # | 실험 | 결과 [확인] |
|---|---|---|
| 13 | 쿠키가 디스크에 쓰이기까지 | **30초.** 파일은 `Agent/Network/Cookies` |
| 14 | 브라우저를 켠 채 복사 (캐시 제외) | 58ms, 2.8MB |
| 15 | 복사본으로 창 열기 | 0.38초. 새 컨텍스트이고 쿠키와 `localStorage`가 들어 있다 |
| 16 | `Local State` 등록 | 창을 연 직후에는 목록에 없었다. 동작에는 지장이 없었다 |
| 17 | 복사본에서 쿠키 변경 | 원본은 그대로 |
| 18 | 탭을 닫고 디렉터리 삭제 | 성공. 브라우저 정상 |
| 19 | 원본이 100ms마다 쓰는 중에 5회 반복 | 5회 모두 성공 |

**판정: 맞다.** 주의: 쿠키가 디스크에 반영되는 데 30초가 걸렸다. 로그인 직후 복사하면 그 로그인이 복사본에 없을 수 있다.

한계: 실험 프로필은 2.8MB짜리 빈 프로필이다. 실제로 쓰던 큰 프로필(IndexedDB, 서비스 워커, 확장 프로그램 자료)에서의 일관성은 확인하지 않았다. 쿠키가 평문에 가까운 실험용 값이라 OS 키 저장소에 묶인 암호화가 복사본에서 풀리는지도 실제 사이트로는 확인하지 않았다.

### A6. 브라우저가 코어에 파이프 CDP를 넘기기

Extended Stable 태그 `156.0.8078.96`의 소스를 읽었다 [확인]. 빌드해서 검증한 것은 아니다.

- `content/browser/devtools/devtools_pipe_handler.{h,cc}`: `DevToolsPipeHandler(int read_fd, int write_fd, base::OnceClosure on_disconnect)`. 생성자가 임의의 fd 쌍을 받는다. 브라우저 타깃을 만들어 붙고, 읽기와 쓰기 스레드를 따로 돌린다. 전송 형식은 NUL로 끊는 JSON 또는 CBOR.
- `content/browser/devtools/devtools_agent_host_impl.cc`: 공개 함수 `DevToolsAgentHost::StartRemoteDebuggingPipeHandler(on_disconnect)`가 fd를 3과 4로 고정해 위 생성자를 부른다. 핸들러는 프로세스에 하나만 둘 수 있는 구조다(정적 변수 하나).
- `chrome/browser/devtools/remote_debugging_server.cc`: `--remote-debugging-pipe`가 있을 때만 시작한다. 그 전에 원격 디버깅 허용 설정과 기본 사용자 데이터 디렉터리 여부를 검사한다.

**판정: 맞다.** 필요한 수정은 이 정도로 보인다.
1. fd를 인자로 받는 공개 함수 하나 추가 (`devtools_agent_host.h`, `devtools_agent_host_impl.cc`, 십여 줄).
2. 새 파일(`//pegboard/`)에서 소켓 쌍을 만들고 `pegboardd`를 띄우며 한쪽을 넘기고, 다른 쪽으로 위 함수를 부른다.
3. 시작 지점에 호출 한 줄.

주의: 핸들러가 하나뿐이라 `--remote-debugging-pipe`와 동시에 쓸 수 없다. 우리 브라우저는 그 스위치를 쓰지 않으므로 문제없다. 원격 디버깅 허용 검사를 우회하게 되므로, 넘기는 상대가 우리가 띄운 `pegboardd`뿐임을 패치에서 보장해야 한다.

패치 없이 가는 대안: 앱의 실행 파일을 작은 런처로 두고, 런처가 브라우저를 `--remote-debugging-pipe`로 띄우는 부모가 된다. 1단계의 "데몬이 부모" 구성과 같다. 앱 번들 구조와 업데이트, 독(Dock) 동작에 미치는 영향은 확인하지 않았다 [미확인].

**`Pegboard` 도메인을 둘 자리** [확인: 디렉터리 목록]: `chrome/browser/devtools/protocol/`에 브라우저 쪽 핸들러가 모여 있다(`browser_handler`, `target_handler`, `extensions_handler`, `window_manager_handler`, `webmcp_handler` 등). 여기에 핸들러 파일 한 쌍을 추가하는 형태가 된다. 프로토콜 정의 파일과 등록 지점은 읽지 않았다 [미확인]. 탭그룹과 프로필의 내부 API 위치도 아직 찾지 않았다.

### A10. Chromium 빌드

Extended Stable 태그 `156.0.8078.96`을 받아 한 번 빌드했다. 스크립트는 `spikes/06-build.sh`, 소스는 저장소 밖 `~/chromium`, 원자료는 `spikes/out/06-build-log.txt`.

실측 [확인]:

| 단계 | 시간 | 디스크 |
|---|---|---|
| depot_tools 받기 | 4초 | |
| 소스 받기 (`fetch --nohooks --no-history`) | 1,161초 (19분) | 약 37GB |
| 태그 받기와 전환 | 약 4분 반 | |
| 의존성 동기화 (`gclient sync -j 3`) | 545초 (9분) | 소스 합계 52GB |
| `gn gen` | 7초 | 대상 36,370개 |
| **전체 빌드** (`autoninja chrome`) | **10,004초 (2시간 47분)** | 산출물 9.1GB, 앱 699MB |
| **증분 빌드** (브랜딩 파일 한 줄 수정) | **89초** | |
| 합계 (빈 상태에서 실행 가능한 브라우저까지) | 약 3시간 20분 | 약 61GB |

- 빌드 설정: `is_debug=false symbol_level=0 is_component_build=false target_cpu="arm64"`.
- 기기: Apple Silicon 15코어, 메모리 48GB. 전체 빌드는 `nice 10`으로 돌렸고 사용자가 같은 기기에서 작업 중이었다(부하 평균 75~176). 전용 장비라면 더 빠를 것이다.
- 결과물 실행 확인: 빌드한 `Chromium.app`이 뜨고 CDP로 `Chrome/156.0.8078.96`을 돌려주었다.
- 패치 흐름 확인: 수정을 `git diff`로 `spikes/branding-test.patch`에 내보내고, 원래대로 되돌린 뒤 `git apply --3way --check`가 통과했다.

**판정: 맞다.** 계획서의 기준(전체 8시간 이내, 증분 30분 이내)을 충족한다.

주의할 점:

- **증분 89초는 리소스 파일 한 줄을 고친 경우다.** 여러 파일이 포함하는 C++ 헤더를 고치면 훨씬 길다. 재지 않았다.
- **빌드 중에는 기기를 쓰기 어렵다.** 부하 평균이 100을 넘었다. 일상 작업 기기와 빌드 기기를 나누거나 밤에 돌려야 한다.
- **`PRODUCT_FULLNAME` 한 줄만 바꿔서는 앱 이름이 바뀌지 않았다.** `Info.plist`의 표시 이름은 그대로 `Chromium`이었다. 브랜딩 패치는 이름 여러 개와 번들 ID, 아이콘을 함께 바꿔야 한다.
- 릴리스마다 전체를 다시 빌드할 필요는 없다. 버전을 올리면 바뀐 파일만 다시 컴파일되지만, 주 버전이 바뀌면 사실상 전체 빌드에 가깝다 [미확인].

걸림돌 두 가지 [확인]:

1. **익명 요청 제한.** `gclient sync`가 기본 동시 요청 수로 돌다가 `chromium.googlesource.com`에서 HTTP 429(`Short term server-time rate limit exceeded`, `shared_anonymous`)를 받고 241초 만에 실패했다. `-j 3`으로 줄여 다시 돌리니 통과했다. CI에서도 같은 일이 생기므로 재시도와 속도 제한, 또는 깃 캐시가 필요하다.
2. **Xcode Metal 도구 모음.** 빌드가 28초 만에 `cannot execute tool 'metal' due to missing Metal Toolchain`으로 멈췄다. `xcodebuild -downloadComponent MetalToolchain`(839MB)으로 해결했다. 빌드 장비 준비 항목이다. 이 구성 요소는 이 기기의 Xcode에 설치된 채로 남아 있다.

공식 문서에서 확인한 것 [확인: `docs/mac_build_instructions.md`]: 요구 사항은 Xcode와 macOS SDK, APFS 볼륨. 외부 기여자는 맥 빌드에 원격 실행을 쓸 수 없다. 빌드를 빠르게 하는 설정으로 `is_component_build=true`(증분 빌드가 빨라짐), `symbol_level=0`, `use_lld=false`, ccache를 든다.

채널 확인 [확인: chromiumdash 조회]: Extended Stable 채널이 실제로 있고(`156.0.8078.96`) 같은 시점의 Stable(`157.0.8103.51`)보다 주 버전 하나 뒤다. 릴리스 주기는 확인하지 않았다.

**3단계 계획에 주는 것**
- CI 한 번에 최소 3시간, 디스크 60GB 이상을 잡는다. 캐시가 없으면 매번 그만큼 든다.
- 소스와 빌드 산출물을 유지하는 전용 빌드 장비(또는 캐시가 붙은 자체 러너)를 둔다. 그러면 보안 수정 릴리스 반영은 증분 빌드에 가까워진다.
- 개발 중에는 `is_component_build=true`로 빌드하고, 배포 빌드만 단일 실행 파일로 한다.

**브라우저 쪽 API 위치** [확인: 태그 `156.0.8078.96` 소스]. `Pegboard` 도메인이 부를 것들이다.

| 기능 | 위치 |
|---|---|
| 탭그룹 생성과 소속 변경 | `chrome/browser/ui/tabs/tab_strip_model.h`의 `AddToNewGroup`, `AddToExistingGroup`, `RemoveFromGroup` |
| 프로필 생성 | `chrome/browser/profiles/profile_manager.h`의 `CreateProfileAsync`, `CreateMultiProfileAsync` |
| 프로필 창 열기 | `chrome/browser/profiles/profile_window.h`의 `OpenBrowserWindowForProfile`, `FindOrCreateNewWindowForProfile` |
| 프로필 삭제 | `chrome/browser/profiles/delete_profile_helper.h`의 `MaybeScheduleProfileForDeletion` |

새 CDP 도메인을 추가할 때 건드릴 업스트림 파일은, 기존 `WindowManager` 도메인이 등록된 곳을 기준으로 `chrome/browser/devtools/inspector_protocol_config.json`, `chrome/browser/devtools/chrome_devtools_session.cc`, `chrome/browser/devtools/BUILD.gn`, 그리고 프로토콜 정의 파일이다. 정의 파일의 위치는 찾지 못했다 [미확인]. 핸들러 자체는 새 파일이다.

## 미실시

| 항목 | 이유 | 언제 |
|---|---|---|
| 화면 잠금 상태에서의 자동화 (A1) | 작업 중인 기기를 잠가야 한다 | **수행 2026-11-13 (1단계 M1).** 잠긴 동안의 11회 반복 포함 실패 0, 7~18ms. 디스플레이 절전(M3)도 같다 |
| 다른 데스크톱 공간에 있는 창 (A1) | 사람이 조작해야 한다 | **수행 2026-11-13 (1단계 M2).** 에이전트 창을 전체 화면 Space로 옮긴 상태에서 실패 0, 7~17ms. 창 모드로 두 번째 일반 데스크탑에 둔 경우는 하지 않았다 |
| 포커스 에뮬레이션의 전력 소모와 부작용 (A1) | 범위 밖 | 2단계 |
| 브라우저 환경설정, 다운로드 경로의 프로필 간 누출 (A12) | 사람이 조작해야 한다 | **수행 2026-11-13 (1단계 M4, M5).** 환경설정 세 가지는 새지 않았다. 다운로드는 기대와 달랐다: 에이전트가 받은 파일이 사람이 쓰는 `~/Downloads`에 생긴다 |
| 실제로 쓰던 큰 프로필의 복사 (A13) | 실험은 빈 프로필 | 2단계 |
| 서비스 워커를 깨워 두는 방법 (A5) | 문제만 확인 | 1단계 첫 작업 |
| C++ 헤더 수정 시 증분 빌드 시간 (A10) | 리소스 한 줄만 측정 | 3단계 |
| 파이프 CDP 패치의 실제 구현 (A6) | 소스만 읽음 | 3단계 |

실험 환경의 한계: 사용자가 같은 화면과 기기에서 작업하는 중에 실험했다. 창 순서가 흔들려 A1의 일부 행을 무효 처리했고, 빌드 시간은 다른 작업과 CPU를 나눠 쓴 값이다.

## 설계 변경 제안

사용자 승인을 받아 architecture.md와 roadmap.md에 반영했다(2026-11-10). 반영 위치는 각 항목 끝에 적었다.

1. **탭 생성을 프록시가 가로챈다 (A11).** agent-browser의 `Target.createTarget`을 브라우저에 그대로 넘기지 않는다. 프록시가 브리지를 통해 에이전트 프로필에 탭을 만들고(1단계: 에이전트 프로필의 확장이 `chrome.tabs.create`, 3단계: `Pegboard` 도메인), 그 타깃 ID를 응답으로 돌려준다. 만든 뒤 `browserContextId`가 세션의 프로필인지 확인하고, 아니면 닫고 오류를 낸다. → 결정 2
2. **컨텍스트 없는 브라우저 수준 메서드는 기본 거부 (A11, A12).** `Storage.getCookies`, `Storage.setCookies`, `Browser.grantPermissions` 등. "기본 컨텍스트"가 어느 프로필인지 실행마다 달랐으므로, 통과시키는 순간 사람 프로필에 닿을 수 있다. → 결정 2, 11절
3. **가려진 탭 대응은 CDP 호출 두 개로 한다 (A1).** `pegboardd`가 세션 탭마다 유지하는 CDP 세션에서 포커스 에뮬레이션과 수명 상태를 설정한다. 5절 패치 목록의 "백그라운드 렌더링 유지" 패치는 뺀다. → 결정 2, 5절
4. **정책 판정은 `pegboardd`가 한다 (A8).** agent-browser의 `--action-policy`와 `--confirm-actions`에는 기대지 않는다. 11절의 "쓸 수 있으면 그것을 쓰고"를 지운다. → 11절
5. **세션을 만들면 곧바로 `stream disable`을 실행한다 (A9).** 남는 노출 구간은 11절 "남는 위험"에 적는다. 업스트림에 시작 시 비활성화 옵션을 제안할 만하다. → 6절, 11절
6. **세션 종료 때 탭은 `pegboardd`가 닫는다 (A2).** agent-browser의 `close`는 탭을 남긴다. → 6절
7. **1단계 브리지는 서비스 워커 수명을 다룬다 (A5).** 확장이 `pegboardd`에 연결을 유지하는 방식을 1단계 첫 작업에서 검증한다. → 결정 3, 로드맵 1단계
8. **프로필 복사 전 쿠키 반영을 기다린다 (A13).** 복사 직전 30초 안에 바뀐 쿠키는 CDP로 읽어 복사본에 넣는 보완을 검토한다. → 6절 복사 방식
9. **에이전트 프로필 창은 실행 파일을 다시 불러 연다 (A11).** `--profile-directory=<이름>`. 3단계에서는 `Pegboard` 도메인으로 바꾼다. → 6절 에이전트 프로필

## 2a단계 측정

2a단계 작업 4(주소 규칙), 5(다운로드), 7(클립보드)이 고를 방법을 정하려고 Chrome for Testing 157.0.8103.78에서 쟀다(2026-11-13). 스크립트는 `spikes/20-request-guard.mjs`, `21-downloads.mjs`, `22-clipboard.mjs`이고 각각 두 번 이상 돌렸다. 아래 줄은 마지막 실행의 출력이다. 두 번 사이에 달라진 것은 밀리초 수치와, 같은 밀리초에 찍힌 사건의 선후 판정 한 건(G4 조합 11)뿐이다. 전체 출력은 `docs/plans/phase2a-notes/task-2-report.md`에 있다. 표기는 위와 같다: [확인]은 직접 관찰한 것.

공통 설정: 임시 user-data-dir에 프로필 둘(`Human`, `Agent`)을 1단계 `04-profiles.mjs`와 같은 방식으로 연다. **21과 22의 모든 실행에서 기본 브라우저 컨텍스트는 `Agent`였다**(20은 확인하지 않았다) [확인]. 그리고 `Browser.setDownloadBehavior`와 `Browser.setPermission`에 `Human`의 컨텍스트 ID를 주면 `Failed to find browser context for id ...`로 거절됐고 `Agent`의 ID는 받아들여졌다(5절의 "남는 질문" 참고). 용어: P는 `pegboardd` 역을 하는 브라우저 연결, A는 에이전트 역을 하는 연결이다. 프록시를 거치지 않은 Chrome의 동작이다. 프록시 필터에 관한 말은 `filter.rs`를 읽은 것이지 실행한 것이 아니다.

### G. 요청 가로채기 (`20-request-guard.mjs`)

**G1. 어떤 요청이 `Fetch.requestPaused`로 잡히는가** [확인]. 세 가지 걸기 방식으로 같은 페이지(`/g1`)를 열었다. A: P가 에이전트 탭의 페이지 세션에만 `Fetch.enable`. B: 거기에 페이지 수준 `Target.setAutoAttach`(`waitForDebuggerOnStart`)를 더하고 새로 붙은 타깃마다 `Fetch.enable`. C: 브라우저 수준 `Target.setAutoAttach`.

| 요청 | A 페이지 세션만 | B +페이지 자동 연결 | C +브라우저 자동 연결 |
|---|---|---|---|
| 주 프레임 탐색 (`Page.navigate`) | 잡힘 | 잡힘 | 잡힘 |
| iframe, 이미지 | 잡힘 | 잡힘 | 잡힘 |
| 페이지 스크립트의 `fetch`, `XMLHttpRequest`, `sendBeacon` | 잡힘 | 잡힘 | 잡힘 |
| `Runtime.evaluate`로 낸 `fetch` | **잡힘** | 잡힘 | 잡힘 |
| 전용 워커의 스크립트와 그 워커의 `fetch` | 잡힘(페이지 세션이 본다. 워커 타깃에는 `Fetch.enable`이 없다: `'Fetch.enable' wasn't found`) | 잡힘 | 잡힘 |
| 공유 워커의 스크립트 | 잡힘 | 잡힘 | 잡힘 |
| 공유 워커가 낸 `fetch` | **못 잡음** | **못 잡음** | 잡힘(`shared_worker` 세션) |
| 서비스 워커의 스크립트 등록 요청 | 못 잡음 | 잡힘(`service_worker` 세션) | 못 잡음 |
| 서비스 워커가 낸 `fetch` | **못 잡음** | 잡힘(`service_worker` 세션) | 잡힘 |
| 웹소켓 핸드셰이크 | **못 잡음** | **못 잡음** | **못 잡음** |

- `G1: evalFetch and pageFetch caught with page-session Fetch: PASS; not caught in phase A (page session only): websocket,sharedWorkerFetch,serviceWorkerScript,serviceWorkerFetch`
- `G1.websocket: handshake never reaches Fetch.requestPaused (phases A, B, C); Network.webSocketCreated at P with Network.enable: ["ws://127.0.0.1:PORT/t/ws?p=A"] (observation only; blocking it was not measured)` 웹소켓은 서버에 도달했고(핸드셰이크 도착), 가로채기는 URL을 알려 주는 `Network.webSocketCreated` 이벤트를 받았을 뿐이다. 막을 수 있는지는 재지 않았다.
- 서비스 워커가 `respondWith`로 처리하는 요청은 **페이지 세션에서 안 보인다** (G4.serviceWorker 참고).

**G2. 요청 단계와 응답 단계에서 보이는 것** [확인].
- `G2.request`: 요청 단계 이벤트의 필드는 `requestId, request{url, method, headers, initialPriority, referrerPolicy}, frameId, resourceType, networkId`. **원격 주소 필드는 없다.** 이름(URL)만 보인다.
- `G2.response`: 응답 단계(`requestStage: Response`) 이벤트는 거기에 `responseStatusCode, responseStatusText, responseHeaders`가 붙는다. `{"remoteIpAtPause":null,"remoteIpAfter500ms":null,"responseReceivedBeforePauseEvent":false}`: 붙들고 있는 동안 `Network.responseReceived`(원격 IP가 들어 있는 이벤트)는 **오지 않았다**(500ms 기다림).
- 풀어 주면 `Network.responseReceived`가 1~2ms 뒤에 오고, 페이지가 응답 헤더를 받는 것과 같은 때다(`ripAfterContinue.receivedByP_msAfterContinue` 1~2, `pageGotHeadersAt_msAfterContinue` 1~2). 즉 IP를 알았을 때는 이미 페이지가 응답을 받은 뒤다.
- `Fetch.getResponseBody`는 붙들고 있는 동안 된다(`ok:16chars`). 본문은 읽히지만 원격 IP는 알 수 없다.
- `G2.redirect: {"requestStageOnly":["/t/redirect","/t/body<-redirect"],"bothStages":["Request::/t/redirect","Response:302:/t/redirect","Request::/t/body:redirected","Response:200:/t/body:redirected"]}`: 리다이렉트의 각 단계가 새 `requestPaused`로 온다. 이동한 곳의 요청에는 `redirectedRequestId`가 붙는다.
- `G2.cut: PASS body not delivered (ERR:Failed to fetch); server was contacted: true`: 응답 단계에서 `Fetch.failRequest`(`BlockedByClient`)를 하면 페이지는 본문을 받지 못하고 `Failed to fetch`가 된다. 그러나 서버는 요청을 받았다(부작용은 일어난다).

**G3. 공개 이름이 사설 주소로 풀릴 때** [확인]. `--host-resolver-rules`로 한 번의 실행에 세 이름을 매핑했다(계획은 따로 실행하라고 했으나 같은 결과를 한 번에 얻었다): `loop.test`→`127.0.0.1`, `lan.test`→이 기기의 사설 LAN 주소(서버가 거기서도 듣게 했다), `ten.test`→`10.255.255.1`(라우팅 안 됨).
- 요청 단계: 세 이름 모두 호스트 이름만 보인다. 요청 단계에서 400ms 붙들어 두는 동안 서버는 연락받지 않았다(`serverContactedBeforeRequestStageContinue:false`).
- 응답 단계: `loop`, `lan` 모두 붙들고 있는 동안 원격 IP를 알 수 없었다(`ripAtResponsePause:null`, `ripWithin600ms:null`). 풀어 준 뒤에야 `127.0.0.1`과 사설 LAN 주소가 `Network.responseReceived`로 왔고, 그때 본문은 이미 페이지에 갔다(`bodyReachedPage:true`).
- `ten.test`: 요청 단계는 보이나 응답이 오지 않았다. `Network.loadingFailed`도 9초 동안 없었고 페이지의 `fetch`는 9005ms에 시간 초과(`ERR:timeout`), 서버 접촉 0.
- 결론: **응답 도착 때의 실제 원격 주소로 본문 전에 끊는다는 설계 5.3의 방법은 쓸 수 없다.** 요청 단계의 이름 판정만 가능하다. 이름을 `pegboardd`가 따로 풀어 보는 방법 등은 재지 않았다.

**G4. 에이전트 쪽 연결이 가로채기를 끄거나 가릴 수 있는가** [확인].
- `G4.switchOff: A's Fetch.disable=ok; P still caught after: Fetch.disable=true, Network.setBypassServiceWorker=true, Network.disable=true, Target.setAutoAttach(off)=true => PASS`: A의 `Fetch.disable`, `Network.setBypassServiceWorker`, `Network.disable`, `Target.setAutoAttach(끄기)`는 P의 가로채기를 끄지 못한다(Fetch 상태는 세션마다 따로다).
- `G4.steal`: A가 P가 붙든 요청의 `requestId`로 `Fetch.continueRequest`/`failRequest`/`fulfillRequest`를 보내면 `Invalid InterceptionId.`로 거절된다. 자기가 `Fetch.enable`을 했을 때도 같다(앞서 안 했을 때는 `Fetch domain is not enabled`).
- **두 세션 모두 `Fetch.enable`을 했을 때는 "가장 새로 만든 세션"이 먼저 판정한다**(`Fetch.enable`을 먼저 했는지가 아니라 세션을 만든 순서). 조합 3(P는 실패, A는 응답 만들기)에서 A의 세션이 P보다 먼저 만들어졌으면 P가 먼저 보고 막는다. 조합 9: P보다 **나중에 만든 A의 새 세션**이 `fulfillRequest`하면 P는 그 요청을 보지도 못하고 페이지는 `OK:FROM-A2`를 받는다(`P saw=false A saw=true`). 즉 새 세션에서 `Fetch.enable`을 할 수 있는 클라이언트는 P의 차단을 우회한다. 한쪽이 응답을 만들거나 실패시키면 뒤의 세션은 그 요청을 보지 못한다.
- 프록시 필터(`crates/pegboard-core/src/proxy/filter.rs`, 읽었을 뿐 실행하지 않음)는 허용 목록 방식이고 `Fetch.*`와 `Network.setBypassServiceWorker`는 목록에 없다. 브라우저 수준 `Fetch.`/`Network.`/`Storage.`는 따로 거절한다. 그래서 위 우회는 필터가 막는다. `Target.setAutoAttach`(페이지 수준)와 `Target.attachToTarget`은 허용 목록에 있다.
- `G4.serviceWorker: {"pageControlledBySW":true,"pageFetch_caughtByP_pageSessionOnly":[],"pageFetch_caughtByP_whenAbypassesSW":["page:XHR"],"caughtByP_whenPAlsoAttachedToSwTarget":["service_worker:XHR:sw=1"],"serverHitsInOrder_plain_bypass_withSw":["SW","page","SW"]}`: 서비스 워커가 `respondWith(fetch(...))`로 처리하는 페이지의 요청은 P의 페이지 세션에서 **보이지 않고**, 서비스 워커가 낸 요청은 P가 그 타깃에도 붙어 `Fetch.enable`을 해야 보인다. `Network.setBypassServiceWorker`는 A가 켜도 P가 덜 보게 만들지는 않는다(켜면 페이지 요청이 P에게 보인다). 서비스 워커는 에이전트 프로필에 속한다(`browserContextId`가 `Agent`).
- `G4.workerRace`: 전용 워커의 요청은 페이지 세션을 거쳐 P에게 보이므로(G1), A가 워커를 먼저 재개해도 숨기지 못한다. 서비스 워커와 공유 워커에 대한 같은 경주는 재지 않았다.

**G5. 가로채기의 수명과 범위** [확인].
- `G5.closeReleases`: P의 연결을 닫으면 붙들려 있던 요청은 **서버로 나가 정상 응답을 받았고**(`OK:not found`는 서버의 404 본문), 그 뒤 요청도 가로채기 없이 나간다. 연결이 끊기면 열려 버린다(fail-open).
- 탭마다 걸어야 한다. 에이전트 탭에서 `window.open`으로 연 탭의 첫 요청은 가로채지 못했다(자동 연결을 안 건 경우, 페이지 수준 자동 연결을 건 경우 모두. 페이지 수준 자동 연결은 팝업을 붙여 주지 않는다: 붙은 것은 워커와 서비스 워커뿐).
- **브라우저 수준 `Target.setAutoAttach`(`waitForDebuggerOnStart: true`)를 걸면** 팝업이 `page/Agent/wait`로 붙고, `Fetch.enable` 뒤 `Runtime.runIfWaitingForDebugger`로 재개하면 팝업의 첫 요청과 하위 요청이 모두 잡혔다(`G5.popupBrowserAutoAttach: {"firstRequestCaught":true,"subresourceCaught":true,...}`). 같은 방식으로 **사람 프로필의 새 탭도** `page/Human/wait`로 붙어 재개될 때까지 멈춘다.
- 브라우저가 URL을 줘서 새 창을 여는 경우(`--profile-directory=Agent --new-window <URL>`)는 브라우저 수준 자동 연결로 붙어도 첫 요청을 못 잡았다. `Target.setDiscoverTargets`로 알아채고 뒤늦게 붙는 방법도 못 잡았다(`G5.newWindow: discoverThenAttach firstRequestCaught:false, browserAutoAttachWait firstRequestCaught:false`). 이미 붙은 탭에서 `Page.navigate`하는 요청은 잡힌다(G1 `nav`).

**G6. 지연** [확인]. 같은 탭에서 `fetch` 100번(순차, 로컬 서버): `G6: mean per request (100 sequential fetches): baseline 0.39ms (runs 0.49/0.29); request-stage 0.54ms (+0.15); request+response stage 0.56ms (+0.17)`. 요청 하나에 더하는 지연은 0.2ms 안팎이다(처리기가 즉시 풀어 줄 때).

**작업 4가 쓸 방법.** P가 에이전트 컨텍스트의 모든 페이지 타깃에 요청 단계 `Fetch.enable`을 건다. 새 탭, 팝업, 서비스 워커, 공유 워커를 놓치지 않도록 브라우저 수준 `Target.setAutoAttach`(`waitForDebuggerOnStart`)로 붙고, 에이전트 컨텍스트가 아닌 타깃(사람 프로필)은 바로 재개한다. 새 탭은 `about:blank`로 만들어 가로채기를 건 뒤 `pegboardd`가 이동시킨다(URL을 주고 여는 경로는 첫 요청이 안 잡힌다). 판정은 URL의 이름과 리터럴 IP로만 하고, 이름이 사설 주소로 풀리는 경우는 응답 단계로는 막을 수 없으므로 "남는 위험"에 적거나 `pegboardd`가 이름을 따로 풀어 보는 방법을 따로 실험한다(재지 않았다). 웹소켓은 `Fetch`로 막을 수 없다. P의 연결이 끊기면 가로채기가 풀리므로 그동안 세션 명령을 받지 않는다(fail-closed). 프록시 허용 목록에 `Fetch.*`가 없는 것을 시험으로 고정한다.

### L. 다운로드 (`21-downloads.mjs`)

모든 파일은 임시 폴더로만 받았다. 정책이 없는 사람 프로필의 다운로드와 정책 연결이 사라진 뒤의 다운로드는 사용자의 `~/Downloads`에 떨어진다(L1b, L5가 재는 것이다). 그 파일은 실행마다 새로 만든 무작위 ID가 이름에 들어 있었고 스크립트가 끝에 그 ID가 든 파일만 지웠다.

**L1. 프로필마다 다른 폴더를 가질 수 있는가** [확인].
- `L1.perProfile: PASS: ... D1 set Agent dir by browserContextId (ok) ...; agent download landed: {"agentDir":true,...}; human download landed: {"userDownloadsByName":true}`: 에이전트 컨텍스트 ID로 건 정책(`allowAndName`, `eventsEnabled:true`)은 에이전트 프로필의 다운로드를 그 폴더로 보냈고 사람 프로필에는 번지지 않았다(사람 쪽 다운로드는 정책이 없어 `~/Downloads`로 갔다).
- **사람 프로필의 컨텍스트 ID는 거절된다**: `Browser.setDownloadBehavior: Failed to find browser context for id <ID>`. 컨텍스트 ID 없이 걸면 기본 컨텍스트에 걸리는데 이 측정에서는 기본이 `Agent`였으므로 사람 프로필에는 정책을 걸 수 없었다(걸지 않는 것이 2a의 요구이므로 문제는 아니다).
- 이벤트(`Browser.downloadWillBegin`, `downloadProgress`)는 `eventsEnabled`로 건 연결에 오고, **컨텍스트와 상관없이 브라우저 전체의 다운로드가 온다**: 에이전트 컨텍스트에 정책을 건 연결이 사람 프로필의 다운로드 시작도 받았다. 걸러내는 일은 `pegboardd`가 해야 한다.
- `L1b` (같은 컨텍스트에 연결 둘): 나중에 건 연결의 폴더가 이긴다. **나중 연결을 닫으면 먼저 건 연결의 정책이 되살아나지 않는다**: 이후 에이전트 다운로드는 `~/Downloads`로 갔다. 연결 하나가 두 컨텍스트를 쥐는 것은 사람 쪽이 거절되어 재지 못했다.

**L2. 다운로드를 탭으로 찾을 수 있는가** [확인]. `Browser.downloadWillBegin`의 `frameId`로 찾는다. 주 프레임의 `frameId`는 그 탭의 `targetId`와 같다. 아래는 모든 시나리오가 에이전트 탭으로 풀렸다는 뜻이다.

| 시나리오 | 찾은 방법 |
|---|---|
| 링크 클릭 (`a.click()`) | `frameId == targetId`(page, Agent). 프레임 트리로도 같은 탭 |
| 주소로 직접 이동(`Page.navigate`), `blob:` 다운로드 | 같다 |
| `window.open`으로 연 탭(바로 닫힘) | `frameId == targetId`인 타깃이 이미 사라졌어도 `targetCreated`로 만들어 둔 표에서 찾는다. `openerId`가 에이전트 탭을 가리킨다. 사라진 뒤에는 프레임 트리로는 못 찾는다 |
| 같은 사이트 iframe의 링크(실제 마우스 클릭) | `targetId`로는 못 찾고 **그 탭의 `Page.getFrameTree`에서** 자식 프레임으로 찾는다 |
| 다른 사이트 iframe(OOPIF) | `frameId == targetId`인 `iframe` 타깃(Agent). 부모 탭은 `frameId`만으로는 알 수 없다(그 탭의 프레임 트리에서도 안 나왔다) |
| 서비스 워커가 `client.navigate`로 시킨 다운로드, 서비스 워커가 만든 `Content-Disposition` 응답 | 해당 클라이언트 탭의 `frameId` |
| 사람 프로필 탭의 다운로드(`JS click`) | `frameId == targetId`(page, **Human**) |

- `L2: link click resolves to an Agent tab: PASS; all agent scenarios resolved: ...=true ...; human tab, synthetic mouse click (hidden window): NO downloadWillBegin; human tab, JS click: resolves to Human: true (a real person's click: not measured)`
- 사람이 그 창에서 직접 누른 다운로드는 재지 못했다. 가려진 사람 창에 `Input.dispatchMouseEvent`로 낸 클릭은 12초를 기다려도 다운로드를 시작하지 못했다. 실제 사람의 클릭은 사람이 해야 한다.
- 다른 사이트 iframe(OOPIF)의 부모 탭을 `frameId`만으로 찾지 못한다는 점은 작업 5가 "어느 세션 것인지 알 수 없으면 지운다"로 처리해야 한다. 이 경우를 "탭을 못 찾음"으로 막을지 `iframe` 타깃의 `Target.getTargetInfo`나 연결 정보를 더 볼지는 재지 않았다.

**L3. 취소와 한도** [확인].
- `Browser.cancelDownload {guid, browserContextId}`가 된다(41~51ms에 응답, 서버는 연결이 끊긴 것을 봄, 상태 `canceled`). 취소 뒤 정책 폴더에 부분 파일은 남지 않았다(`leftover files []`). 진행 중에는 정책 폴더에 `guid` 이름의 부분 파일이 있고 커진다(L5).
- `L3.withContentLength: ... totalBytes first reported 30000000 ...`: `Content-Length`가 있으면 첫 `downloadProgress`부터 `totalBytes`를 알려 준다(0바이트 시점에 선언된 크기가 한도를 넘는 것을 알 수 있다). 없으면(`nolen`) `totalBytes`는 0이다.
- **진행 이벤트는 약 500ms에 한 번 온다**(`ms between events [2,514,4]`, `received series [0,0,6553600,0]`). 한도(예: 2 MiB)를 넘은 것을 본 첫 이벤트의 `receivedBytes`는 약 6.3~6.5 MiB였고(서버가 초당 약 13MB를 보냄), 서버는 취소가 걸리기까지 6.5 MiB를 보냈다. 한도를 넘는 양은 전송 속도에 500ms를 곱한 만큼이다. `canceled` 이벤트의 `receivedBytes`는 0이다.

**L4. 저장 파일 이름** [확인]. `allowAndName`이면 정책 폴더에 `guid`(대문자 UUID 모양) 이름으로 저장된다(`file on disk is named by the download guid: true`). 원래 이름은 `Browser.downloadWillBegin.suggestedFilename`에서 얻는다: `filename="..."`은 그대로, `filename*=UTF-8''...`은 디코드된 `한글-....txt`, `filename="../../x.txt"`는 `_.._x.txt`로 정리되고, 헤더가 없으면 URL의 마지막 조각(`plain.bin`).

**L5. 정책 연결이 닫혔을 때** [확인].
- `L5: policy connection closed after {"state":"inProgress","received":851968}; the download FINISHED (server sent all); ... the finished file landed: {"agentDir":true,"userDownloads":false} ... events after close seen on: []`: 진행 중이던 다운로드는 **정해 둔 폴더로 끝까지 받아졌다**(4,000,000바이트). 그러나 **이벤트는 더 오지 않는다**(완료 이벤트도). `pegboardd`가 완료를 알 수 없다.
- 연결이 닫힌 뒤에 시작한 다운로드는 `~/Downloads`로 간다(이름으로 찾았다). 그 다운로드의 시작 이벤트도 아무에게도 가지 않는다(`L5.afterLoss`: `landed: null`, 이벤트를 본 연결 없음).

**작업 5가 쓸 방법.** 프로필마다 정책 연결 하나를 두고(`Agent` 프로필에는 컨텍스트 ID로) `allowAndName` + `eventsEnabled`로 `pegboardd` 전용 폴더에 받게 한다. `downloadWillBegin.frameId`를 `targetCreated`로 쌓은 표(주 프레임은 `frameId == targetId`, 팝업은 `openerId`)와 탭의 `Page.getFrameTree`로 세션을 찾고, 사람 프로필의 이벤트(같은 연결에 온다)는 무시한다. 진행 이벤트로 한도를 검사하고(500ms 간격이라 한도를 넘는 양이 생기므로 한도에 여유를 두거나 `totalBytes`가 한도를 넘으면 시작 즉시 취소) `cancelDownload`로 끊는다. `Content-Length` 없는 다운로드는 `totalBytes`가 0이라 진행 이벤트의 `receivedBytes`로만 안다. 세션 폴더로의 이동은 `completed` 이벤트의 `guid` 파일을 옮기는 것이다. 정책 연결이 끊기면 그 프로필의 세션은 명령을 받지 않아야 한다(끊긴 뒤 시작한 다운로드는 `~/Downloads`로 간다). 새 연결을 걸어도 이전 연결의 정책은 되살아나지 않으므로 다시 걸어야 하고, 같은 컨텍스트에 연결 둘이 걸리지 않게 한다.

### C. 클립보드 (`22-clipboard.mjs`)

시작할 때 `pbpaste`로 저장하고 끝에서 `pbcopy`로 되돌렸다(종료 신호와 예외에서도). 측정 때 클립보드는 비어 있었고 끝난 뒤에도 비어 있다.

**C1. 권한을 주지 않은 에이전트 프로필 탭** [확인]. 기본 권한 상태는 `clipboard-read: prompt`, `clipboard-write: granted`.
- 쓰기: 사용자 제스처가 있는 `Runtime.evaluate`의 `writeText`가 **시스템 클립보드를 바꾼다**(포커스 에뮬레이션을 켰을 때). 제스처가 없으면 쓰이지 않는다(`PENDING`, 시스템 클립보드 그대로). 포커스 에뮬레이션이 없고 창이 앞에 없으면 `NotAllowedError: ... Document is not focused`. 페이지의 클릭 처리기에서 낸 `writeText`와 `clipboard.write`는 실제 마우스 클릭(`Input.dispatchMouseEvent`)이면 포커스 에뮬레이션 없이도 쓰인다.
- 읽기: `readText`는 기본이 `prompt`라서 **사람이 눌러 줄 권한 창을 기다리며 대답이 오지 않는다**(`PENDING: no answer in 5s`). 거부가 아니라 대기다.
- `C1.withFocusEmulation: ... W1_writeText_evaluate_gesture=CHANGED to expected marker (page: ok); W1b_writeText_evaluate_noGesture=unchanged (page: PENDING: no answer in 5s); W2_writeText_realClickHandler=CHANGED ...; R1_readText_evaluate_gesture=saw "PENDING: no answer in 5s"; R2_readText_realClickHandler=saw "ERR NotAllowedError ..."`

**C2. `Browser.setPermission` / `grantPermissions` / `resetPermissions`** [확인].
- `Browser.setPermission {permission:{name:"clipboard-read"|"clipboard-write"}, setting:"denied", origin, browserContextId}`가 된다. `clipboard-sanitized-write`는 `Invalid PermissionDescriptor name: clipboard-sanitized-write`. `grantPermissions`의 `clipboardReadWrite`, `clipboardSanitizedWrite`는 허용만 하고 거부는 없다. `Human`의 컨텍스트 ID는 `Failed to find browser context for id ...`로 거절된다.
- 거부 뒤: `writeText`(제스처, 클릭 처리기)와 `clipboard.write`는 `NotAllowedError ... Write permission denied`, `readText`는 `NotAllowedError`.
- 범위(`C2.scope`, 한 번도 권한 창이 뜬 적 없는 출처 `c.test`로 잼): **프로필(컨텍스트) × 출처 단위**다. 같은 프로필 같은 출처의 다른 탭에도 걸려 있고(`sameOriginNewTabSameProfile: read denied, write denied`), 다른 출처(`localhost`)에는 안 걸리고, 사람 프로필 탭에도 안 걸린다. **탭 단위는 없다.** `origin`을 안 주면 그 컨텍스트의 모든 출처에 걸린다(`noOriginGiven_otherOrigin: denied/denied`).
- **설정한 연결을 닫으면 설정이 사라진다**(`agentTabAfterSetterConnectionClosed: read prompt, write granted`). `resetPermissions`도 같은 효과다. 다운로드 정책과 같은 수명이다.
- 주의: 같은 출처에서 `readText`를 기다리게 두면 권한 창이 뜨고 반복되면 Chrome이 그 출처의 읽기를 `denied`로 막아 둔다(우리가 걸지 않았는데도 `denied`가 남았다). 범위 측정은 그래서 따로 새 출처로 했다.

**C3. `document.execCommand('copy')`와 `'paste'`** [확인].
- `copy`는 **제스처가 있으면(평가에 `userGesture:true`, 또는 페이지 클릭 처리기) 시스템 클립보드를 바꾸고, 클립보드 권한을 전부 거부해도 똑같이 바꾼다**(`C3: ... with all clipboard permissions denied [W3_... =CHANGED to expected marker, W4_execCommandCopy_realClickHandler=CHANGED ...]`). 제스처가 없으면 `false`이고 바뀌지 않는다. `execCommand('paste')`는 아무것도 읽어 오지 못했다.
- 포커스: 포커스 에뮬레이션이 없을 때 평가로 낸 `copy`는 `false`지만 실제 마우스 클릭으로 낸 것은 쓰인다(클릭이 문서에 포커스를 준다). 즉 포커스 에뮬레이션을 끄는 것은 방어가 아니다.
- 막는 방법: 다른 `pegboardd` 연결이 `Page.addScriptToEvaluateOnNewDocument`로 `Document.prototype.execCommand = ()=>false`를 넣으면 클릭 처리기의 `copy`도 막혔다(`C3.override: overrideActiveInPage:true, realClickWithOverride: pageSaw "false", system "unchanged"`). 같은 페이지의 iframe에서도 `false`였다. 그러나 (1) 키 입력의 `commands:["copy"]` 경로는 이것으로 안 막힌다(`keyCommandsWithOverride: CHANGED`), (2) **주입한 연결을 닫으면 주입이 사라져 다시 쓰인다**(`overrideActiveAfterInjectingConnectionClosed:false`), (3) 새 탭과 팝업에는 따로 주입해야 한다. 이 주입이 우회되는지는 더 시험하지 않았다.

**C4. CDP 키 입력** [확인].
- `Input.dispatchKeyEvent`로 Meta+C, Meta+X를 `commands` 없이 보내면 **시스템 클립보드가 바뀌지 않는다**. Meta+V도 아무것도 붙지 않는다(`W7, W9, R5: unchanged/empty`).
- `commands:["copy"]`, `["cut"]`을 붙이면 선택한 글자가 **시스템 클립보드에 쓰인다**. `commands:["paste"]`는 **시스템 클립보드의 내용이 페이지에 붙는다**(`R4: PAGE SAW THE SYSTEM CLIPBOARD`). 이 셋은 클립보드 권한을 전부 거부해도 똑같다(`C4: denied [W6=CHANGED, W8=CHANGED, R4=PAGE SAW THE SYSTEM CLIPBOARD]`).

**C5. 페이지의 `copy` 이벤트 처리기** [확인]. 페이지가 `copy` 처리기에서 `clipboardData.setData`와 `preventDefault`를 하면 `execCommand('copy')`와 키 `commands:["copy"]` 모두 **페이지가 정한 내용이 시스템 클립보드에 쓰인다**(`C5: default [... CHANGED to expected marker] ; denied [... CHANGED to expected marker]`). 권한 거부로 막히지 않는다.

**작업 7이 쓸 방법.** 에이전트 컨텍스트 × 출처에 `Browser.setPermission`으로 `clipboard-read`, `clipboard-write`를 `denied`로 건다(출처를 생략하면 그 컨텍스트 전체. 설정한 연결이 살아 있는 동안만 유지되므로 연결이 끊기면 다시 걸고, 그동안 세션 명령을 받지 않는다). 이것으로 `navigator.clipboard`의 읽기와 쓰기는 막힌다. 나머지 경로는 권한이 아니라 프록시와 문법이 막아야 한다: `Input.dispatchKeyEvent`의 `commands` 매개변수를 거부하고(`Meta`/`Ctrl` + `c`/`x`/`v` 조합은 `commands` 없이는 아무 일도 안 하지만 안전하게 같이 거부), `Runtime.evaluate`와 `callFunctionOn`의 `userGesture`를 `false`로 고쳐 쓰게 한다(제스처 없는 `execCommand('copy')`는 안 된다). 페이지 클릭 처리기의 `execCommand('copy')`는 권한으로도 막히지 않는다(아래 정지 조건 4). 완화로 `Page.addScriptToEvaluateOnNewDocument` 주입을 쓸 수 있으나 연결 수명과 새 탭 적용 문제가 G5와 같다.

### 프로필(브라우저 컨텍스트) 단위로만 강제되는 항목

| 항목 | 단위 | 수명 | 근거 |
|---|---|---|---|
| 다운로드 폴더/동작 (`Browser.setDownloadBehavior`) | 컨텍스트 | 건 연결이 살아 있는 동안. 같은 컨텍스트에 연결 둘이면 나중 것이 이기고, 나중 것을 닫으면 정책이 없어진다. 연결이 끊겨도 진행 중인 다운로드는 끝까지 받는다 | L1, L1b, L5 |
| 다운로드 이벤트 | 연결(컨텍스트와 무관, 모든 컨텍스트의 이벤트) | 이벤트를 켠 연결 | L1 |
| 클립보드 권한 (`Browser.setPermission`) | 컨텍스트 × 출처(출처 생략 시 컨텍스트 전체). 탭 단위 없음 | 건 연결이 살아 있는 동안 | C2 |
| 서비스 워커, 공유 워커가 내는 요청의 판정 | 탭이 아니라 별도 타깃(에이전트 컨텍스트에 속함). 붙어서 `Fetch.enable`을 해야 보인다 | 그 타깃 세션이 살아 있는 동안 | G1, G4 |
| 요청 가로채기 (`Fetch.enable`) | **탭(세션)마다**. 컨텍스트 전체에 한 번에 거는 방법은 없다. 브라우저 수준 자동 연결로 새 타깃에 건다 | 건 연결이 살아 있는 동안. 닫히면 열린다 | G5 |
| 스크립트 주입 (`Page.addScriptToEvaluateOnNewDocument`) | 탭(세션)마다 | 주입한 연결이 살아 있는 동안 | C3 |
| 포커스 에뮬레이션 | 탭마다 | 건 세션 | C1(방어가 아님) |

공통점: 건 연결이 사라지면 모두 풀리거나(권한, 가로채기, 주입) 정책 없이 기본값으로 돌아간다(다운로드). 그 프로필의 세션이 하나라도 있는 동안 이 연결들을 `pegboardd`가 유지하고, 하나라도 끊기면 그 프로필의 명령을 거절해야 한다.

### 정지 조건 판정 (계획 작업 2 5단계)

1. **G1에서 `Runtime.evaluate`로 낸 요청이나 페이지 스크립트의 `fetch`가 잡히지 않는다: 해당 없음(not met).** 둘 다 세 방식 모두에서 잡혔다(`G1: evalFetch and pageFetch caught with page-session Fetch: PASS`). 다만 알려야 할 구멍이 있다: 웹소켓 핸드셰이크는 어떤 방식으로도 안 잡히고, 서비스 워커가 `respondWith`로 처리한 페이지 요청은 페이지 세션에서 안 보이며 서비스 워커/공유 워커가 낸 요청은 그 타깃에 붙어야 보인다(브라우저 수준 자동 연결로 보였다).
2. **G4에서 에이전트 쪽이 가로채기를 끄거나 가릴 수 있고 프록시 필터로 그 메서드를 막아도 막히지 않는다: 해당 없음(not met).** A는 P의 가로채기를 끄지 못하고(`G4.switchOff: PASS`), P가 붙든 요청을 가로채지도 못한다(`Invalid InterceptionId.`). 다만 P보다 나중에 만든 세션이 `Fetch.enable`을 하면 먼저 판정해 P의 차단을 우회할 수 있다(조합 9, 10). 이 `Fetch.enable`은 프록시 허용 목록에 없어 막힌다(`filter.rs`를 읽은 것이며 프록시로 시험한 것은 아니다). 필터의 허용 목록에서 `Fetch.*`가 빠지지 않게 시험으로 고정해야 한다. 서비스 워커 경로(`G4.serviceWorker`)는 CDP 메서드가 아니라 필터로 막을 수 없고, P가 서비스 워커 타깃에도 붙는 것으로 막아야 한다.
3. **L2에서 링크 클릭 다운로드의 탭을 찾을 수 없다: 해당 없음(not met).** 링크 클릭, 직접 이동, `blob:`, `window.open` 탭, 같은 사이트 iframe, 서비스 워커 경로 모두 에이전트 탭으로 풀렸다(`L2: link click resolves to an Agent tab: PASS`). 예외: 다른 사이트 iframe(OOPIF)은 `iframe` 타깃까지만 알 수 있고 부모 탭은 이 측정으로 풀지 못했다.
4. **C1~C4에서 막을 방법이 없는 쓰기 경로가 있다: 해당함(met).** 제스처가 있는 `document.execCommand('copy')`(페이지 클릭 처리기, 또는 `userGesture:true`)는 클립보드 권한을 전부 거부해도 시스템 클립보드를 바꾼다(C3). 에이전트가 `eval`로 단추를 만들어 마우스 클릭으로 누르면 제스처를 얻을 수 있다. `commands:["copy"]` 키 경로도 권한으로는 못 막고 프록시가 `commands`를 거부해야 한다. 막는 방법으로 측정한 것은 스크립트 주입 하나뿐이고 연결 수명과 키 경로 때문에 불완전하다. 읽기는 권한 거부(`readText`)와 프록시의 `commands` 거부(붙여넣기 경로)로 막을 수 있다. 사용자에게 알리고 작업 7을 시작하기 전에 정할 것: 쓰기 경로를 "남는 위험"으로 받아들일지, 주입을 쓸지, `--allow clipboard`가 없는 세션에서 `eval`과 클릭을 어떻게 다룰지.

### 남는 질문과 재지 않은 것

- 사람 프로필의 컨텍스트 ID가 `setDownloadBehavior`와 `setPermission`에서 거절되고 에이전트 쪽이 받아들여지는 것이 이 측정의 모든 실행에서 그랬다. 제품의 시작 순서(사람 프로필이 먼저)에서도 같은지, 1단계 `04`의 "기본 컨텍스트가 실행마다 달랐다"와 어떻게 맞는지는 조사하지 않았다. 작업 5와 7은 시작할 때 에이전트 컨텍스트 ID로 정책이 걸리는지 확인하는 검사를 넣어야 한다.
- 웹소켓을 막을 수 있는지, 이름 해석을 `pegboardd`가 따로 하는 방법, HTTPS 요청의 가로채기, IPv6 주소, 서비스 워커와 공유 워커의 "재개 경주"는 재지 않았다.
- 사람이 직접 한 다운로드 클릭과 사람이 누른 Cmd+C/V는 재지 않았다. 클립보드는 텍스트만 다뤘다.
