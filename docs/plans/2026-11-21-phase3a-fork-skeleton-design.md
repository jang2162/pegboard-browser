# 3a단계 설계: Chromium 포크 뼈대

- 작성일: 2026-11-21
- 상태: 사용자 승인 (2026-11-21)
- 기반: [architecture.md 4절 결정 3, 5절](../architecture.md#5-chromium-포크-전략), [roadmap.md 3단계](../roadmap.md#3단계-chromium-포크-v1), [spike-findings.md A6, A10](../spike-findings.md)
- 이 문서에서 "사용자 결정"은 사용자가 고른 것, "제안"은 작성자가 정한 것이다. 제안은 사용자가 고칠 수 있다.
- 3단계는 3a(포크 뼈대) → 3b(화면 패치) → 3c(배포) → 3d(유지) 순서로 나눠 하나씩 설계하고 만든다(사용자 결정, 2026-11-21). 이 문서는 3a만 다룬다.

## 1. 의도

3단계의 목적은 Pegboard를 **사람이 평소에 쓰는 브라우저로 설치**할 수 있게 하는 것이다. 지금은 `pegboardd`가 순정 Chrome을 띄우고, 디버깅 포트(루프백 TCP)로 붙고, 명령줄로 올린 확장 프로그램으로 탭그룹과 에이전트 프로필 탭을 다룬다. 이 구성으로는 사람이 Dock에서 브라우저를 켤 수 없고, 디버깅 포트는 같은 기기의 모든 프로세스에 브라우저 전체 제어권을 준다.

3a는 이것을 뒤집는 뼈대다. **브라우저가 `pegboardd`를 띄우고 파이프로 CDP를 넘기며, 확장 프로그램이 하던 일은 브라우저 안의 `Pegboard` CDP 도메인이 한다.** 3a가 끝나면 맥미니에서 빌드한 Pegboard.app을 켜는 것만으로 `pegboardd`가 같이 뜨고, 1~2단계의 CLI·MCP·시험이 디버깅 포트 없이 그대로 동작한다. 화면(탭그룹 표시, 확인 창, 구글 로그인 숨김, 비밀번호 관리자 끄기)은 3b, 서명·설치·업데이트는 3c, 리베이스 자동화는 3d다.

## 2. 목표, 하지 않는 것, 실패 조건

**목표**
- 패치 저장소 구조가 있고, 스크립트 하나로 고정한 Chromium 버전을 받아 패치를 적용하고 빌드할 수 있다.
- 빌드한 Pegboard.app을 켜면(Dock, `open`, 명령줄 어느 쪽이든) `pegboardd`가 자식으로 뜨고 파이프로 CDP를 받는다. 브라우저를 끄면 `pegboardd`도 세션을 정리하고 끝난다.
- 포크 모드에서 TCP 디버깅 포트가 열리지 않는다.
- 탭그룹, 에이전트 프로필 탭 만들기, 프로필 창 열기, 탭 전환 알림을 `Pegboard` CDP 도메인이 제공한다. 포크 모드에는 확장 프로그램이 없다.
- `pegboardd`는 브라우저와의 물리 연결 하나 위에 가상 연결 여러 개를 싣는 **중계 계층**으로 동작한다. 순정 모드와 포크 모드가 같은 중계 계층을 쓴다.
- 1~2단계의 자동 시험 전체가 두 모드에서 통과한다. 순정 모드는 이 기기에서, 포크 모드는 맥미니에서.

**하지 않는 것 (3a 범위 밖)**
- 서명, 공증, 설치 패키지, 자동 업데이트, 채널(3c). 3a의 앱은 임시 서명(ad-hoc)한 개발용 빌드다.
- 에이전트 탭그룹 표시, 확인 창, 구글 로그인 UI 숨김, 비밀번호 관리자 끄기(3b).
- 리베이스 자동화, 예행연습, 구버전 경고(3d).
- 공식 빌드 설정(`is_official_build`)과 그 빌드 시간 측정(3c).
- 기존 순정 모드 사용자의 데이터 옮기기. 순정 모드의 브라우저에는 실제 계정을 넣지 않기로 했으므로(architecture.md 결정 3) 옮길 것이 없다.

**실패 조건 (동작해도 이렇게 되면 실패)**
- 에이전트(세션 프록시를 거친 연결)나 브라우저 안의 확장 프로그램이 `Pegboard` 도메인을 부를 수 있다.
- 중계 계층에서 한 가상 연결의 이벤트나 응답이 다른 가상 연결로 샌다. 특히 한 세션 프록시가 다른 세션이나 사람 프로필의 타깃 이벤트를 받는다.
- 포크 모드에서 어떤 TCP 포트라도 DevTools를 받는다.
- 사람 프로필에서 탭을 만들 수 있거나, 사람 프로필의 탭을 그룹에 넣을 수 있다(1단계 격리 시험이 그대로 지켜야 한다).
- `pegboardd`가 죽으면 브라우저도 죽는다. 브라우저는 사람이 쓰는 것이라 데몬 장애에 끌려가면 안 된다.

## 3. 결정 사항

| # | 질문 | 결정 | 누가 |
|---|---|---|---|
| F1 | 빌드를 어디서 | 맥미니를 빌드 전용으로 | 사용자 (2026-11-21) |
| F2 | 3단계 순서 | 3a → 3b → 3c → 3d | 사용자 (2026-11-21) |
| F3 | 브라우저와 데몬 사이 연결, 브리지 대체 | 파이프 + `pegboardd`의 중계 계층 + C++ `Pegboard` 도메인 (아래 A안) | 사용자 (2026-11-21) |
| F4 | 순정 Chrome 모드 | 개발·시험용으로 남긴다. 브리지는 확장 판과 도메인 판 두 구현, 중계 계층은 공통 | 사용자 (2026-11-21) |
| F5 | Chromium 버전 | 착수 시점의 최신 Extended Stable. 조사 시점 기준 `156.0.8078.109`(mac_arm64). 1~2단계 시험은 157 CfT로 돌았다 | 제안 |
| F6 | 패치 관리 방식 | 깃 커밋 하나 = 패치 하나, `patches/series` 순서. 아래 4.1 | 제안 |
| F7 | 가상 연결 만드는 법 | 가상 연결마다 `Target.attachToBrowserTarget`으로 브라우저 타깃 세션을 하나씩 연다. 아래 4.4 | 제안 (첫 작업에서 검증) |
| F8 | 데몬이 죽으면 | 브라우저가 다시 띄운다. 1분에 3번까지. 넘으면 포기하고 브라우저는 계속 동작 | 제안 |
| F9 | 번들 ID와 이름 | 이름 `Pegboard`, 번들 ID `app.pegboard.browser`, 사용자 데이터 `~/Library/Application Support/Pegboard` | 제안 (3c 서명 전에 바꿀 수 있다. 서명 후에는 바꾸면 사용자 데이터 경로와 키체인 항목이 갈린다) |
| F10 | 포크 모드에서 사람 프로필 | 에이전트 프로필(데몬이 등록한 `PegboardAgent*` 디렉터리)이 **아닌 모든 프로필**이 사람 것이다. 사람 컨텍스트는 하나가 아니라 집합이고, 컨텍스트 ID는 바뀔 수 있다. 아래 4.5 | 제안 (검토에서 나옴) |
| F11 | 데몬이 다시 뜨거나 앱이 다시 켜질 때 남은 에이전트 상태 | 이어받지 않는다. 새 데몬은 시작할 때 남은 에이전트 탭·그룹·창을 닫고, 이전 세션을 끝난 것으로 기록하고, 열려 있던 녹화를 마감한다. 에이전트는 세션을 다시 만든다 | 제안 (검토에서 나옴) |
| F12 | `Pegboard` 도메인을 브라우저 쪽에서도 에이전트에게서 막나 | 막는다. 데몬이 처음 연 브라우저 세션(제어 세션)이 `Pegboard.claim`으로 도메인을 가져가고, 그 뒤 다른 브라우저 세션(세션 프록시의 가상 연결 포함)에는 도메인이 없다 | 제안 (검토에서 나옴) |
| F13 | 포크 모드 시험에서 파이프의 부모 | 데몬. 데몬이 포크 바이너리를 `--remote-debugging-pipe`로 직접 띄우는 **시험·개발용 세 번째 구성**을 둔다. 기존 e2e 하네스 구조가 그대로 간다 | 제안 (검토에서 나옴) |

### 접근 방식 (F3)

- **A. 파이프 + 중계 계층 + C++ `Pegboard` 도메인 (선택).** 설계 문서의 결정 3 그대로다. 파이프는 브라우저가 띄운 자식만 닿으므로 경계가 가장 좁다. 대신 파이프는 연결이 하나뿐이라 지금 `pegboardd`가 여는 여러 연결(세션 프록시마다, 다운로드 감시, 방어 장치, 녹화기)을 하나에 겹쳐 싣는 중계 계층이 새로 필요하다.
- **B. 유닉스 소켓 + C++ 도메인.** Chromium에 안드로이드용 유닉스 소켓 DevTools 서버 코드가 있고 macOS에서도 컴파일된다(조사로 확인). 연결 여러 개가 그대로 되어 중계 계층이 필요 없다. 대신 같은 macOS 사용자의 다른 프로세스도 소켓에 닿고, macOS에서 Chromium이 주는 접속자 정보는 사용자 ID뿐이라 서명 확인 코드를 직접 넣어야 한다. 사용자가 A를 골랐다.
- **C. 유닉스 소켓 + 내장 확장 프로그램.** 패치가 가장 작지만, 내장 확장도 `chrome.debugger`를 쓰면 "디버깅 중" 경고 띠가 뜨고(억제하려면 패치), 서비스 워커가 30초 쉬면 꺼지는 문제가 남는다. 설계 문서가 이미 버린 방식이다.

## 4. 설계

### 4.1 저장소 구조와 패치 관리

```
browser/
  CHROMIUM_VERSION          고정한 버전 태그 한 줄 (F5)
  patches/
    series                  적용 순서. 한 줄에 패치 하나
    branding.patch
    devtools-pipe-fds.patch
    pegboard-bootstrap.patch
    pegboard-cdp-domain.patch
  pegboard/                 Chromium 트리의 //pegboard/ 로 복사되는 새 소스
    BUILD.gn
    bootstrap/              데몬 실행과 파이프 연결
    devtools/               Pegboard 도메인 정의(.pdl)와 핸들러
  branding/                 아이콘, 이름 문자열
  args.gn                   개발 빌드 GN 인자
  scripts/
    fetch.sh                depot_tools와 소스 받기, 버전 태그로 맞추기
    apply.sh                pegboard/ 복사 + series 순서로 git apply --3way, 패치마다 커밋
    export.sh               작업 브랜치의 커밋을 patches/로 다시 뽑기
    build.sh                gn gen + autoninja
    package-dev.sh          pegboard와 agent-browser를 앱 번들에 넣고 임시 서명
```

- Chromium 소스는 이 저장소에 넣지 않는다(architecture.md 5절). 맥미니에 받아 둔 트리를 재사용한다.
- 패치를 고치는 흐름: `apply.sh`가 패치마다 커밋을 하나씩 만든다 → 그 브랜치에서 고친다(`git commit --fixup` 등) → `export.sh`가 커밋마다 패치 하나로 다시 뽑는다. ungoogled-chromium은 quilt를, Brave는 같은 경로 파일 덮어쓰기(`chromium_src`)를 쓴다(조사). 혼자 유지하는 데에는 깃만으로 되는 쪽이 도구가 하나 적다(제안).
- 새 코드는 `//pegboard/` 아래에 두고 업스트림 파일에는 연결용 몇 줄만 넣는다(architecture.md 5절 원칙 2).
- 패치마다 머리말에 목적과 건드리는 업스트림 파일을 적는다. 업스트림 파일 수정 줄 수를 `scripts/budget.sh`(또는 `export.sh`의 출력)로 센다. 3a가 끝날 때의 값을 기준선으로 기록한다.

### 4.2 패치

**branding.patch**: 앱 이름 `Pegboard`, 번들 ID, 아이콘, 사용자 데이터 디렉터리(F9). Chromium의 브랜딩 파일(`chrome/app/theme/chromium/BRANDING` 등) 위주. 0단계에서 브랜딩 패치 하나를 적용해 봤다(A10).

**devtools-pipe-fds.patch**: 지금의 파이프 핸들러는 fd 3, 4에 고정이고 인스턴스가 하나다(조사로 확인: `content/browser/devtools/devtools_pipe_handler.h`, `devtools_agent_host_impl.cc`). 임의의 fd 쌍을 받아 시작하고, 끝난 뒤 다시 시작할 수 있게 공개 함수를 하나 더한다. 0단계 소스 읽기 기준 업스트림 파일 2개에 십여 줄(A6). 메시지 형식은 기존 파이프와 같은 NUL 구분 JSON이다.

**pegboard-bootstrap.patch**: 브라우저 시작 때 `//pegboard/bootstrap`을 부르는 연결 한 곳. 부트스트랩이 하는 일:
1. 다음 중 하나면 아무것도 하지 않는다: `--remote-debugging-pipe`로 띄워졌다(부모가 이미 파이프를 가진 시험 구성, 4.6), `--pegboard-no-daemon`이 있다, 이미 실행 중인 브라우저에 요청만 넘기고 끝나는 프로세스다.
2. 파이프 두 개를 만들고 앱 번들 안의 `pegboard`를 `pegboard daemon --cdp-pipe`로 띄운다. 자식 쪽에서 파이프는 fd 3(데몬이 읽음), 4(데몬이 씀)에 둔다. 브라우저 쪽 끝으로 파이프 핸들러를 시작한다.
3. 데몬이 끝나면(파이프 EOF 또는 프로세스 종료) 파이프 핸들러를 정리하고 F8에 따라 다시 띄운다.
4. 브라우저가 끝날 때 파이프를 닫는다. 데몬은 EOF를 받고 세션을 정리한 뒤 끝난다. 데몬이 5초 안에 끝나지 않으면 브라우저가 종료 신호를 보낸다.

어느 시점에 띄울지(프로필 관리자가 준비된 뒤, 첫 창 전후)는 만드는 쪽이 정한다. 조건: 데몬이 붙은 직후 사람 프로필의 브라우저 컨텍스트를 찾을 수 있어야 한다(지금 `wait_for_first_context`가 하는 일).

**pegboard-cdp-domain.patch**: 새 도메인 `Pegboard`. 정의 파일은 `//pegboard/devtools/pegboard_protocol.pdl`에 두고, ChromeOS 전용 도메인(`cros_protocol.pdl`)이 `chrome/browser/devtools/BUILD.gn`에서 덧붙여지는 방식을 따라 붙인다(조사로 확인). 업스트림 수정은 BUILD.gn 인자, `inspector_protocol_config.json`의 도메인 항목, `chrome_devtools_session.cc`의 핸들러 등록 정도다.

도메인은 **신뢰된 클라이언트(파이프 연결)에만** 연다. 확장 프로그램의 `chrome.debugger`나 그 밖의 신뢰되지 않은 클라이언트에는 없는 도메인처럼 보인다(`IsDomainAvailableToUntrustedClient` 검사).

그런데 파이프 위의 가상 연결은 모두 같은 신뢰된 클라이언트라, 위 검사만으로는 세션 프록시의 가상 연결(에이전트)도 도메인을 볼 수 있다. 그래서 **제어 세션 하나에만** 연다(F12): 데몬이 처음 연 브라우저 세션이 `Pegboard.claim`을 부르면 그 세션이 주인이 되고, 그 뒤 다른 모든 세션에서 `Pegboard.*`는 없는 메서드로 응답한다. 주인 세션이 떨어지면(데몬 재시작) 다음 `claim`이 다시 가져간다. 세션 프록시의 메서드 허용목록이 첫 번째 벽, 이것이 브라우저 쪽의 두 번째 벽이다.

메서드는 지금 확장 판 브리지(`extension/bridge/sw.js`, `crates/pegboard-core/src/bridge.rs`)가 하는 일을 그대로 옮긴다:

| 메서드·이벤트 | 하는 일 | 지금은 |
|---|---|---|
| `Pegboard.openProfileWindow {profileDirectory}` → `{browserContextId}` | 프로필을 (없으면 만들어) 불러오고 창을 연다. 이미 열려 있으면 그 컨텍스트를 돌려준다 | 브라우저 바이너리를 `--profile-directory`로 다시 실행하고 새 컨텍스트를 기다림 |
| `Pegboard.createTab {browserContextId, url, groupId?}` → `{targetId, groupId}` | 그 프로필의 창에 탭을 만들고 그룹에 넣는다. 그룹이 없으면 만든다(`title`, `color`) | `chrome.tabs.create` + 그룹 |
| `Pegboard.createGroup`, `addToGroup`, `getGroupMembers`, `ungroup`, `ungroupTargets` | 탭그룹 조작. 타깃 ID로 말한다 | `chrome.tabGroups` + 타깃·탭 ID 대응 |
| `Pegboard.closeTargets {targetIds}` | 탭 닫기 | 확장 |
| 이벤트 `Pegboard.tabActivated {browserContextId, targetId, timestamp}` | 탭 전환 알림(녹화의 사람 개입 판정) | 워커의 `pegActivity` 바인딩 |

- 모든 메서드는 **에이전트 프로필이 아닌 컨텍스트와 그 탭을 거부한다**(F10). 에이전트 프로필은 디렉터리 이름이 `PegboardAgent`로 시작하는 프로필이다(지금 `profiles.rs`의 `profile_dir` 규칙). 브라우저 쪽이 이 규칙으로 직접 판정한다. 데몬이 알려 주는 값에 기대지 않는다.
- `Pegboard.getProfiles` → `[{profileDirectory, browserContextId, isAgent}]`와 이벤트 `Pegboard.profileLoaded`/`profileUnloaded`: 프로필 디렉터리와 컨텍스트 ID의 대응. 프로필이 내려갔다 다시 올라오면 컨텍스트 ID가 바뀌므로 데몬은 이것으로 표를 갱신한다(4.5).
- URL 허용 규칙(`about:blank`, 절대 `http:`, `https:`, `data:`만)은 지금 확장의 `allowedUrl`과 같다.
- 매개변수 모양은 만드는 쪽이 다듬어도 된다. 조건: Rust 쪽 브리지 trait(4.3)의 두 구현이 같은 시험을 통과한다.

### 4.3 Rust 쪽: 브리지를 trait로

`crates/pegboard-core/src/bridge.rs`의 공개 메서드(`create_group`, `create_tab`, `add_to_group`, `members`, `close_targets`, `follow_activity`, `ungroup_targets`, `ungroup`)를 trait로 뽑는다.

- `ExtensionBridge`: 지금 코드 그대로. 순정 모드.
- `DomainBridge`: `Pegboard.*`를 부른다. 워커 깨우기·되살리기 코드가 없다.
- `Browser::open_profile_window`도 모드에 따라 나뉜다. 포크 모드는 `Pegboard.openProfileWindow` 한 번.
- 다운로드 정책(`Browser.setDownloadBehavior` 컨텍스트별), 포커스 에뮬레이션, 방어 장치, 녹화는 표준 CDP라 두 모드에서 같다.

### 4.4 중계 계층

지금 `pegboardd`는 브라우저에 웹소켓을 여러 개 연다: `Browser`의 연결, 세션 프록시마다(`proxy::server::WsUpstream`, 이미 `Upstream` trait로 갈아 끼울 수 있게 되어 있다), 다운로드 감시(`downloads.rs`), 방어 장치(`guard.rs`), 녹화기(`recording/recorder.rs`). 모두 `ws_url`로 `CdpConn::connect`를 부른다.

새 구조:

```
BrowserLink (물리 연결 하나: 파이프 또는 웹소켓)
  ├─ 가상 연결 1  = Target.attachToBrowserTarget 로 얻은 브라우저 세션 S1
  ├─ 가상 연결 2  = 브라우저 세션 S2
  └─ ...
```

- **가상 연결 만들기 (F7)**: 가상 연결마다 `Target.attachToBrowserTarget`으로 브라우저 타깃 세션을 하나 연다. 가상 연결이 `sessionId` 없이 보내는 메시지에는 그 브라우저 세션의 ID를 붙여 보낸다. 브라우저 세션 위에서 켠 `Target.setDiscoverTargets`, `setAutoAttach`, 그 세션에서 붙인 자식 세션의 이벤트는 그 세션으로만 오리라고 기대한다. 그러면 중계 계층은 "세션 ID → 가상 연결" 표만 들고 있으면 된다.
- **라우팅**: 요청 ID는 물리 연결 전체에서 겹치지 않게 다시 매기고 응답에서 되돌린다. 들어오는 메시지는 `sessionId`로 주인을 찾는다. 자식 세션의 주인은 `Target.attachedToTarget` 이벤트와 `attachToTarget` 응답이 어느 브라우저 세션에서 왔는지로 기록하고, `detachedFromTarget`에서 지운다. 주인을 모르는 메시지는 **아무에게도 주지 않고** 기록만 한다(실패 조건: 새면 안 된다).
- **가상 연결 닫기**: 그 브라우저 세션과 자식 세션들을 떼고(`Target.detachFromTarget`) 표에서 지운다. 웹소켓 하나를 닫는 것과 같은 효과여야 한다(지금 프록시는 클라이언트 연결이 끝나면 위쪽 연결을 닫는다).
- **물리 연결**: 파이프(NUL 구분 JSON) 또는 웹소켓. 순정 모드도 웹소켓 하나 위에 같은 중계 계층을 쓴다(F4). 그래서 중계 계층의 시험 대부분이 이 기기에서 순정 Chrome으로 돈다.
- `CdpConn`과 `Upstream`은 가상 연결 위에서 지금 인터페이스 그대로 동작하게 한다. 호출하는 쪽(프록시, 다운로드, 방어 장치, 녹화기)은 `ws_url` 대신 `BrowserLink`에서 가상 연결을 받는 것만 바뀐다.
- **큰 메시지**: 방어 장치는 응답 본문을 조각으로 읽고, 큐 연결은 256MB까지 받는다(`cdp.rs`의 `QUEUED_MAX_MESSAGE`). 물리 연결 하나가 큰 메시지를 나르는 동안 다른 가상 연결이 막힌다. 녹화의 지연 기준(2b 설계 4.6, 명령 지연 중앙값 60ms)이 중계 계층 위에서도 지켜지는지 잰다.

**첫 작업에서 검증할 것 (F7의 전제)**: 순정 Chrome에서 웹소켓 하나 위에 브라우저 세션 둘을 열고 다음이 연결 둘을 연 것과 같은지 본다. 하나라도 다르면 중계 계층이 그 상태를 흉내 내야 하므로 설계를 다시 본다.
- `Target.setDiscoverTargets`, `Target.setAutoAttach`(브라우저 수준, `flatten`)를 한 세션에서만 켰을 때 다른 세션에 이벤트가 가지 않는다.
- 한 세션에서 붙인 페이지 세션의 이벤트가 다른 세션 쪽으로 오지 않는다.
- `Browser.setDownloadBehavior`, `Fetch.enable`(방어 장치), `Page.startScreencast`(녹화기)가 세션별로 독립이다.
- 세션 프록시가 지금 쓰는 메서드 허용목록 전체가 브라우저 세션 위에서 같은 결과를 낸다.
- 한 세션을 떼면 그 세션에서 붙인 자식 세션들이 같이 떨어진다(또는 중계 계층이 떼야 한다).

### 4.5 데몬의 두 모드와 CLI

- **순정 모드**: 지금처럼 `pegboardd`가 순정 Chrome을 띄운다. 디버깅 포트 + 확장 판 브리지 + 웹소켓 위의 중계 계층.
- **포크 모드**: 브라우저가 `pegboard daemon --cdp-pipe`로 띄운다. fd 3, 4의 파이프 + 도메인 판 브리지. 데몬은 브라우저를 띄우지도 끄지도 않는다.
- `pegboard browser start`: 포크 모드면 `open -b app.pegboard.browser`로 앱을 켜고 데몬 소켓이 생길 때까지 기다린다. 순정 모드면 지금처럼. 어느 모드인지는 설정 `browser.kind = fork | stock`으로 정하고, 기본값은 앱이 `/Applications` 또는 `~/Applications`에 있으면 `fork`, 없으면 `stock`(제안).
- `pegboard browser stop`: 포크 모드에서는 브라우저에 종료를 요청한다(`Browser.close`). 사람이 쓰던 창도 같이 닫히므로 CLI가 확인을 받는다(대화형일 때). MCP에는 이 동작을 주지 않는다(지금도 에이전트는 브라우저를 끌 수 없다).
- **시험·개발용 세 번째 구성 (F13)**: 데몬이 포크 바이너리를 `--remote-debugging-pipe --pegboard-no-daemon`과 시험 플래그로 띄우고 파이프의 부모가 된다. 브라우저를 띄우는 흐름은 순정 모드와 같고, 연결은 파이프, 브리지는 도메인 판이다. 환경 변수(예: `PEGBOARD_TEST_BROWSER=fork:/path/Pegboard.app`)로만 켠다. 사람이 쓰는 설정(`browser.kind`)에는 넣지 않는다.
- **사람 프로필 (F10)**: 포크 모드에서는 에이전트 프로필이 아닌 모든 프로필(추가로 만든 개인 프로필, 게스트 포함)이 사람 것이다. 지금 코드는 사람 컨텍스트를 값 하나(`Browser::human_context`)로 다루고 `downloads.rs`, `guard.rs`가 그 값과 같은지 비교한다. 이것을 "에이전트 컨텍스트 집합에 들지 않으면 사람 것"으로 뒤집는다. 에이전트 쪽 집합은 `Pegboard.getProfiles`와 그 이벤트로 유지한다. 순정 모드도 같은 판정으로 바꾼다(순정 모드에는 에이전트 프로필 외에 `Default`뿐이라 결과가 같다). `wait_for_first_context`("처음 보이는 페이지가 사람 것")의 가정은 포크 모드에서 쓰지 않는다. 창이 하나도 없이 켜진 브라우저에서도 데몬이 시작되어야 한다.
- **남은 상태 정리 (F11)**: 데몬은 시작할 때 에이전트 프로필의 창·탭·그룹을 모두 닫고(세션 복원으로 되살아난 것 포함), 이전 데몬의 세션을 끝난 것으로 기록하고, 열려 있던 녹화를 마감한다. 지금은 데몬이 죽으면 브라우저도 같이 죽었으므로(`kill_on_drop`) 이 경우가 처음 생긴다. 에이전트 프로필의 세션 복원 자체를 끄는 것은 3b에서 본다.
- `pegboard` 홈(세션, 녹화, 동작 기록)은 지금 경로 그대로다. 순정 모드의 `~/.pegboard/chrome`은 순정 모드만 쓴다.
- 데몬 소켓이 이미 있으면(다른 브라우저 인스턴스의 데몬, 또는 순정 모드 데몬) 포크의 데몬은 시작하지 않고 끝나며 브라우저는 F8의 재시도를 하지 않는다. 같은 기기에서 두 모드를 동시에 돌리지 않는다(제안).

### 4.6 시험

- **순정 모드 (이 기기, 지금처럼)**: 기존 시험 전체 + 중계 계층 단위 시험(가짜 브라우저로 라우팅, 새는 메시지 없음, 닫기) + 4.4의 검증 시험(실제 Chrome).
- **포크 모드 (맥미니)**: F13의 세 번째 구성. 데몬이 포크 바이너리를 시험 플래그(`--host-resolver-rules` 등, 지금 `browser.rs`가 넘기는 것)와 함께 파이프로 띄운다. 기존 e2e 시험 전체가 이 구성에서 통과해야 한다. 브리지 시험(`e2e_bridge`)은 두 구현에 같은 시험을 돌린다.
- **사람 프로필 시험 (포크 모드)**: 사람 프로필을 하나 더 만든 상태에서, 그 프로필의 탭에 세션이 닿지 못하고, 그 프로필의 다운로드에 에이전트 정책이 걸리지 않는다. 에이전트 프로필 창을 모두 닫아 프로필이 내려간 뒤 세션을 만들면 새 컨텍스트 ID로 동작한다.
- **부트스트랩 e2e (맥미니)**: 앱을 평소처럼 켠다(창이 없는 상태도 한 번) → 데몬이 뜬다 → `pegboard session create`와 동작 명령 하나 → 브라우저를 끈다 → 데몬이 끝나고 세션 탭이 정리됐다. 그 사이 브라우저 프로세스가 TCP로 듣는 포트가 없다(`lsof -iTCP -sTCP:LISTEN -a -p <pid>`). 세션이 있는 상태에서 데몬을 죽이면 다시 뜨고(F8), 남은 에이전트 탭과 그룹이 닫히고 이전 세션이 끝난 것으로 기록된다(F11). 4번째에는 뜨지 않으며 브라우저는 살아 있다.
- **도메인 차단 시험**: 세션 프록시로 `Pegboard.*`를 보내면 거부된다. 프록시의 허용목록을 일부러 끈 시험 빌드에서도, 제어 세션이 아닌 브라우저 세션에서 `Pegboard.*`는 없는 메서드다(F12). 확장 프로그램의 `chrome.debugger`로 붙은 세션에서도 없다.

### 4.7 빌드

- 맥미니의 Chromium 트리 하나를 계속 쓴다. 0단계 실측: 빈 상태부터 약 3시간 20분, 디스크 61GB, 한 줄 수정 증분 89초(A10). 착수 때 디스크 여유가 100GB 이상인지 확인한다.
- 개발 빌드 인자(제안): `is_debug=false`, `symbol_level=0`, `is_component_build=false`, `target_cpu="arm64"`, 구글 API 키 없음. 공식 빌드는 3c.
- `pegboard`(Rust)는 Chromium과 따로 빌드하고 `package-dev.sh`가 번들의 `Contents/Helpers/`(위치는 만드는 쪽이 정한다)에 agent-browser와 함께 넣는다. Rust만 고친 경우 Chromium을 다시 빌드하지 않는다.
- 맥미니 작업은 지금까지처럼 Orca 원격 터미널로 한다. 빌드가 몇 시간 걸리므로 백그라운드로 돌리고 끝을 기다린다.

## 5. 위험과 측정할 것

1. **F7의 전제가 틀릴 수 있다 (가장 큰 위험).** 브라우저 세션 둘이 연결 둘처럼 독립적이지 않으면, 중계 계층이 연결별 상태(타깃 감시, 자동 연결, 이벤트 구독)를 흉내 내야 하고 크기가 몇 배가 된다. 첫 작업에서 순정 Chrome으로 검증하므로 포크 빌드 전에 안다.
2. **C++ 시행착오 비용.** 헤더를 고치면 증분 빌드가 89초보다 훨씬 길 수 있다(A10에서 재지 못함). 도메인 핸들러의 첫 빌드 때 잰다.
3. **파이프 재시작.** 파이프 핸들러는 원래 프로세스당 한 번 시작하도록 만들어졌다. 다시 시작할 때 남은 상태가 있으면 F8이 어렵다. 안 되면 F8을 "데몬이 죽으면 다시 띄우지 않고, 브라우저를 다시 켜야 한다"로 낮추고 사용자에게 알린다.
4. **물리 연결 하나의 막힘.** 4.4 끝의 큰 메시지 문제. 녹화 지연 기준을 다시 잰다.
5. **패치 예산.** 3a 끝의 업스트림 수정 줄 수를 기준선으로 남긴다. 부트스트랩이 예상(중간)보다 커지면 그 이유를 적는다.

## 6. 만드는 쪽에 맡긴 것과 정한 값

- 부트스트랩을 부르는 정확한 지점(4.2).
- 사람 프로필을 브라우저 쪽이 아는 방법(4.2).
- `Pegboard` 도메인 매개변수의 세부 모양(4.2).
- 앱 번들 안의 `pegboard` 위치.
- 정한 값(제안): 데몬 재시작 1분에 3번(F8), 종료 대기 5초(4.2), 번들 ID `app.pegboard.browser`(F9).
- 사람 판정을 집합으로 뒤집을 때 `downloads.rs`, `guard.rs`, 녹화의 사람 개입 판정에서 바꿀 곳을 찾는 일.

## 7. 끝나는 조건

- `browser/` 구조와 스크립트로 맥미니에서 Pegboard.app을 처음부터 빌드할 수 있고, 걸린 시간을 기록했다.
- 4.4의 검증 결과를 spike-findings.md에 적었다.
- 순정 모드 시험 전체가 이 기기에서, 포크 모드 시험 전체가 맥미니에서 통과한다.
- 부트스트랩 e2e와 도메인 차단 시험이 통과한다.
- A6을 빌드로 확인했다(파이프 패치의 실제 크기). 헤더 수정 증분 빌드 시간을 쟀다.
- 업스트림 수정 줄 수 기준선을 기록했다.
- architecture.md(결정 3의 "다중화 미검증" 문구, 5절 [미확인] 중 조사로 확인한 것)와 roadmap.md를 고쳤다.
