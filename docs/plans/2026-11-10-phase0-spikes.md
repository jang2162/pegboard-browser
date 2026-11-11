# 0단계 검증 스파이크 실행 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 설계 문서의 미확인 가정 A1~A13에 각각 "맞다 / 틀리다 / 조건부"와 근거를 붙여, 1단계(로컬 MVP)를 시작해도 되는지 판정한다.

**Architecture:** 제품 코드는 만들지 않는다. `spikes/` 아래에 버릴 Node.js 스크립트를 두고, 순정 Chrome for Testing을 임시 사용자 데이터 디렉터리로 띄워 CDP로 실험한다. 스크립트는 측정값을 JSON으로 남기고, 사람이 그 값을 보고 `docs/spike-findings.md`에 판정을 적는다.

**Tech Stack:** Node.js 22 이상, `ws` 패키지, agent-browser 0.42.0(고정), Chrome for Testing, Chromium 소스와 depot_tools(작업 6만).

**Spec:** [docs/architecture.md](../architecture.md) 12절, [docs/roadmap.md](../roadmap.md) 0단계. 실행자는 두 문서를 함께 읽는다.

## Global Constraints

- 대상은 macOS arm64 한 가지다.
- agent-browser는 `0.42.0`으로 고정한다. 설계 문서의 [확인] 항목이 이 버전(커밋 `e751995`) 기준이다.
- 브라우저는 반드시 창이 뜨는 모드로 띄운다. 헤드리스로 얻은 결과는 A1, A5, A7, A11에 대한 근거가 아니다.
- 사용자가 평소 쓰는 Chrome 프로필을 건드리지 않는다. 모든 실험은 `spikes/out/` 아래의 임시 사용자 데이터 디렉터리에서 한다.
- 디버깅 포트와 프록시는 `127.0.0.1`에만 연다.
- `spikes/`의 코드는 버린다. 1단계에서 가져다 쓰지 않는다. 시험 코드와 리팩터링에 시간을 쓰지 않는다.
- `spikes/out/`과 `spikes/node_modules/`는 깃에 넣지 않는다.
- 결과 문서는 한국어로 쓰고, 직접 관찰한 것은 [확인], 그렇지 않은 것은 [미확인]으로 표시한다.
- 실행 스크립트마다 고유한 `--session` 이름을 쓰고 끝에 `close`한다. 이전 실행의 agent-browser 데몬이 남아 결과를 오염시키지 않게 한다.
- 커밋은 사용자가 요청할 때만 한다. 각 작업의 커밋 단계는 그 승인을 받은 뒤 실행한다.

## Review Focus

스파이크에서 "통과"가 거짓으로 나오기 쉬운 지점이다. 각 항목의 방지 장치를 해당 작업에 넣었다.

1. **agent-browser가 우리 엔드포인트 대신 자기 브라우저를 띄워 통과한다.** `--cdp` 접속이 실패하면 로컬 실행으로 넘어갈 수 있다. 프록시 로그에 그 명령의 CDP 메시지가 찍혔는지로 확인한다. (작업 3)
2. **가렸다고 생각했지만 실제로는 가려지지 않았다.** 측정 직전에 페이지의 `document.visibilityState`와 `document.hasFocus()`를 함께 기록한다. (작업 2)
3. **플래그를 켠 결과만 기록해 기본 동작을 모른다.** 모든 시나리오를 플래그 없음과 플래그 있음 두 번 돌리고 둘 다 남긴다. (작업 2)
4. **프로필 복사가 메모리에 있던 쿠키 덕에 통과한다.** 만료 시각이 있는 쿠키를 쓰고, 복사 전에 디스크의 쿠키 파일 수정 시각이 갱신됐는지 확인한다. (작업 5)
5. **프로필 구분을 타깃 하나로만 확인한다.** 프로필마다 탭을 두 개 이상 열고, 새로 만든 탭과 `window.open`으로 열린 탭까지 포함해 확인한다. (작업 5)

---

## 파일 구조

```
spikes/
  package.json            ws, agent-browser@0.42.0
  .gitignore              out/, node_modules/
  lib/
    launch.mjs            Chrome for Testing 실행과 종료
    cdp.mjs               최소 CDP 클라이언트
    fixture.mjs           실험용 페이지를 주는 로컬 HTTP 서버
    ab.mjs                agent-browser 실행 래퍼
  00-smoke.mjs            환경 확인
  01-occluded.mjs         A1
  02-proxy.mjs            A2 A3 A8 A9
  03-tabgroups.mjs        A4 A5 A7
  ext-tabgroups/          A5용 확장 프로그램 (manifest.json, sw.js)
  04-profiles.mjs         A11 A12 A13
  out/                    측정 결과 JSON, 임시 프로필 (깃 제외)
docs/
  spike-findings.md       가정별 판정과 근거
```

---

### Task 1: 실험 환경과 결과 문서 뼈대

**Files:**
- Create: `spikes/package.json`, `spikes/.gitignore`, `spikes/lib/launch.mjs`, `spikes/lib/cdp.mjs`, `spikes/lib/fixture.mjs`, `spikes/lib/ab.mjs`, `spikes/00-smoke.mjs`
- Create: `docs/spike-findings.md`

**Interfaces:**
- Produces:
  - `launchChrome({ name, args = [], profileDirectory }) -> Promise<{ proc, port, browserWsUrl, userDataDir, kill() }>`: `spikes/out/<name>/udd`를 사용자 데이터 디렉터리로, `--remote-debugging-port=0`으로 창 있는 모드 실행. `DevToolsActivePort` 파일에서 포트와 ws 경로를 읽는다. 실행 파일은 환경 변수 `CHROME`.
  - `connect(wsUrl, { headers } = {}) -> Promise<{ send(method, params = {}, sessionId) -> Promise<result>, on(eventName, cb), attach(targetId) -> Promise<sessionId>, close() }>`: `Target.attachToTarget`은 `flatten: true`.
  - `startFixture() -> Promise<{ origin, close() }>`: 아래 경로를 준다.
    - `/counter`: 버튼 `#btn`을 누르면 `#clicks` 숫자가 1 오른다. `setInterval(100ms)`로 `window.__ticks`, `requestAnimationFrame`으로 `window.__frames`를 센다. 배경색이 100ms마다 바뀐다.
    - `/cookie/set`: `Set-Cookie: spike=1; Max-Age=86400; Path=/`와 `localStorage.spike = "1"`.
    - `/cookie/get`: `document.cookie`와 `localStorage.spike`를 본문에 출력.
    - `/opener`: `window.open("/counter")`를 부르는 버튼 `#open`.
  - `ab(args: string[], { session, cdp, cdpHeaders, env } = {}) -> Promise<{ code, stdout, stderr, json }>`: `spikes/node_modules/.bin/agent-browser`를 `--json`과 함께 실행. `AGENT_BROWSER_SOCKET_DIR`는 `spikes/out/ab-sock`.
  - `writeResult(name, object)`: `spikes/out/<name>.json`에 저장.

- [ ] **Step 1: `spikes/package.json`과 `.gitignore` 작성 후 설치**

`dependencies`: `"ws": "^8"`, `"agent-browser": "0.42.0"`. `"type": "module"`.

Run: `cd spikes && npm install && npx agent-browser --version`
Expected: `0.42.0`이 출력된다.

- [ ] **Step 2: Chrome for Testing 설치와 경로 확정**

Run: `cd spikes && npx agent-browser install && npx agent-browser doctor`
Expected: 설치된 브라우저 경로가 출력에 보인다. 그 실행 파일 경로를 `export CHROME=...`로 지정하고 `docs/spike-findings.md`의 "환경"에 경로와 브라우저 버전을 적는다. `doctor`가 경로를 보여 주지 않으면 `~/.agent-browser` 아래에서 찾는다.

- [ ] **Step 3: `lib/launch.mjs`, `lib/cdp.mjs`, `lib/fixture.mjs`, `lib/ab.mjs` 구현**

위 Interfaces의 시그니처 그대로.

- [ ] **Step 4: `00-smoke.mjs` 작성과 실행**

브라우저를 띄우고 `Browser.getVersion`을 부르고, 픽스처의 `/counter`를 새 탭으로 연 뒤 `ab(["snapshot","-i"], { session: "smoke", cdp: String(port) })`를 실행하고 종료한다.

Run: `node spikes/00-smoke.mjs`
Expected: `product`에 `Chrome/` 버전이 찍히고, 스냅샷 출력에 `button`과 `[ref=`가 들어 있고, 종료 코드 0.

- [ ] **Step 5: `docs/spike-findings.md` 뼈대 작성**

구성: 환경(기기, macOS 버전, 브라우저 버전, agent-browser 버전), 요약 표(A1~A13: 가정, 판정, 근거 한 줄, 설계 영향), 가정별 상세 절, 설계 변경 제안. 가정 문장은 architecture.md 12절에서 그대로 옮긴다. 판정 칸은 비워 둔다.

- [ ] **Step 6: 커밋**

```bash
git add docs spikes/package.json spikes/package-lock.json spikes/.gitignore spikes/lib spikes/00-smoke.mjs
git commit -m "docs: add design docs and phase 0 spike scaffolding"
```

---

### Task 2: 가려진 창과 탭에서의 자동화 (A1)

가장 큰 위험이다. 다른 작업보다 먼저 한다.

**Files:**
- Create: `spikes/01-occluded.mjs`
- Modify: `docs/spike-findings.md` (A1 절)

**Interfaces:**
- Consumes: Task 1의 `launchChrome`, `connect`, `startFixture`, `ab`, `writeResult`
- Produces: `spikes/out/01-occluded.json`

**측정 설계**

시나리오 (대상 탭은 항상 `/counter`):

| 코드 | 상태 | 만드는 법 |
|---|---|---|
| `fg` | 앞에 보이는 탭 | 자동 |
| `bg-tab` | 같은 창의 뒤쪽 탭 | 자동: 다른 탭을 `Target.activateTarget` |
| `occluded` | 창 전체가 다른 앱에 가려짐 | 수동: 안내 문구 출력 후 Enter 대기 |
| `minimized` | 창 최소화 | 자동: `Browser.setWindowBounds { windowState: "minimized" }` |
| `other-space` | 다른 데스크톱 공간에 있음 | 수동 |
| `locked` | 화면 잠금 | 수동: 안내 후 30초 대기하는 동안 사람이 잠근다. 결과는 파일로만 남긴다 |

구성 (시나리오마다 전부 실행):

| 코드 | 내용 |
|---|---|
| `default` | 추가 플래그 없음 |
| `flags` | `--disable-backgrounding-occluded-windows --disable-renderer-backgrounding --disable-background-timer-throttling` |
| `cdp` | 플래그 없음 + 대상 탭에 `Emulation.setFocusEmulationEnabled { enabled: true }`와 `Page.setWebLifecycleState { state: "active" }` |
| `flags+cdp` | 둘 다 |

시나리오와 구성의 조합마다 기록:

| 필드 | 측정 |
|---|---|
| `visibilityState`, `hasFocus` | 측정 직전 `Runtime.evaluate` |
| `ticksPerSec`, `framesPerSec` | 5초 간격으로 `__ticks`, `__frames` 두 번 읽어 계산 |
| `snapshotMs`, `snapshotOk` | `ab(["snapshot","-i"])` 소요 시간과 성공 여부 |
| `clickOk`, `clickMs` | `ab(["click","#btn"])` 뒤 `#clicks`가 1 올랐는지 |
| `screenshotMs`, `screenshotOk` | `Page.captureScreenshot`, 제한 10초 |
| `screencastFrames` | `Page.startScreencast { format: "jpeg", everyNthFrame: 1 }` 후 10초 동안 받은 프레임 수. 프레임마다 `Page.screencastFrameAck` |

**A1 판정 기준** (작성자가 정한 값이며 findings에 그대로 적는다): `bg-tab`과 `occluded`에서, 네 구성 중 하나 이상이 다음을 모두 만족하면 "맞다" 또는 "조건부(그 구성 필요)".
- `snapshotOk`와 `clickOk`가 참
- `screenshotOk`가 참이고 `screenshotMs` 2000 이하
- `screencastFrames` 10 이상 (초당 1프레임 이상)

`minimized`, `other-space`, `locked`는 판정에 넣지 않고 한계로 기록한다. `locked`는 원격 데스크톱 사용에 직접 영향을 주므로 결과를 architecture.md 9절(릴레이)의 "원격 머신의 종류"에 반영한다.

- [ ] **Step 1: `01-occluded.mjs` 작성**

구성 4개를 바깥 반복, 시나리오 6개를 안쪽 반복으로 돈다. 구성이 바뀔 때마다 브라우저를 새로 띄운다. 수동 시나리오는 표준 입력으로 Enter를 기다린다. `--only=<시나리오>`, `--config=<구성>` 인자로 일부만 돌릴 수 있게 한다.

- [ ] **Step 2: 자동 시나리오만 먼저 실행해 스크립트 검증**

Run: `node spikes/01-occluded.mjs --only=fg,bg-tab,minimized`
Expected: `spikes/out/01-occluded.json`에 12개 항목(구성 4 × 시나리오 3). `fg`의 모든 `*Ok`가 참이고 `screencastFrames`가 10 이상. `fg`가 통과하지 못하면 측정 코드가 틀린 것이니 고친다.

- [ ] **Step 3: 가려짐 확인 장치 점검**

Run: Step 2 결과에서 `bg-tab`의 `visibilityState`를 본다.
Expected: `default` 구성에서 `hidden`. `visible`로 나오면 탭 전환이 되지 않은 것이니 스크립트를 고친다.

- [ ] **Step 4: 전체 실행 (사람이 옆에 있어야 한다)**

Run: `node spikes/01-occluded.mjs`
Expected: 24개 항목. 안내에 따라 창을 다른 앱으로 덮고, 다른 데스크톱 공간으로 옮기고, 화면을 잠근다.

- [ ] **Step 5: 에이전트 프로필 창 구성 재현**

설계에서는 에이전트 창이 사람 창과 별개다. 사람이 다른 창에서 작업하는 동안 에이전트 창이 화면 한쪽에 **일부만 보이는 상태**와 **전부 가려진 상태**를 각각 `occluded` 시나리오로 한 번씩 더 돌린다.

Run: `node spikes/01-occluded.mjs --only=occluded` (두 번, 결과 파일 이름을 `01-occluded-partial.json`, `01-occluded-full.json`으로)
Expected: 두 파일 생성.

- [ ] **Step 6: A1 판정 기록**

`docs/spike-findings.md`의 A1 절에 결과 표(시나리오 × 구성), 판정, 필요한 플래그나 CDP 호출, 한계를 적는다. 판정이 "틀리다"이면 Task 7로 가기 전에 사용자에게 알린다.

- [ ] **Step 7: 커밋**

```bash
git add spikes/01-occluded.mjs docs/spike-findings.md
git commit -m "spike: measure automation in occluded windows (A1)"
```

---

### Task 3: 세션 범위 CDP 프록시와 agent-browser (A2, A3, A8, A9)

**Files:**
- Create: `spikes/02-proxy.mjs`
- Modify: `docs/spike-findings.md` (A2, A3, A8, A9 절)

**Interfaces:**
- Consumes: Task 1의 모든 함수
- Produces: `spikes/out/02-proxy.json`, `spikes/out/02-methods.json`
- 스크립트 내부 함수: `startProxy({ upstreamWsUrl, token, allowedTargetIds: Set<string> }) -> Promise<{ url, log: Array<{dir, method, sessionId, blocked}>, httpRequests: string[], close() }>`

**프록시 동작 (스파이크용 최소판)**

- `127.0.0.1`의 임의 포트. WebSocket 경로 `/s/test`. `Authorization: Bearer <token>`이 없으면 401.
- 업스트림은 브라우저의 `browserWsUrl`.
- 클라이언트에서 브라우저로:
  - `Target.createTarget`: 통과. 응답의 `targetId`를 `allowedTargetIds`에 추가.
  - `Target.attachToTarget`, `Target.closeTarget`, `Target.activateTarget`: `targetId`가 허용 목록에 없으면 오류 응답.
  - `Browser.close`: 막고 빈 성공 응답을 돌려준다. `blocked: true`로 기록.
  - 그 밖: 통과시키고 메서드 이름을 기록.
- 브라우저에서 클라이언트로:
  - `Target.getTargets` 응답의 `targetInfos`에서 허용되지 않은 것을 뺀다.
  - `Target.targetCreated`, `Target.targetInfoChanged`, `Target.targetDestroyed`, `Target.attachedToTarget`: 허용되지 않은 타깃이면 버린다. 단, `openerId`가 허용 목록에 있으면 새 타깃을 허용 목록에 넣는다.
- 평문 HTTP 요청(`/json/version`, `/json/list` 등)이 오면 경로를 `httpRequests`에 기록하고 404를 준다.

**실험 절차**

브라우저에 "사람 탭" 2개(`/counter`, `/cookie/get`)를 먼저 열어 둔다. 허용 목록은 빈 상태로 시작한다.

| # | 명령 (모두 `ab(...)`에 `cdp: 프록시 url`, `cdpHeaders: {"Authorization":"Bearer <token>"}`, `session: "s1"`) | 기록할 것 |
|---|---|---|
| 1 | `open <origin>/counter` | 성공 여부. 새 탭이 생겼는지, 기존 사람 탭을 가져가지 않았는지 |
| 2 | `snapshot -i` | 성공 여부 |
| 3 | `click #btn` | `#clicks`가 올랐는지 (업스트림에 직접 붙어 확인) |
| 4 | `fill`용 입력란이 있는 페이지에서 `fill` | 성공 여부 (픽스처에 `/form` 추가) |
| 5 | `get url`, `screenshot spikes/out/02.png` | 성공 여부 |
| 6 | `tab list` | 사람 탭 2개가 **보이지 않는지** |
| 7 | `tab new <origin>/opener` 뒤 `click #open` | `window.open`으로 열린 탭이 `tab list`에 보이는지 |
| 8 | `eval "document.title"` | 성공 여부 |
| 9 | 잘못된 토큰으로 `snapshot` | 실패하는지. **실패 후 agent-browser가 자기 브라우저를 띄우지 않았는지** (`ps`로 Chrome 프로세스 수 비교) |
| 10 | `--action-policy`에 `{"default":"allow","deny":["eval"]}`을 주고 `eval` | 거부되는지 (A8) |
| 11 | `--confirm-actions eval`로 `eval` | `confirmation_required`가 오는지 (A8) |
| 12 | `stream status`, 이어서 `stream disable`, 다시 `stream status` | 스트림 서버 포트, 끌 수 있는지. `lsof -iTCP -sTCP:LISTEN -P`로 열린 포트 확인. 인증 없이 그 포트에 WebSocket이 붙는지 (A9) |
| 13 | `close` | 프록시 로그에 `Browser.close`가 있었는지. 브라우저와 사람 탭이 살아 있는지 |

모든 명령 뒤에 "이 명령 동안 프록시 로그에 메시지가 추가됐는가"를 확인한다(Review Focus 1). 추가되지 않았다면 그 명령은 프록시를 거치지 않은 것이므로 결과를 무효로 한다.

**판정 기준**
- A2 맞다: 1~8이 성공하고 6에서 사람 탭이 보이지 않는다.
- A3 맞다: `httpRequests`가 비어 있고 1이 성공한다. 비어 있지 않으면 "조건부(HTTP 탐색 구현 필요)"와 경로 목록.
- A8 맞다: 10과 11이 기대대로.
- A9 맞다: 12에서 서버를 끌 수 있거나, 시작부터 뜨지 않게 하는 방법이 있다.

- [ ] **Step 1: 픽스처에 `/form` 추가** (`<input id="name">`, `<input id="pw" type="password">`)

- [ ] **Step 2: `startProxy`와 실험 절차를 `02-proxy.mjs`에 작성**

- [ ] **Step 3: 실행**

Run: `node spikes/02-proxy.mjs`
Expected: `spikes/out/02-proxy.json`에 13개 단계의 결과, `spikes/out/02-methods.json`에 agent-browser가 쓴 CDP 메서드 이름과 호출 횟수(이름순).

- [ ] **Step 4: 실패한 단계 원인 조사**

실패한 단계마다 프록시 로그에서 마지막으로 오간 메시지를 확인하고, 프록시가 막아서인지 agent-browser가 기대한 응답이 달라서인지 구분해 적는다. 프록시 규칙 한두 줄로 고쳐지면 고치고 다시 돌린다. 고친 내용은 findings에 남긴다(1단계 프록시 설계의 입력이다).

- [ ] **Step 5: A2, A3, A8, A9 판정 기록**

findings에 판정과 함께 `02-methods.json`의 메서드 목록을 붙인다. 이 목록이 1단계 프록시 허용목록의 출발점이다.

- [ ] **Step 6: 커밋**

```bash
git add spikes/02-proxy.mjs spikes/lib/fixture.mjs docs/spike-findings.md
git commit -m "spike: scoped CDP proxy with agent-browser (A2 A3 A8 A9)"
```

---

### Task 4: 탭그룹 제어 (A4, A5, A7)

**Files:**
- Create: `spikes/ext-tabgroups/manifest.json`, `spikes/ext-tabgroups/sw.js`, `spikes/03-tabgroups.mjs`
- Modify: `docs/spike-findings.md` (A4, A5, A7 절)

**Interfaces:**
- Consumes: Task 1의 `launchChrome`, `connect`, `startFixture`, `writeResult`
- Produces: `spikes/out/03-tabgroups.json`, `spikes/out/03-protocol-groups.txt`
- 확장 프로그램 서비스 워커의 전역 함수 (CDP `Runtime.evaluate`로 부른다):
  - `pegTargets() -> Promise<Array<{ targetId, tabId }>>`: `chrome.debugger.getTargets()`에서 `type === "page"`인 것.
  - `pegGroup(tabIds: number[], title: string, color: string) -> Promise<number>`: `chrome.tabs.group` 뒤 `chrome.tabGroups.update`. 그룹 ID를 돌려준다.
  - `pegAdd(groupId: number, tabIds: number[]) -> Promise<void>`
  - `pegMembers(groupId: number) -> Promise<number[]>`: `chrome.tabs.query({ groupId })`의 탭 ID.

`manifest.json`: `manifest_version: 3`, `permissions: ["tabs", "tabGroups", "debugger"]`, `background.service_worker: "sw.js"`.

**실험 절차**

| # | 실험 | 기록할 것 |
|---|---|---|
| 1 | `http://127.0.0.1:<port>/json/protocol`을 받아 `group`이 들어간 명령, 이벤트, 타입 이름을 뽑는다 | 탭그룹 관련 항목이 있는지 (A4). 브라우저 버전도 함께 |
| 2 | `--load-extension=spikes/ext-tabgroups`로 실행하고 `Target.getTargets`에서 `type: "service_worker"`인 확장 타깃을 찾는다 | 확장이 로드되는지. 안 되면 CDP `Extensions.loadUnpacked`(파이프 접속 필요)를 시도하고 결과 기록 |
| 3 | CDP `Target.createTarget`으로 탭 2개를 만들고 `pegTargets()`로 `targetId`와 `tabId`를 대응시킨다 | 대응이 되는지 (A5의 핵심) |
| 4 | `pegGroup([두 탭], "조사", "blue")` 뒤 `pegMembers` | 그룹이 생기고 두 탭이 들어가는지. 화면에서도 확인 |
| 5 | 그룹 안 탭에서 `/opener`의 `#open`을 누른다 | `window.open`으로 열린 탭이 자동으로 같은 그룹에 들어가는지. 안 들어가면 `pegAdd`로 넣을 수 있는지 |
| 6 | CDP `Target.createTarget`으로 탭을 하나 더 만든다 | 새 탭이 어디에 생기는지(어느 창, 그룹 안팎). `pegAdd`까지 걸리는 시간 |
| 7 | 사람이 그룹 안 탭을 그룹 밖으로 끌어낸다(수동) | 확장이 `chrome.tabs.onUpdated`의 `groupId` 변화로 알 수 있는지 |
| 8 | `Target.createBrowserContext` 뒤 `Target.createTarget { browserContextId }` | 새 탭이 같은 창에 생기는지 별도 창인지. `pegGroup`으로 기존 그룹에 넣을 수 있는지 (A7) |
| 9 | 확장의 서비스 워커가 유휴로 내려간 뒤(30초 이상 대기) 다시 `pegMembers` | 다시 깨어나 응답하는지 |

**판정 기준**
- A4 맞다: 1에서 탭그룹 제어 명령이 없다.
- A5 맞다: 2, 3, 4가 성공한다. 5와 6에서 새 탭을 그룹에 넣을 수 있다.
- A7 맞다: 8에서 별도 창에 생기거나 그룹에 넣을 수 없다.

- [ ] **Step 1: 확장 프로그램 작성** (`manifest.json`, `sw.js`에 위 함수 4개와 `chrome.tabs.onUpdated` 기록)

- [ ] **Step 2: `03-tabgroups.mjs` 작성**

- [ ] **Step 3: 실행**

Run: `node spikes/03-tabgroups.mjs`
Expected: `spikes/out/03-tabgroups.json`에 9개 실험 결과. 4번에서 브라우저 창에 "조사"라는 파란 탭그룹이 보인다.

- [ ] **Step 4: A4, A5, A7 판정 기록**

A5가 "틀리다"이면 1단계가 포크 없이 불가능하다는 뜻이다. Task 7 전에 사용자에게 알린다.

- [ ] **Step 5: 커밋**

```bash
git add spikes/ext-tabgroups spikes/03-tabgroups.mjs docs/spike-findings.md
git commit -m "spike: tab group control via extension (A4 A5 A7)"
```

---

### Task 5: 프로필 격리와 복사 (A11, A12, A13)

**Files:**
- Create: `spikes/04-profiles.mjs`
- Modify: `docs/spike-findings.md` (A11, A12, A13 절)

**Interfaces:**
- Consumes: Task 1의 모든 함수, Task 4의 확장 프로그램
- Produces: `spikes/out/04-profiles.json`
- 스크립트 내부 함수: `openProfileWindow(userDataDir, profileDirectory, url) -> Promise<void>`: 같은 사용자 데이터 디렉터리와 `--profile-directory=<이름>`으로 `CHROME`을 한 번 더 실행해 실행 중인 브라우저에 창을 열게 한다.

**실험 절차**

브라우저를 `--profile-directory=Human`으로 띄운다. `openProfileWindow(udd, "Agent", ...)`로 두 번째 프로필 창을 연다.

A11 (프로필 구분):

| # | 실험 | 기록할 것 |
|---|---|---|
| 1 | 프로필마다 탭 2개씩 연 뒤 `Target.getTargets` | 타깃마다 `browserContextId`가 있는지, 같은 프로필끼리 같고 다른 프로필끼리 다른지 |
| 2 | `Target.getBrowserContexts` | 프로필이 목록에 나오는지 |
| 3 | Agent 프로필의 `browserContextId`로 `Target.createTarget` | Agent 창에 탭이 생기는지 |
| 4 | Agent 탭에서 `/opener`의 `#open` | 열린 탭의 `browserContextId`가 Agent 것인지 |
| 5 | `browserContextId`와 프로필 디렉터리 이름의 대응 | CDP만으로 "어느 것이 Agent인가"를 알 수 있는지. 안 되면 대안(프로필별 표식 페이지를 열어 확인) 기록 |
| 6 | 두 번째 프로필 창을 여는 방법이 실제로 동작하는지 | `openProfileWindow`의 성공 여부와 소요 시간 |

A12 (새는 것 목록):

| # | 실험 | 기록할 것 |
|---|---|---|
| 7 | Human에서 `/cookie/set`, Agent에서 `/cookie/get` | 쿠키와 `localStorage`가 넘어가지 않는지 |
| 8 | `--load-extension`으로 올린 확장이 어느 프로필에 있는지 | 프로필마다 `Target.getTargets`의 확장 서비스 워커 |
| 9 | Human에서 픽스처 출처에 알림 권한을 허용(수동), Agent에서 `navigator.permissions.query` | 사이트 권한이 프로필별인지 |
| 10 | Human에서 홈 버튼 표시 설정을 바꾼다(수동) | Agent 창에 반영되는지 |
| 11 | 두 프로필의 다운로드 기본 경로 | 프로필별로 다르게 둘 수 있는지 |
| 12 | Agent 탭에서 CDP `Storage.getCookies`(브라우저 수준, `browserContextId` 없이) | Human의 쿠키가 나오는지. **나온다면 프록시가 반드시 막아야 할 메서드다** |

A13 (복사):

| # | 실험 | 기록할 것 |
|---|---|---|
| 13 | Agent에서 `/cookie/set`. 5초 뒤 `<udd>/Agent` 아래 쿠키 파일의 수정 시각 확인 | 디스크에 기록됐는지 (Review Focus 4). 안 됐으면 기록될 때까지의 시간 |
| 14 | 브라우저가 켜진 채로 `<udd>/Agent`를 `<udd>/AgentCopy`로 복사(`Cache`, `Code Cache`, `GPUCache` 제외) | 복사 소요 시간, 크기, 오류 |
| 15 | `openProfileWindow(udd, "AgentCopy", "<origin>/cookie/get")` | 창이 열리는지, 쿠키와 `localStorage`가 있는지 |
| 16 | `<udd>/Local State`의 프로필 목록에 `AgentCopy`가 등록됐는지 | 자동 등록 여부 |
| 17 | AgentCopy에서 쿠키를 바꾸고 Agent에서 확인 | 원본에 영향이 없는지 |
| 18 | AgentCopy 창을 닫고 디렉터리를 지운 뒤 브라우저가 정상인지, `Local State`에 남는지 | 브라우저를 끄지 않고 삭제가 되는지 |
| 19 | 14~15를 Agent 창에서 페이지가 계속 쓰는 중(100ms마다 `localStorage` 갱신)에 5번 반복 | 5번 중 몇 번 정상인지 |

**판정 기준**
- A11 맞다: 1, 3, 4가 성공하고 5에서 Agent를 확실히 가려낼 방법이 하나 이상 있다.
- A12: 판정 대신 7~12의 표를 낸다. 새는 항목마다 대응을 한 줄씩 적는다.
- A13 맞다: 15, 17, 18이 성공하고 19가 5번 모두 정상. 일부 실패면 "조건부"와 실패 양상.

- [ ] **Step 1: `04-profiles.mjs` 작성**

- [ ] **Step 2: A11 실험(1~6) 실행**

Run: `node spikes/04-profiles.mjs --only=a11`
Expected: `spikes/out/04-profiles.json`의 `a11`에 6개 결과. 화면에 창이 두 개 뜬다.

- [ ] **Step 3: A12 실험(7~12) 실행** (9, 10은 안내에 따라 수동)

Run: `node spikes/04-profiles.mjs --only=a12`
Expected: `a12`에 6개 결과.

- [ ] **Step 4: A13 실험(13~19) 실행**

Run: `node spikes/04-profiles.mjs --only=a13`
Expected: `a13`에 7개 결과. 19번은 `{ attempts: 5, ok: <숫자> }`.

- [ ] **Step 5: A11, A12, A13 판정 기록**

A11이 "틀리다"이면 프로필 격리 대신 브라우저 인스턴스 분리로 물러서야 한다(architecture.md 결정 6). Task 7 전에 사용자에게 알린다.

- [ ] **Step 6: 커밋**

```bash
git add spikes/04-profiles.mjs docs/spike-findings.md
git commit -m "spike: profile isolation and copy (A11 A12 A13)"
```

---

### Task 6: Chromium 빌드 실측과 부트스트랩 패치 가늠 (A6, A10)

몇 시간이 걸린다. Task 2~5와 겹치지 않게, 빌드가 도는 동안 다른 작업을 한다. Chromium 소스는 이 저장소 밖(`~/chromium`)에 둔다.

**Files:**
- Modify: `docs/spike-findings.md` (A6, A10 절)
- Create: `spikes/out/06-build-log.txt` (깃 제외), `spikes/branding-test.patch`

**Interfaces:**
- Consumes: 없음
- Produces: findings의 실측 수치. 3단계 계획의 입력이다.

- [ ] **Step 1: 시작 전 상태 기록**

Run: `df -h ~ && sysctl -n hw.memsize hw.ncpu && xcodebuild -version`
Expected: 남은 디스크, 메모리, 코어 수, Xcode 버전. findings에 적는다. Chromium 문서의 macOS 빌드 요구 사항(디스크, Xcode 버전)을 읽고 [확인]으로 함께 적는다. 디스크가 요구 사항보다 적으면 여기서 멈추고 사용자에게 알린다.

- [ ] **Step 2: 소스 받기**

Chromium 공식 문서의 macOS 빌드 안내를 따른다. depot_tools를 받고, `~/chromium`에서 히스토리 없이 받는다. 현재 Extended Stable의 버전 번호를 공식 릴리스 정보에서 확인해 그 태그로 맞춘다.

Run: 각 명령을 `time`으로 감싸 `spikes/out/06-build-log.txt`에 남긴다.
Expected: 받은 버전 태그, 소요 시간, `du -sh ~/chromium/src` 결과. Extended Stable의 실제 릴리스 주기도 이때 확인해 [확인]으로 적는다(architecture.md 5절의 미확인 항목).

- [ ] **Step 3: 전체 빌드와 시간 측정**

`gn gen out/Default`의 인자: `is_debug=false`, `symbol_level=0`, `is_component_build=false`, `target_cpu="arm64"`.

Run: `time autoninja -C out/Default chrome`
Expected: 빌드 성공. 소요 시간, `du -sh out/Default`, 빌드 뒤 남은 디스크. 결과물이 실행되는지 `out/Default/Chromium.app` 실행으로 확인.

- [ ] **Step 4: 작은 패치의 증분 빌드 시간**

`chrome/app/theme/chromium/BRANDING`의 `PRODUCT_FULLNAME`을 `Pegboard`로 바꾼다.

Run: `time autoninja -C out/Default chrome` 뒤 `git diff > <저장소>/spikes/branding-test.patch`
Expected: 증분 빌드 시간. 앱 이름이 바뀌어 보이는지. 패치 파일이 다른 체크아웃에 `git apply --3way`로 들어가는지.

- [ ] **Step 5: 파이프 CDP 코드 읽기 (A6)**

읽을 것: `content/browser/devtools/devtools_pipe_handler.*`, `--remote-debugging-pipe`를 처리하는 시작 코드, 브라우저가 자식 프로세스를 띄우는 기존 예. 답할 질문:
- 파이프 핸들러가 "브라우저를 띄운 부모의 fd 3, 4"에 묶여 있는가, 임의의 fd 쌍을 받을 수 있는가.
- 브라우저가 시작 뒤 자식을 띄워 그 fd를 넘기려면 어떤 파일을 고쳐야 하는가. 파일 수와 대략의 줄 수.
- 더 작은 대안이 있는가. 예: `pegboardd`가 유닉스 소켓을 열고 브라우저가 거기에 접속.

Expected: findings의 A6 절에 고칠 파일 목록, 예상 규모(작음/중간/큼), 권하는 방식.

- [ ] **Step 6: 탭그룹과 프로필의 브라우저 쪽 API 위치 확인**

`Pegboard` CDP 도메인이 부를 내부 API를 찾는다: 탭그룹 생성과 소속 변경, 프로필 생성과 창 열기, 프로필 삭제. 기존 CDP 도메인이 `chrome/browser/devtools/protocol/` 아래에 어떻게 추가돼 있는지 한 개를 골라 파일 구성을 적는다.

Expected: findings에 API 이름과 파일 경로, 새 도메인 추가에 필요한 파일 목록.

- [ ] **Step 7: A6, A10 판정 기록**

A10 판정 기준: 전체 빌드가 하룻밤(8시간) 안에 끝나고, 증분 빌드가 30분 안이면 "맞다". 숫자는 작성자가 정한 기준이다. CI에 쓸 장비에 대한 의견을 한 문단 붙인다.

- [ ] **Step 8: 커밋**

```bash
git add spikes/branding-test.patch docs/spike-findings.md
git commit -m "spike: chromium build measurements and bootstrap patch estimate (A6 A10)"
```

---

### Task 7: 종합 판정과 설계 문서 갱신

**Files:**
- Modify: `docs/spike-findings.md` (요약 표, 설계 변경 제안)
- Modify: `docs/architecture.md` (12절 표에 판정 열 추가, 틀린 가정에 걸린 절)
- Modify: `docs/roadmap.md` (0단계에 완료 표시와 결과 링크, 일정 절)

**Interfaces:**
- Consumes: Task 2~6의 판정
- Produces: 1단계 계획의 입력

- [ ] **Step 1: 요약 표 채우기**

A1~A13 각각에 판정, 근거 한 줄(결과 파일 이름 포함), 설계 영향. 빈칸이 없어야 한다. 실험하지 못한 항목은 "미실시"와 이유를 적는다.

- [ ] **Step 2: architecture.md 12절 갱신**

표에 "판정" 열을 추가하고 findings의 해당 절로 링크한다. "맞다"로 확인된 가정이 본문에서 [미확인]으로 남아 있으면 [확인: 0단계 스파이크]로 바꾼다.

- [ ] **Step 3: 틀리거나 조건부인 가정에 대한 설계 변경**

12절의 "틀리면" 열을 출발점으로, 가정마다 본문의 어느 결정을 어떻게 바꿀지 findings의 "설계 변경 제안"에 적는다. architecture.md 본문은 사용자 승인 뒤에 고친다.

- [ ] **Step 4: 1단계 착수 판정**

다음을 모두 만족하면 "착수 가능": A1, A2, A5, A11이 "맞다" 또는 "조건부". 하나라도 "틀리다"이면 "설계 수정 필요"와 그 이유.

- [ ] **Step 5: 로드맵 갱신**

0단계에 완료 날짜와 findings 링크. "일정에 대해" 절에 Task 6의 실측치와, 스파이크 전체에 실제로 든 시간을 적는다.

- [ ] **Step 6: 정합성 확인**

Run: `grep -n "미실시\|판정" docs/spike-findings.md | head -40 && grep -c "\[미확인\]" docs/architecture.md`
Expected: 요약 표에 13개 판정이 모두 있다. architecture.md의 [미확인] 개수가 스파이크 전보다 줄었다(전: 실행 시작 시 세어 둔 값).

- [ ] **Step 7: 커밋하고 사용자에게 보고**

```bash
git add docs
git commit -m "docs: record phase 0 spike verdicts and design updates"
```

보고에 넣을 것: 13개 판정 요약, 착수 판정, 설계 변경 제안 목록. 설계 변경은 승인을 받는다.
