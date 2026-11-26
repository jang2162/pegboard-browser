# 3c단계 설계: 배포 (빌드, 서명, 설치, 자동 업데이트)

- 작성일: 2026-11-26
- 상태: **설계(구현 전)**. 사용자 결정(아래 표의 "사용자")은 2026-11-10과 2026-11-26에 받은 것이다. "제안"은 작성자가 정한 것이고 사용자가 고칠 수 있다. 구현하며 처음 제안과 달라지는 곳은 3a·3b처럼 그 자리에 "처음 제안은 …"으로 적는다.
- 기반: [roadmap.md 3단계](../roadmap.md#3단계-chromium-포크-v1) 할 일 8, 9, [architecture.md 5절 "빌드와 배포 파이프라인"](../architecture.md#빌드와-배포-파이프라인)(2026-11-10 사용자 결정: macOS 먼저, Developer ID 보유, 공개 저장소 MIT, 업데이트 파일은 깃허브 릴리스, 채널 `stable`·`canary`), [3b 설계](./2026-11-25-phase3b-browser-ui-design.md) 10절 "열린 항목 (3c 이후)", [3a 설계](./2026-11-21-phase3a-fork-skeleton-design.md) 9절, [spike-findings.md](../spike-findings.md) "3a 작업 7"(키체인, `AperitifHelpers`), "3b 작업 7"(`pegboard_test_hooks`), "3b 작업 11"(사람이 본 확인)
- 3단계는 3a(포크 뼈대, 끝남) → 3b(화면 패치, 끝남) → 3c(배포) → 3d(유지). 이 문서는 3c만 다룬다.

## 1. 의도

3b까지 Pegboard.app은 개발 기기에서 빌드하고 `package-dev.sh`로 임시 서명해 시험 기기에 복사하는 앱이었다. 사람이 내려받아 설치할 수 없고, 새 버전이 나와도 알 길이 없으며, 앱을 다시 빌드할 때마다 키체인이 쿠키 저장소 열쇠를 다시 묻는다.

3c는 이것을 배포할 수 있는 제품으로 만든다. 개발 기기에서 스크립트 하나(`browser/scripts/release.sh`)가 시험 훅을 끈 배포 빌드를 만들고, `pegboard`와 agent-browser와 라이선스 고지를 넣고, 보유한 Developer ID로 서명하고, Apple에 공증받아 DMG로 묶고, 깃허브 릴리스와 업데이트 목록(appcast)에 올린다. 설치한 앱은 Sparkle 2로 새 버전을 찾아 서명을 확인하고, 종료할 때(또는 사람이 고를 때) 바꿔 끼우고 다시 켠다. 다시 켠 앱은 새 데몬을 띄운다.

3b의 열린 항목 중 배포와 함께 풀어야 하는 것(키체인 확인 창, 서명한 빌드의 `AperitifHelpers`, 시험 훅, pegboard 홈과 브라우저 사용자 데이터 디렉터리, 확인 창의 기본 버튼·잠금 모양, 도움말 화면의 "업데이트 확인 중 오류")도 여기서 닫는다.

## 2. 목표, 하지 않는 것, 실패 조건

**목표**
- `browser/scripts/release.sh canary <N>` 한 번이 배포 빌드 → 묶기 → 서명 → 공증 → DMG → 게시를 끝낸다. 중간에 멈추면 멈춘 단계부터 다시 시작할 수 있다. `release.sh stable <N>`은 이미 canary로 낸 빌드 `N`을 그대로(같은 바이트) stable로 올린다.
- 사람은 깃허브 릴리스에서 DMG를 내려받아 `Applications`로 끌어 설치하고, 처음 켤 때 Gatekeeper 경고("확인되지 않은 개발자")가 없다.
- 설치한 앱은 하루에 한 번 업데이트를 찾고, 내려받아 두었다가 종료할 때 설치한다. 앱 메뉴의 "업데이트 확인…"으로 바로 찾고 "설치 후 다시 시작"할 수 있다.
- 업데이트는 EdDSA 서명(Sparkle), 서명된 appcast, 같은 Developer ID 팀의 코드 서명을 모두 확인한다. 더 낮은 버전으로는 바뀌지 않는다.
- 채널은 `stable`과 `canary`. canary를 고른 앱은 둘 중 더 새것을 받는다.
- 업데이트로 다시 켠 앱에서 데몬이 새 판으로 다시 뜨고 CLI·MCP가 그대로 동작한다. 키체인이 다시 묻지 않는다.
- 사람에게 줄 빌드에는 브라우저 쪽 시험 훅도(`pegboard_test_hooks = false`) 데몬 쪽 시험 훅도 없다.
- pegboard 홈이 브라우저 사용자 데이터 디렉터리와 나뉜다. 기존 홈은 한 번 옮긴다.
- 확인 창의 "거부"가 기본 버튼으로 보이고, 잠긴 동안 버튼이 흐리게 보인다.
- 도움말 화면(`chrome://settings/help`)에 "업데이트 확인 중 오류" 줄이 없다.
- `pegboard` CLI를 `PATH`에 두는 방법이 문서에 있고, 심볼릭 링크로 불러도 동작한다.
- 앱 안에 agent-browser(Apache-2.0), Sparkle(MIT), Pegboard(MIT)의 라이선스가 있고, Chromium과 그 제3자 코드는 `chrome://credits`로 고지된다.

**하지 않는 것 (3c 범위 밖)**
- CI 러너, 빌드 캐시 서버(아래 R1: 개발 기기에서 스크립트로). 리베이스 자동화, 구버전 경고(3d).
- 인텔 맥(`x64`)과 유니버설 빌드. 3c는 `arm64` 하나다.
- 델타 업데이트(Sparkle의 `.delta`). 매번 DMG 전체를 받는다(약 200 MB 예상).
- `is_official_build = true`(링크 시간 최적화, PGO). 빌드 시간과 메모리가 크게 늘어 3d 이후로 둔다(열린 항목).
- 도움말 화면에 Sparkle 상태를 잇기(R19의 이유). 업데이트는 앱 메뉴와 자동 확인으로 한다.
- 충돌 보고 수집(Crashpad 업로드 서버), 사용 통계.
- 3b가 남긴 화면 항목(웹 UI 로고, 구글 문구, 확인 창 번역, `network` 확인).

**실패 조건 (동작해도 이렇게 되면 실패)**
- 서명이나 공증이 안 된 바이트가 깃허브 릴리스나 appcast에 올라간다. appcast의 항목이 가리키는 파일의 바이트가 EdDSA 서명과 다르다.
- 설치한 앱이 EdDSA 서명이 틀린 업데이트, 다른 팀(또는 임시 서명)으로 서명된 업데이트, 더 낮은 버전, HTTPS가 아닌 주소의 업데이트, 서명되지 않은 appcast 중 어느 하나라도 받아들인다.
- 에이전트나 페이지가 업데이트를 고르거나 피드 주소를 바꿀 수 있다(사용자 기본값 `SUFeedURL`, 환경 변수, 명령줄, CDP 어느 것으로든).
- 사람에게 줄 빌드에서 환경 변수로 확인 창의 자동 답이나 데몬의 시험 훅이 켜진다.
- 업데이트 뒤 앱이 데몬 없이 뜬다(옛 데몬이 잠금을 쥔 채 남음 등), 또는 사람 프로필의 쿠키·로그인이 사라진다.
- 업데이트 뒤 키체인 `Pegboard Safe Storage` 확인 창이 뜬다(사람 없는 화면에서 모든 HTTP를 막는다).
- Apple 자격 증명(앱 암호, API 키), EdDSA 비밀 키, 서명 ID의 이름(사람 이름이 들어 있다), 깃허브 소유자 이름이 저장소에 들어간다. 에이전트가 Apple 자격 증명을 다룬다.
- 빌드가 `-j 8`보다 높은 병렬로 돈다(개발 기기가 커널 패닉으로 멈췄다).
- 1~3b의 보장이 약해진다(3b 계획 Global Constraints의 목록 그대로).

## 3. 결정 사항

| # | 질문 | 결정 | 누가 |
|---|---|---|---|
| R1 | 어디서 빌드하나 | **개발 기기에서 스크립트로**(CI 러너가 아니다). Chromium 트리와 `out/` 산출물이 이미 그 기기에 있고, 서명 ID가 그 기기의 로그인 키체인에 있다 | 사용자 (2026-11-26, "이 기기에서 스크립트로") |
| R2 | 진입점 | `browser/scripts/release.sh <canary\|stable> <N>`. canary는 단계 `preflight → build → package → notarize → dmg → publish`, stable은 `promote` 하나. 단계마다 작업 디렉터리에 끝 표시를 남기고, `PEGBOARD_RELEASE_FROM=<단계>`로 그 단계부터 다시 한다(`apply.sh`의 `PEGBOARD_APPLY_FROM`과 같은 꼴) | 사용자(스크립트 하나) + 제안(단계) |
| R3 | 배포 빌드 설정 | `browser/args.release.gn`, 산출 디렉터리 `out/Release`(개발 빌드 `out/Pegboard`와 따로라 둘 다 증분으로 남는다). `args.gn`과 다른 줄은 `pegboard_test_hooks = false` 하나(시험이 이것을 고정한다). `dcheck_always_on = false`, `disable_fieldtrial_testing_config = true`는 그대로(R14). `is_official_build`는 켜지 않는다 | 사용자(훅 끔, 따로 디렉터리) + 제안(나머지) |
| R4 | 서명 | 로그인 키체인의 Developer ID Application 서명 ID로, hardened runtime(`-o runtime`), 보안 타임스탬프, **안쪽부터**(도우미·라이브러리·프레임워크 → 바깥 앱), `--deep` 없이. Chromium 부분은 트리의 맥 서명 도구(`chrome/installer/mac/signing`)가 쓰는 순서·옵션·엔타이틀먼트를 따른다(4.3). 서명 ID는 실행 때 키체인에서 찾고 저장소에 이름을 쓰지 않는다 | 사용자(Developer ID) + 제안(방법) |
| R5 | 공증 | `xcrun notarytool submit --keychain-profile pegboard-notary --wait`. 프로필은 **사용자가 한 번** `xcrun notarytool store-credentials pegboard-notary`로 만든다. 에이전트는 Apple 자격 증명을 보거나 다루지 않는다(계획에서 멈추고 사용자에게 부탁하는 단계). 앱과 DMG를 각각 공증하고 둘 다 스테이플한다 | 사용자 |
| R6 | 설치 패키지 | DMG 하나(`Pegboard.app` + `Applications` 링크, 압축 `ULFO`). 서명·공증·스테이플. 같은 DMG가 Sparkle의 업데이트 파일이다(파일 하나만 서명·공증·게시한다) | 사용자(DMG) + 제안(같은 파일) |
| R7 | 자동 업데이트 | **Sparkle 2**(2.9 이상: 서명된 appcast). `Sparkle.framework`를 `Contents/Frameworks/`에 넣고, `//pegboard/updater`가 실행 중에 그 번들을 읽어(링크하지 않는다) `MainParts::PostBrowserStart`에서 시작한다. 업스트림 줄 0. 샌드박스 앱이 아니므로 Sparkle의 XPC 서비스는 뺀다 | 사용자(Sparkle 2, 최소 패치) + 제안(실행 중 읽기) |
| R8 | 업데이트 정책 | 자동 확인 하루 한 번(`SUScheduledCheckInterval` 86400), 자동 내려받기, **종료할 때 설치**(`SUAutomaticallyUpdate`). 앱 메뉴 "업데이트 확인…"은 Sparkle 기본 창(설치 후 다시 시작). 실행 중인 앱을 업데이트가 끄지 않는다. 다시 켤 때 지난 세션을 복원한다(4.6) | 제안 |
| R9 | 업데이트 검증 | EdDSA(`SUPublicEDKey`), `SUVerifyUpdateBeforeExtraction` = YES, `SURequireSignedFeed` = YES(appcast와 릴리스 노트 서명), Sparkle의 코드 서명 팀 확인. `SUAllowsDowngrades`는 넣지 않는다. 피드와 내려받기 주소는 HTTPS만 | 사용자(EdDSA + 팀 일치) + 제안(서명 피드) |
| R10 | 업데이트 키 | EdDSA 비밀 키는 Sparkle의 `generate_keys`가 개발 기기의 로그인 키체인에 만든다. **사용자가 돌린다**. 공개 키만 `browser/release/sparkle-public-ed-key`에 커밋한다. 비밀 키 백업(`generate_keys -x`)은 사용자가 정한다 | 사용자 |
| R11 | 채널 | appcast 하나에 Sparkle 채널. stable = 기본 채널(채널 표시 없음), canary = `<sparkle:channel>canary</sparkle:channel>`. 앱의 선택은 앱 메뉴 "업데이트 채널"(안정 / 카나리), 사용자 기본값 `PegboardUpdateChannel`(없으면 안정). canary 앱은 기본 채널도 본다(Sparkle 규칙) | 사용자(두 채널) + 제안(Sparkle 채널) |
| R12 | appcast를 어디에 | **깃허브 Pages(`gh-pages` 브랜치의 `appcast.xml`)**. DMG는 깃허브 릴리스 자산. 주소(소유자, 저장소)는 실행 때 `gh repo view`에서 얻고 저장소 파일에 쓰지 않는다. 비교는 아래 "접근 방식" | 사용자(깃허브 릴리스) + 제안(Pages) |
| R13 | 버전 | 빌드 번호 `N`: 모든 채널이 함께 쓰는 1부터의 정수, 늘기만 한다. 보이는 버전 `<Chromium 버전>-pb.<N>`(예 `156.0.8078.109-pb.3`), 태그 `v<보이는 버전>`. `CFBundleVersion` = `<Chromium BUILD>.<PATCH>.<N>`(예 `8078.109.3`, Sparkle이 비교하는 값). `CFBundleShortVersionString`은 Chromium 값 그대로(프레임워크 경로가 이 버전이다) | 제안 |
| R14 | `AperitifHelpers` | 서명한 배포 빌드에서 `--enable-features=AperitifHelpers`로 HTTP 요청이 되는지 잰다(3a의 [미확인] 닫기). 결과와 상관없이 `disable_fieldtrial_testing_config = true`는 **둔다**: 그 파일은 시험용 실험 묶음이고 Chrome도 배포 빌드에 쓰지 않는다 | 사용자(확인) + 제안(둔다) |
| R15 | 키체인 `Pegboard Safe Storage` | 고정된 서명 ID에서는 키체인 항목의 접근 목록이 지정 요구 사항(팀 ID와 번들 ID)으로 묶여, 업데이트 뒤 묻지 않을 것으로 본다 [미확인]. 시험 기기에서 잰다: (1) 다른 서명이 만든 항목이 없을 때 첫 실행, (2) 개발 빌드가 만든 항목이 있을 때 첫 실행, (3) 업데이트 뒤. (3)에서 묻으면 **배포를 멈춘다**. (2)만 묻으면 첫 실행 안내에 적는다 | 사용자(측정) + 제안(멈춤 조건) |
| R16 | 시험 훅 | 브라우저: `pegboard_test_hooks = false`(R3), 그리고 그 빌드의 `DaemonLauncher`는 `PEGBOARD_TEST_*`를 데몬 환경에서 **언제나** 지운다. 데몬: 배포 빌드(`PEGBOARD_RELEASE_VERSION`을 주고 빌드한 `pegboard`)의 `testenv::var`는 언제나 없음 | 사용자 + 제안(데몬 쪽) |
| R17 | pegboard 홈과 브라우저 사용자 데이터 | 브라우저 사용자 데이터는 그대로 `~/Library/Application Support/Pegboard`, pegboard 홈의 기본값을 **`~/Library/Application Support/Pegboard Agent`**로 옮긴다. 데몬과 CLI가 처음 시작할 때 옛 위치의 pegboard 항목만(정해진 이름 목록) 옮긴다(4.8). `PEGBOARD_HOME`은 그대로 이긴다 | 제안 |
| R18 | 확인 창 모양 | "거부"에 강조 모양을 직접 건다(키 창이 아니어도 그려지게). 잠긴 동안은 세 버튼을 흐린 모양으로 직접 그린다. 동작(잠금 규칙, 기본 버튼)은 3b 그대로 | 사용자(3b 확인에서 나옴) + 제안(방법) |
| R19 | 도움말 화면의 업데이트 줄 | **숨긴다**. Chromium의 업데이트 상태(Keystone·Chromium 업데이터 쪽)를 "사용 안 함"으로 보이게 해 줄이 없어지게 한다. 방법은 트리에서 고른다(4.7, 패치면 10줄 이하). Sparkle에 잇지 않는 이유: 그 화면의 "다시 시작"은 Chromium 자신의 재실행이고, Sparkle이 번들을 바꾸는 동안 옛 번들에서 다시 뜨는 경쟁이 생긴다 | 사용자(숨기거나 잇기) + 제안(숨긴다) |
| R20 | CLI를 `PATH`에 | 문서에 적은 심볼릭 링크 한 줄(`ln -s /Applications/Pegboard.app/Contents/Helpers/pegboard <PATH의 디렉터리>/pegboard`). 앱이 업데이트되어도 경로가 같다. MCP 설정은 앱 안의 절대 경로를 쓴다. `pegboard`는 자기 위치를 심볼릭 링크를 풀어 찾는다 | 사용자(방법을 정하라) + 제안(링크) |
| R21 | 첫 실행 | 앱을 켜면 지금처럼 데몬이 뜬다. Sparkle은 자동 확인을 묻지 않는다(`SUEnableAutomaticChecks` = YES). DMG에서 바로 켠 앱(옮기지 않음)에는 따로 안내하지 않는다(남는 위험) | 제안 |
| R22 | 라이선스 고지 | `Contents/Resources/Notices/`에 `Pegboard-LICENSE.txt`(MIT), `agent-browser/`(그 패키지의 `LICENSE`와 고지·제3자 파일 전부), `Sparkle/LICENSE`. Chromium은 빌드가 만드는 `chrome://credits`. 릴리스 본문에 같은 목록 | 사용자(MIT, Apache 고지) + 제안(자리) |
| R23 | 시험 | 실제 설치는 시험 기기(GUI 세션, 화면 앞의 확인은 agent-desktop). 끝의 증명은 **canary 업데이트 왕복**: vN 설치 → vN+1 canary 게시 → 앱이 바뀌고 데몬이 다시 뜸. 거부해야 할 업데이트는 따로 리허설 피드로 시험한다(4.10) | 사용자 |
| R24 | 검토 | 서명(작업 6), 게시(작업 8), 업데이터(작업 9)는 구현 검토와 따로 **보안 검토**: 업데이트 경로의 무결성, 키 다루기, 내리기 공격, HTTPS, 배포 빌드에 자동 승인 길이 없음 | 사용자 |

### 접근 방식

**Sparkle을 앱에 넣는 법 (R7)**
- **A. 실행 중에 읽기 (선택).** `release.sh`가 서명 전에 `Sparkle.framework`를 `Contents/Frameworks/`에 복사하고, `//pegboard/updater`가 `NSBundle`로 그 번들을 읽어 `SPUStandardUpdaterController`를 만든다. 업스트림 GN과 링크 설정을 건드리지 않는다. 개발 빌드(`package-dev.sh`)에는 Sparkle이 없어 업데이터가 아무것도 하지 않는다. hardened runtime의 라이브러리 검증은 같은 팀이 서명한 프레임워크를 받아들인다.
- **B. 빌드 때 링크.** `chrome/BUILD.gn`에 `bundle_data`와 프레임워크 링크를 더한다. 업스트림 줄이 늘고, 모든 빌드(개발 포함)가 Sparkle을 갖는다. 버렸다.
- **C. 직접 만든 업데이터.** 서명 검증, 설치, 다시 켜기를 새로 짜야 하고 보안 검토 범위가 커진다. 버렸다.

**appcast를 어디에 둘까 (R12)**

| 자리 | 장점 | 단점 |
|---|---|---|
| **깃허브 Pages, `gh-pages` 브랜치 (선택)** | 고정 HTTPS 주소, 파일 하나를 커밋으로 바꾸고 이력이 남는다, 본 브랜치(`main`) 이력과 섞이지 않는다 | 사용자가 Pages를 켜야 한다. CDN 캐시로 몇 분 늦게 보인다 |
| 깃허브 릴리스 자산(`releases/latest/download/appcast.xml`) | 따로 켤 것 없음 | "latest"는 프리릴리스(canary)를 가리키지 않는다. 목록 파일이 릴리스마다 흩어진다 |
| `raw.githubusercontent.com`의 `main` 파일 | 따로 켤 것 없음 | 릴리스마다 `main`에 커밋이 생긴다. raw 주소는 배포용이 아니다 |

DMG는 Pages에 둘 수 없다(파일 100 MB 한도). 깃허브 릴리스 자산 한도(2 GiB)는 넉넉하다.

**서명하는 법 (R4)**
- **A. 트리의 맥 서명 도구 (선택, 156에서 확인).** Chromium의 `chrome/installer/mac/signing`(빌드 산출물의 `Pegboard Packaging/sign_chrome.py`)이 도우미 앱, 프레임워크 안의 라이브러리와 실행 파일을 각자의 옵션(`restrict`, `kill`, `library`, `runtime`)과 엔타이틀먼트로 안쪽부터 서명한다. 우리가 더한 것(`pegboard`, `agent-browser`, `Sparkle.framework`, 고지)은 그 전에 우리가 서명한다. 도구의 공증·DMG 기능은 쓰지 않는다(R5, R6은 우리 단계).
- **B. 우리 스크립트로 같은 순서.** A가 Chromium 이름 빌드(구글 설정이 없는)에 맞지 않으면, 그 도구의 부품 목록과 엔타이틀먼트 파일을 읽어 같은 순서로 `codesign`을 부른다.

## 4. 설계

### 4.1 `release.sh`와 작업 디렉터리

```
release.sh canary <N>
  preflight  서명 ID, 공증 프로필, EdDSA 키, gh 로그인·Pages, 동봉물 체크섬, N이 appcast의 가장 큰 N보다 큰지
  build      out/Release 배포 빌드 (build.sh <src> release, -j 8, 약 2시간 45분 빈 상태 / 증분 수 분)
  package    cargo build --release(PEGBOARD_RELEASE_VERSION), 앱 복사, 동봉, Info.plist, 서명, 검증
  notarize   앱 zip → notarytool → 스테이플 → 검증
  dmg        DMG 만들기 → 서명 → notarytool → 스테이플 → 검증
  publish    깃허브 릴리스(canary는 프리릴리스) → 올린 바이트 다시 받아 비교 → appcast 항목 → appcast 서명 → gh-pages 푸시 → Pages에서 다시 받아 비교
release.sh stable <N>
  promote    appcast에서 N의 canary 표시를 지우고 다시 서명·푸시, 릴리스를 프리릴리스에서 내림
```

- 인자: 첫째는 채널, 둘째는 빌드 번호 `N`(정수). Chromium 트리는 `PEGBOARD_SRC`(없으면 멈춤). 작업 디렉터리 `<저장소>/target/release-pegboard/<N>/`(`target/`은 이미 깃이 무시한다): `Pegboard.app`, `Pegboard-<보이는 버전>.dmg`, 공증 기록, `release.log`, 단계 끝 표시 `stages/<단계>.done`.
- **떼어 돌리기**: 빌드가 길고 도구의 시간 제한이 긴 명령을 끊으므로 언제나 `nohup … > release.log 2>&1 &`로 돌리고 표시 파일과 로그를 본다. 스크립트 안의 Chromium 빌드는 `build.sh`를 거쳐 `-j 8`, `nice`다. `PEGBOARD_BUILD_JOBS`를 8보다 크게 주면 `release.sh`가 거부한다.
- **떼어 돌리기 전에 키체인 창**: 서명 ID와 EdDSA 키를 처음 쓸 때 macOS가 "항상 허용"을 묻는다. 떼어 돈 스크립트에서 뜨면 아무도 답하지 않아 멈추므로, `preflight`를 **앞에서**(떼지 않고) 한 번 돌려 사람이 답하게 한다. `preflight`는 작은 임시 파일에 서명하고 `sign_update`를 한 번 부른다.
- 단계는 앞 단계의 산출물만 읽는다. `package`는 `out/Release/Pegboard.app`을 작업 디렉터리로 복사해 그 사본만 고치므로, 빌드 산출물은 다른 `N`의 `package`에 그대로 다시 쓸 수 있다(Chromium이 같으면 `N+1`은 빌드 없이 `PEGBOARD_RELEASE_FROM=package`로 약 30~60분).
- 설정(저장소에 두는 것, 비밀 아님): `browser/release/config.sh`에 공증 프로필 이름(`pegboard-notary`), 채널 이름, 확인 주기, appcast 파일 이름. 저장소에 두지 않는 것: 서명 ID 이름(`security find-identity -v -p codesigning`에서 "Developer ID Application"이 정확히 하나일 때 그것, 여럿이면 `PEGBOARD_SIGN_IDENTITY`에 SHA-1을 주라고 멈춤), 소유자·저장소(`gh repo view`).

### 4.2 묶기 (`package`)

`Pegboard.app` 사본에:
- `Contents/Helpers/pegboard`: `PEGBOARD_RELEASE_VERSION=<보이는 버전> cargo build --release -p pegboard-cli`. 그 값이 `pegboard --version`과 `browser.status`의 `version`이 되고, 데몬의 시험 훅을 끈다(R16).
- `Contents/Helpers/agent-browser`: `scripts/fetch-agent-browser.sh`가 받은 것. 배포에서는 잠금 파일(`vendor/agent-browser.lock`)이 비어 있으면 처음 받은 값을 적는 대신 **멈춘다**(지금 스크립트는 비어 있으면 받은 값을 적는다).
- `Contents/Frameworks/Sparkle.framework`: `scripts/fetch-sparkle.sh`가 고정 판을 받고 `vendor/sparkle.lock`의 SHA-256으로 확인한 것. `XPCServices/`는 지운다(R7). Sparkle 배포물의 `bin/`(`generate_keys`, `sign_update`)은 개발 기기에서만 쓰고 앱에 넣지 않는다.
- `Contents/Resources/Notices/`(R22).
- `Info.plist`(PlistBuddy로, 서명 전에):

| 키 | 값 |
|---|---|
| `CFBundleVersion` | `<BUILD>.<PATCH>.<N>`(R13) |
| `PegboardVersion` | `<보이는 버전>` |
| `SUFeedURL` | `https://<소유자>.github.io/<저장소>/appcast.xml`(R12) |
| `SUPublicEDKey` | `browser/release/sparkle-public-ed-key`의 한 줄 |
| `SUEnableAutomaticChecks` | YES |
| `SUAutomaticallyUpdate` | YES |
| `SUScheduledCheckInterval` | 86400 |
| `SUVerifyUpdateBeforeExtraction` | YES |
| `SURequireSignedFeed` | YES |

넣지 않는 것(검증이 거부한다): `SUAllowsDowngrades`, `SUEnableJavaScript`, `SUAllowedURLSchemes`, HTTPS가 아닌 `SUFeedURL`.
- 서명(4.3) 뒤 검증: `codesign --verify --strict --deep -vv`(검증에는 `--deep`을 쓴다. 서명에는 쓰지 않는다), 모든 Mach-O가 hardened runtime이고 팀 ID가 같은지(`codesign -dv`를 파일마다), `spctl -a -t exec -vv`는 공증 뒤에. `Info.plist`의 위 표를 다시 읽어 맞는지. 시험 훅 문자열이 브라우저 프레임워크에 없는지(`PEGBOARD_TEST_DIALOG_ANSWER`를 `strings`로 찾으면 실패).

### 4.3 서명

- 순서(안쪽부터): `Sparkle.framework`의 `Autoupdate`, `Updater.app` → `Sparkle.framework` → `Contents/Helpers/agent-browser`, `Contents/Helpers/pegboard` → (트리의 도구가) 프레임워크 안의 라이브러리·실행 파일, `Pegboard Helper*.app`, `Pegboard Framework.framework` → 바깥 `Pegboard.app`.
- 옵션: 모두 `--timestamp -o runtime`. 엔타이틀먼트는 트리의 것(`app-entitlements.plist`, `helper-renderer/gpu/plugin-entitlements.plist`)을 쓰되 **프로비저닝 프로필이 있어야 하는 제한 엔타이틀먼트는 뺀다**(Developer ID 앱에 프로필 없이 그것이 있으면 실행이 거부된다. 156의 목록은 작업에서 확인. 빼서 꺼지는 기능은 결과에 적는다).
- `pegboard`와 agent-browser는 엔타이틀먼트 없이 시작한다. agent-browser가 hardened runtime에서 돌지 않으면(실행 중 코드 생성 등) 그 파일 하나에만 필요한 것을 주고 이유를 적는다.
- 브랜딩의 `MAC_TEAM_ID`(지금 비어 있음)를 서명 도구가 요구하면 실행 때 서명 ID에서 읽은 팀 ID를 넘긴다(트리 파일에 쓰지 않는다).

### 4.4 공증과 DMG

- 앱: `ditto -c -k --keepParent Pegboard.app Pegboard.zip` → `notarytool submit --keychain-profile pegboard-notary --wait` → 결과가 `Accepted`가 아니면 `notarytool log`를 작업 디렉터리에 받고 멈춘다 → `stapler staple Pegboard.app` → `stapler validate`, `spctl -a -t exec -vv`(출처 `Notarized Developer ID`).
- DMG: 스테이플한 앱과 `Applications` 링크를 담은 폴더에서 `hdiutil create -volname Pegboard -format ULFO -srcfolder …` → `codesign --timestamp`(DMG 자체) → 공증 → 스테이플 → `spctl -a -t open --context context:primary-signature -vv`.
- 공증은 한 번에 수 분~수십 분이다. 기다림은 `--wait`가 하고, 끊기면 `PEGBOARD_RELEASE_FROM=notarize`(또는 `dmg`)로 다시 한다(제출 ID를 기록해 두고 이미 받아들여졌으면 다시 내지 않는다).

### 4.5 appcast와 게시

- `browser/scripts/appcast.py`(파이썬 표준 라이브러리, `xml.etree`): `add`, `promote`, `remove`, `check`. `appcast.xml`은 `gh-pages` 브랜치의 작업 사본에서 고친다.
- 항목: `title`, `pubDate`, `sparkle:version`(= `CFBundleVersion`), `sparkle:shortVersionString`(보이는 버전), `sparkle:minimumSystemVersion`(앱의 `LSMinimumSystemVersion`), canary면 `sparkle:channel`, `description`(짧은 본문과 깃허브 릴리스 링크, 항목 안에 넣어 따로 서명할 릴리스 노트 파일이 없다), `enclosure url length type sparkle:edSignature`.
- `add`가 거부하는 것: `N`이 이미 있음, 새 `sparkle:version`이 어떤 항목보다 크지 않음(내리기 방지, 채널과 상관없이), `https://github.com/<소유자>/<저장소>/releases/download/`로 시작하지 않는 주소, 길이·서명 없음, 올리려는 DMG 안 앱의 `SUFeedURL`이 운영 피드가 아님(리허설 빌드가 섞이지 않게, 4.10).
- 항목은 최근 20개만 둔다. `promote N`: 그 항목의 채널 표시를 지운다(같은 바이트, 같은 서명). `remove N`: 잘못 낸 canary를 더 퍼지지 않게 지운다(이미 받은 앱은 내려가지 않는다. 고친 `N+1`을 낸다).
- 서명: 고친 `appcast.xml`에 Sparkle의 `sign_update`를 돌려 피드 서명을 넣는다(R9). 서명 뒤에는 고치지 않는다.
- 게시 순서(앞이 실패하면 뒤를 하지 않는다): (1) `gh release create v<보이는 버전> <DMG> --prerelease`(canary), 본문에 버전·Chromium 버전·SHA-256·고지 목록 (2) 자산 주소에서 다시 받아 SHA-256이 같은지 (3) `appcast.py add` → `sign_update` → `appcast.py check` (4) `gh-pages`에 커밋·푸시 (5) Pages 주소에서 다시 받아 같은지(캐시 때문에 10분까지 다시 본다).
- `promote`: (1) `appcast.py promote` → 서명 → 푸시 → 다시 받아 비교 (2) `gh release edit --prerelease=false --latest`.

### 4.6 업데이터 (`//pegboard/updater`)

- `pegboard::Updater`(Objective-C++). `MainParts::PostBrowserStart`에서 만들고 `PostMainMessageLoopRun`에서 지운다.
- 시작 조건: 앱 번들에 `Contents/Frameworks/Sparkle.framework`가 있고, 명령줄에 `--user-data-dir`이 **없다**(제품 실행만. 시험과 사람이 따로 띄운 인스턴스가 업데이트를 받지 않는다). 시험 훅 빌드에서만 `PEGBOARD_TEST_UPDATER=1`이 이 조건을 무시하고 상태를 기록한다(`PEGBOARD_TEST_UPDATER_LOG`).
- `NSBundle`로 Sparkle을 읽고(`-load`), `SPUStandardUpdaterController`를 `startingUpdater:YES`와 우리 위임자로 만든다. 쓰는 Sparkle 선언은 `//pegboard/updater/sparkle_interface.h` 하나에 모은다(Sparkle 공개 헤더에서 쓰는 것만 옮김, MIT 고지).
- 위임자: `allowedChannelsForUpdater:` → 사용자 기본값 `PegboardUpdateChannel`이 `canary`면 `{"canary"}`, 아니면 빈 집합. `feedURLStringForUpdater:`는 **만들지 않는다**(피드는 `Info.plist` 하나). `updaterWillRelaunchApplication:` → 다음 시작에 지난 세션을 복원하게 Local State의 "종료 때 지난 세션 다시 열기" 값(156에서 이름 확인, Chrome 자신의 다시 시작이 쓰는 것)을 켠다.
- 메뉴: 앱 메뉴("Pegboard")의 "Pegboard 정보" 아래에 "업데이트 확인…"(`checkForUpdates:`), "업데이트 채널" 하위 메뉴(안정 / 카나리, 고른 것에 체크). 실행 중에 `NSApp.mainMenu`에 더한다(업스트림 줄 0. 156의 메뉴 빌더가 나중에 메뉴를 다시 만들면 그 뒤에 다시 더한다 [미확인]).
- 사용자 기본값의 `SUFeedURL`: Sparkle 2는 피드 주소를 사용자 기본값에서 읽지 않는 것으로 안다 [미확인]. 시험이 `defaults write app.pegboard.browser SUFeedURL <다른 주소>`가 효과 없음을 확인한다. 효과가 있으면 업데이터가 시작 전에 그 키를 지운다.
- 데몬과의 관계: 업데이트는 종료 때 설치되므로 실행 중인 데몬과 세션을 끊지 않는다. 사람이 "설치 후 다시 시작"을 고르면 보통 종료와 같다: `DaemonLauncher`가 데몬을 끝내고(SIGTERM, 기다림, 3a) 브라우저가 끝난 뒤 Sparkle이 번들을 바꾸고 새 앱을 켠다. 새 앱의 `DaemonLauncher`가 새 데몬을 띄운다. 옛 데몬이 잠금을 쥔 채 남아 있으면(종료 코드 4) 실패 조건이므로 시험이 잰다(5절 위험 4).

### 4.7 도움말 화면의 업데이트 줄 (R19)

- 3b 확인에서 `chrome://settings/help`에 "업데이트 확인 중 오류"가 보였다. 맥의 `VersionUpdater`(`chrome/browser/ui/webui/help/version_updater_mac.mm` 근처)가 구글 업데이터 없이 실패를 알리는 것으로 본다 [추정].
- 고르는 순서: (1) 그 상태를 "사용 안 함"으로 만드는 기존 스위치·정책·빌드 인자가 있으면 그것(업스트림 줄 0). (2) 없으면 맥의 `VersionUpdater::Create`가 `//pegboard/updater`의 구현(언제나 `DISABLED`)을 돌려주게 하는 패치 `pegboard-about-update.patch`, **10줄 이하**. 넘으면 멈추고 보고한다.
- 시험: 도움말 화면 본문에 "업데이트 확인 중 오류"(한국어)와 "error"(영어) 줄이 없다.

### 4.8 pegboard 홈 나누기 (R17)

- 지금 둘 다 `~/Library/Application Support/Pegboard`다: 포크의 `Default/`, `Local State`, `PegboardAgent*/`, `Crashpad/`와 pegboard의 `sessions/`, `recordings/`, `chrome/`(순정 모드의 사용자 데이터), `agent-browser-home/`, `state.json`, `config.toml`, `daemon.log`, `daemon.lock`, `pegboardd.sock`이 한 디렉터리에 있다. 이름은 겹치지 않지만 브라우저 쪽 정리(프로필 지우기, 사용자 데이터 초기화)와 업로드 거부 목록이 둘을 구분할 수 없다.
- 새 기본 홈 `~/Library/Application Support/Pegboard Agent`(`paths::paths`). 소켓 경로 길이 제한(103바이트)은 홈 디렉터리가 보통 길이면 맞는다(지금처럼 넘으면 오류).
- **옮기기**(`paths::migrate_legacy_home`, CLI와 데몬이 홈을 만들기 전에 부른다): `PEGBOARD_HOME`이 없고, 새 홈이 없고, 옛 위치에 `state.json`·`config.toml`·`recordings/`·`sessions/` 중 하나가 있을 때만. 옛 위치의 `daemon.lock`을 잡고(못 잡으면 옛 데몬이 도는 것이므로 옮기지 않고 `browser_failed` "an older pegboard daemon is running; quit Pegboard and try again", 데몬은 종료 코드 4), 새 홈을 0700으로 만들고, 정해진 이름(`sessions`, `recordings`, `chrome`, `agent-browser-home`, `state.json`, `config.toml`, `daemon.log`)을 하나씩 `rename`한다(같은 볼륨이라 각자 원자적). `pegboardd.sock`과 `daemon.lock`은 옮기지 않고 지운다. 다른 이름(브라우저의 것)은 건드리지 않는다. 마지막에 옛 위치에 `pegboard-home-moved.txt`(새 위치 안내 한 줄)를 남긴다.
- 업로드 거부 목록(`Paths::upload_forbidden`)은 새 홈과 브라우저 사용자 데이터 디렉터리, 그 둘의 캐시 짝을 모두 담는다.

### 4.9 확인 창 모양과 배포 빌드의 시험 훅 (R16, R18)

- 확인 창(`//pegboard/ui/confirm_dialog.cc`): 기본 버튼(거부)에 `views::MdTextButton`의 강조 모양(156의 `ui::ButtonStyle::kProminent` 계열, 이름은 트리에서)을 직접 건다. 잠긴 동안 세 버튼은 `SetEnabled(false)`에 더해 흐린 글자색(또는 투명도)을 직접 지정하고, 풀리면 되돌린다. 시험 훅 기록에 `"defaultButtonStyle"`과 잠긴 동안·풀린 뒤의 버튼 모양 값을 더하고, 화면은 시험 기기에서 사람이(또는 agent-desktop이) 본다.
- 데몬 포기 알림의 "다시 시작"도 같은 기본 버튼 규칙인지 본다(3b: 닫기가 기본). 같은 문제면 같이 고친다.
- `DaemonLauncher::DaemonEnvironment`: `BUILDFLAG(PEGBOARD_TEST_HOOKS)`가 거짓인 빌드에서는 브라우저 환경과 상관없이 `PEGBOARD_TEST_HOOKS`와 모든 `PEGBOARD_TEST_*`를 지운다.

### 4.10 시험과 리허설

- **이 기기(브라우저 없음)**: Rust(홈 옮기기, 버전, 시험 훅 끄기, 심볼릭 링크로 부른 CLI), 파이썬(appcast의 더하기·올리기·지우기·거부, 버전 계산), 셸(`shellcheck`, 배포 인자 비교, `release.sh`의 인자·단계 검사를 서명 없이 도는 부분).
- **개발 기기, 서명과 공증**: 실제 서명 ID로 서명, `codesign`/`spctl` 검증, 실제 공증. 브라우저는 켜지 않는다.
- **시험 기기, 개발 빌드(시험 훅 있음)**: 3b의 포크·순정 전체(퇴행), 업데이터의 시험 훅 시험, 확인 창 모양.
- **시험 기기, 배포 빌드**: 깃허브 릴리스에서 받은 DMG로 실제 설치(격리 속성이 붙은 채), Gatekeeper, 키체인(R15), `AperitifHelpers`(R14), 시험 훅이 없음, 배포 빌드 시험 묶음 `e2e_release`.
- **리허설 피드(거부 시험)**: 운영 피드와 다른 `SUFeedURL`(`…/rehearsal/appcast.xml`)을 넣어 묶은 같은 빌드를 시험 기기에 설치하고, 리허설 피드에 나쁜 항목을 하나씩 올린다: EdDSA 서명이 다른 키, EdDSA는 맞지만 임시 서명한 앱, 더 낮은 버전, 피드 서명이 틀림. 앱은 넷 다 받아들이지 않아야 한다. 리허설 DMG는 이름이 `rehearsal-`로 시작하는 프리릴리스에만 두고 끝나면 지운다. `appcast.py add`는 운영 피드에 리허설 빌드를 넣지 않는다(4.5).
- **끝의 증명(canary 왕복)**: 시험 기기에 canary `N` 설치 → 채널을 카나리로 → 개발 기기에서 canary `N+1` 게시 → "업데이트 확인…" → 설치 후 다시 시작 → 새 `CFBundleVersion`, 새 데몬의 `version`, 세션이 만들어지고 페이지가 열림, 키체인 창 없음, 사람 프로필의 쿠키·북마크가 남음. 자동 경로(내려받아 두었다가 종료 때 설치)도 한 번.
- 모든 C++ 변경에는 변형 확인("이 줄을 이렇게 바꾸면 시험 X가 실패한다")을 붙인다(3a, 3b와 같다).

### 4.11 빌드와 예산

- 개발 기기의 Chromium 트리(`156.0.8078.109`, 브랜치 `pegboard-work`, 생성 커밋 `rebrand-strings (generated)`가 맨 끝), `browser/scripts/{apply,export,build,package-dev,budget}.sh`. 빌드는 언제나 `-j 8`, 떼어 돌리고 끝을 기다린다. `out/Release`의 첫 빌드는 빈 상태라 약 2시간 45분이고 계획의 앞쪽(작업 3)에서 시작해 다른 작업과 겹쳐 돌린다. 그 뒤의 `//pegboard` 변경은 두 산출 디렉터리 모두 증분(1분 안팎)이다.
- 개발 기기에서 브라우저를 켜지 않는다. 앱은 시험 기기에서만 켠다.
- 업스트림 손 패치: 3b 끝 15개 파일, 120줄. 3c 증가 목표 **15줄 이하**(예상: 도움말 화면 패치 10줄 이하, 나머지 0). 디스크: `out/Release` 약 10 GB 더.

## 5. 위험과 측정할 것

1. **제한 엔타이틀먼트.** 트리의 앱 엔타이틀먼트에 프로비저닝 프로필이 있어야 하는 것(패스키 플랫폼 인증기 같은 것)이 있으면 Developer ID 앱이 실행되지 않는다 [미확인]. 빼고 서명해 시험 기기에서 켜 보고, 빠진 기능을 적는다.
2. **트리의 서명 도구가 Chromium 이름 빌드에 맞나.** 구글 설정만 있거나 이름이 고정되어 있으면 접근 방식 B로 간다(작업 6에서 정한다).
3. **키체인(R15).** 업데이트 뒤에도 묻으면 배포를 멈추고 원인(지정 요구 사항, `codesign -d -r-`)을 본다.
4. **업데이트 뒤 데몬.** Sparkle이 새 앱을 켤 때 옛 데몬이 아직 잠금을 쥐고 있으면 새 앱이 데몬 없이 뜬다(종료 코드 4, 알림도 없다). 3a의 종료 정리는 브라우저가 끝나기 전에 데몬을 기다리지만 Sparkle 경로에서 재 본 적이 없다. 그렇게 되면 `DaemonLauncher`가 시작 때 잠금이 "끝나는 중인 데몬"에게 있으면 짧게(10초) 기다리게 고친다.
5. **세션 복원.** Sparkle의 다시 시작은 Chromium의 다시 시작이 아니어서 탭이 복원되지 않을 수 있다. 4.6의 값으로 고치고 시험이 탭 복원을 본다.
6. **메뉴 항목.** Chromium이 실행 중에 앱 메뉴를 다시 만들면 더한 항목이 사라진다 [미확인]. 시험 훅 기록으로 메뉴를 본다.
7. **공증 거부.** 서명 안 된 파일, 타임스탬프 없는 파일, hardened runtime 없는 실행 파일이 하나라도 있으면 거부된다. `notarytool log`의 목록으로 고친다.
8. **Pages 캐시.** 푸시 뒤 몇 분 옛 appcast가 보인다. 게시가 다시 받아 비교하며 기다린다. 시험은 그 뒤에 확인한다.
9. **시험 기기의 화면.** 화면이 잠기면 시험 대부분이 실패하고, 창 복원으로 남은 Pegboard가 키체인 창을 띄울 수 있다. 시험 전에 사용자에게 화면 잠금을 풀어 달라고 하고 남은 인스턴스를 끈다.

## 6. 만드는 쪽에 맡긴 것과 정한 값

- 맡긴 것: 서명 도구의 정확한 부름(접근 방식 A/B), 빼는 엔타이틀먼트의 목록, 도움말 화면을 숨기는 정확한 자리, 확인 창의 강조·흐린 모양의 정확한 API, Sparkle 고정 판(2.9 이상 중 2주 이상 지난 가장 새 것), DMG 창 배치(배경 그림은 두지 않는다).
- 정한 값(제안): 빌드 번호 `N`은 1부터, 보이는 버전 `<Chromium>-pb.<N>`, `CFBundleVersion` `<BUILD>.<PATCH>.<N>`, 확인 주기 86400초, 공증 프로필 `pegboard-notary`, 채널 이름 `canary`, 사용자 기본값 `PegboardUpdateChannel`, appcast 항목 20개, Pages 다시 받기 10분, 새 홈 `Pegboard Agent`, 도움말 패치 10줄, 3c 손 패치 15줄, `-j 8`.

## 7. 끝나는 조건

- 2절의 목표가 모두 시험이나 기록으로 고정되어 있고, canary 왕복과 리허설 거부 시험이 시험 기기에서 통과했다.
- 3b의 포크·순정 전체가 시험 기기에서 다시 통과한다(퇴행 없음).
- 모든 C++ 변경에 변형 확인이 있고, 작업 6, 8, 9는 보안 검토를 받았다.
- R14, R15의 측정, 패치별 업스트림 줄 수, 배포 빌드 시간, 서명·공증·게시 시간, DMG 크기를 기록했다.
- README(설치, CLI를 `PATH`에, MCP 설정, 채널, 업데이트), `browser/README.md`(배포 빌드, `release.sh`, 사용자가 한 번 할 일), architecture.md(5절 파이프라인과 패치 표, 11절 남는 위험), roadmap.md(할 일 8, 9, 진행 현황)를 고쳤다.

## 8. 남는 위험

설계 때 보인 것:
- **EdDSA 비밀 키와 서명 ID가 개발 기기 하나에 있다.** 기기를 잃으면 백업이 없는 한 설치된 앱에 업데이트를 보낼 수 없다(사람이 DMG로 다시 설치해야 한다). 키가 새면 피드 주인(깃허브 계정)까지 빼앗긴 공격자는 서명된 appcast와 업데이트를 만들 수 있다. 팀 서명 확인이 둘째 벽이지만 그것도 같은 기기에 있다.
- **업데이트 멈추기(freeze).** 공격자가 피드를 막거나 옛 서명 피드를 되풀이하면 앱은 새 버전을 모른다. 내리기는 안 되지만 머무르게 할 수는 있다. 구버전 경고(3d)가 대응이다.
- **깃허브 계정.** 저장소와 Pages, 릴리스를 쥔 계정이 뚫리면 피드를 지울 수 있다(서명 없이는 바꿀 수 없다). 계정의 2단계 인증은 사용자가 지킨다.
- **DMG에서 바로 켠 앱**은 앱 이동 보호(App Translocation)로 임시 경로에서 돌고 Sparkle이 업데이트하지 못한다. 안내하지 않는다.
- **순정 모드의 데몬**(설치한 앱 안의 `pegboard`를 CLI로 띄운 것)은 업데이트가 번들을 바꿔도 옛 실행 파일로 계속 돈다. 다음 시작까지 agent-browser 판이 어긋날 수 있다.
- **세션은 업데이트를 넘지 않는다.** 다시 켜면 에이전트 세션이 모두 끝난다(데몬 재시작과 같다).
- **인텔 맥**에서는 앱이 돌지 않는다.
