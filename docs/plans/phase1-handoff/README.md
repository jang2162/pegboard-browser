# 1단계 인계 문서

- 작성일: 2026-11-12 (처음 작성 2026-11-11)
- 브랜치: `phase1-local-mvp`를 2026-11-13에 main으로 병합했다. 남은 것은 수동 확인 9가지([../../phase1-manual-checks.md](../../phase1-manual-checks.md))다
- 계획: [../2026-11-11-phase1-local-mvp.md](../2026-11-11-phase1-local-mvp.md)
- 이 폴더: `ledger.md`는 진행 원장(판정, 미룬 항목 전부), `task-N-report.md`는 작업별 구현 보고서다.

## 완료

10개 작업이 모두 구현되고 검토를 통과했다. 최종 전체 검토에서 치명적 문제는 없었고 중요한 문제 셋이 나와 병합 전에 고쳤다.

- **I1 (고침)**: 세션의 마지막 탭이 닫히면(사람이 닫거나 에이전트가 닫음) Chrome이 탭그룹을 지워서, 그 세션은 이후 탭을 만들 수 없었다. 이제 브리지(`extension/bridge/sw.js`의 `createTab`)가 그룹이 사라진 것을 알아채면 세션의 제목과 색으로 새 그룹을 만들고 새 그룹 ID를 돌려준다. 프록시와 세션 관리자는 그 ID로 옮겨 가고(`session info`, 세션 닫기가 새 그룹을 쓴다) 새 탭은 전과 같이 `finish_tab`의 컨텍스트 확인을 거친다. 그룹 만들기가 다른 이유로 실패하면 방금 만든 탭을 닫고 오류를 낸다. 브리지를 통한 정리용 닫기는 2초 안에서 재시도하고, 끝내 못 닫은 탭은 타깃 ID만 표준 오류에 남긴다.
- **I2 (고침)**: 거부된 명령의 첫 낱말이 문법의 명령어가 아니면(예: `pegboard "$PEGBOARD_SESSION" snapshot`처럼 핸들을 명령 자리에 친 경우) 동작 기록에 `[redacted]`로 남는다.
- **I3 (문서)**: 탭을 다른 세션의 그룹으로 끌어도 소속은 바뀌지 않는다(세션 범위는 타깃 ID). 수동 확인 M6, `architecture.md`, README를 코드에 맞췄고, 그룹을 따르게 할지는 2단계 결정으로 `roadmap.md`에 넘겼다.
- 작은 수정: `command.exec`의 `timeoutMs`는 1~600000만 받는다(아니면 `invalid_params`). 정상적인 세션 닫기가 시작된 뒤에는 `command.exec`가 `session_not_found`를 돌려주고 기록에 아무것도 덧붙이지 않는다(예외 두 가지: 데몬 종료 중 잠금을 기다리던 명령, 그리고 닫기가 실패한 뒤의 명령은 마감된 기록에 항목을 덧붙일 수 있다). 보안 절의 TCP 포트 문구에 단계 조건을 붙였고, README "알려진 한계"에 몇 줄을 더했다.

수정 보고서는 저장소 밖 작업 공간의 `final-fix-report.md`에 있다(RED/GREEN 출력 포함). 이 폴더에는 원장과 작업 1~10의 보고서가 있다.

## 남은 것

- **사용자의 수동 확인 M1~M8b** (2026-11-13에 수행했다. 8개는 기대대로, M5 다운로드 경로가 기대와 달랐고 같은 날 에이전트 프로필의 다운로드를 막아 고쳤다(`agent_download_writes_no_file`). 결과는 [../../phase1-manual-checks.md](../../phase1-manual-checks.md)의 "실제 결과" 칸. 아래는 수행 전에 적은 목록이다): 화면 잠금, 다른 데스크톱 공간, 절전, 설정 누출, 다운로드 경로, 탭 끌기(M6는 위 I3의 기대 결과로 바뀌었다), 창 닫기, 사람 창이 마지막 활성 창일 때의 탭 생성(M8), 에이전트 동작 중 다른 앱의 타이핑(M8b)이다.
- **병합 결정**: 푸시와 main 병합은 사용자가 요청할 때만 한다.
- **2단계로 넘긴 것**(`../../roadmap.md` 2단계의 "2단계에서 정할 것"): 그룹을 따라 소속 바꾸기, 토큰 없는 API의 권한 모델, 브라우저 수준 자동 연결을 프록시가 흉내 내기, 열린 파일 기술자 정리, 기록 보존 기간, 유휴 세션 만료.
- **알려진 미해결 관찰 (고치지 않았다)**:
  - 사람이 세션의 마지막 탭을 닫은 뒤에도 agent-browser의 `tab list`에 닫힌 탭이 남아 보였다. 새 탭은 정상으로 만들어지고 조종된다. 최종 재검토가 코드로 원인을 확인했다: 세션 관리자가 먼저 범위에서 지우면 프록시가 그 `targetDestroyed`와 `detachedFromTarget` 이벤트를 범위 밖 타깃으로 보고 에이전트에게 전하지 않는다. 이번 수정 이전부터 있던 동작이고 격리 영향은 없다. 2단계 항목.
  - agent-browser는 세션의 마지막 탭에 대한 `tab close`를 스스로 거부한다("Cannot close the last tab"). 에이전트 쪽 마지막 탭 닫기는 CDP 클라이언트로만 재현된다.
  - 세션 탭을 프록시로 만든 직후 곧바로 세션을 닫으면, 세션 관리자가 그 탭에 붙이던 포커스 에뮬레이션 작업이 잠시(최대 CDP 요청 시간 제한인 30초) 살아 있을 수 있다. 시험 하네스에서만 브라우저 핸들이 남은 것으로 관찰됐고, 최종 재검토는 제품에서 연결 누수나 닫기 멈춤이 없다고 판단했다. 시험에서는 짧게 기다려 피했다.
  - 두 클라이언트가 동시에 지워진 그룹으로 탭을 만들면 같은 제목의 그룹이 둘 생길 수 있다. 세션을 닫으면 둘 다 정리된다.

## 다른 기기 준비

요구 사항: macOS arm64, Rust stable(`cargo`), git, Claude Code와 superpowers 플러그인. Chromium 소스나 Xcode는 필요 없다.

```bash
git clone https://github.com/<소유자>/pegboard-browser
cd pegboard-browser
git checkout main

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

기대 결과: `cargo test --workspace`가 브라우저 없이 통과한다. 실제 브라우저 시험의 전체 목록과 돌리는 방법은 README의 "시험"을 본다.

그 기기에서 이미 agent-browser를 쓰고 있다면: 시험은 전용 소켓 디렉터리(`/tmp/pegboard-<uid>/ab`)와 `peg-`로 시작하는 세션 이름만 쓰므로 기존 데몬을 건드리지 않는다.

## 이어서 하는 방법

`ledger.md`의 `Ruling:` 줄은 계획서보다 우선한다(계획의 결함을 고친 판정들이다). 2단계를 시작하기 전에 `ledger.md`의 `minor (deferred)` 줄 전부와 위 "남은 것"을 읽고 2단계 계획에 넣을 것을 고른다. 실행 방식은 그대로다: 작업마다 구현 에이전트 하나, 독립 검토자 하나, 문제가 나오면 수정 후 범위를 좁힌 재검토.

## 계획서와 달라진 것 (요약)

자세한 근거는 `ledger.md`의 `Ruling:` 줄에 있다.

- **프록시 규칙**: 브라우저 수준 `Target.setAutoAttach`는 `waitForDebuggerOnStart`를 false로 바꿔 넘기고, 숨긴 자동 연결 세션은 재개한 뒤 끊는다. `Target.activateTarget`과 `Page.bringToFront`는 허용하지 않는다. `Page.navigate`와 탭 생성은 `http`, `https`, `data`, `about:blank`만 받고 정규화한 주소만 넘긴다. 브리지도 같은 주소 검사를 한다.
- **세션**: `register(id, context, group, bridge, initial_targets)` 하나뿐이다. 닫힌 세션은 표에서 지운다. 세션의 첫 탭도 `browserContextId`를 확인한 뒤에 세션에 넣는다. 탭 생성은 지워진 그룹을 스스로 복구한다(위 I1).
- **명령 필터**: 허용 목록 문법(`argv::check`). 파일 인자는 이름만 받고 세션 파일 디렉터리 아래로 바뀐다. 1단계에서 빠진 명령은 README의 "아직 안 되는 것"에 있다.
- **동작 로그**: 입력한 글자와 비밀 값은 보수적으로 가린다. 거부된 명령줄은 문법에 있는 첫 낱말만 남긴다(위 I2).
