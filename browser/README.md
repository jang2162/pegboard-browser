# browser/ — Pegboard 브라우저(Chromium 포크)

Chromium 소스는 이 저장소에 넣지 않는다. 여기에는 버전, 패치, 새 소스, 스크립트만 있고, 받아 둔 Chromium 트리(`<src>`, 보통 `.../chromium/src`)에 적용해 빌드한다. 설계는 [3a 설계 4.1](../docs/plans/2026-11-21-phase3a-fork-skeleton-design.md).

| 경로 | 내용 |
|---|---|
| `CHROMIUM_VERSION` | 고정한 Chromium 태그 한 줄 |
| `args.gn` | 개발 빌드 GN 인자(`<src>/out/Pegboard/args.gn`으로 복사된다) |
| `patches/series` | 패치 적용 순서. 한 줄에 패치 파일 이름 하나 |
| `patches/*.patch` | 업스트림 파일 수정. 패치 하나 = 작업 브랜치의 커밋 하나. 머리말에 목적과 건드리는 파일 |
| `pegboard/` | 트리의 `//pegboard/`로 복사되는 새 소스 |
| `branding/` | 아이콘(`mac/app.icns`, `mac/Assets.car`)과 그 생성 스크립트. 트리의 `//pegboard/branding/`으로 복사된다 |
| `scripts/` | 아래 스크립트 |

## 처음부터 빌드

```sh
browser/scripts/fetch.sh <src>      # depot_tools와 소스 받기(없으면), 태그로 맞추기, gclient sync -D
browser/scripts/apply.sh <src>      # 태그에서 pegboard-work 브랜치 만들기, //pegboard 복사, 패치 적용
browser/scripts/build.sh <src>      # gn gen + autoninja chrome → <src>/out/Pegboard/Pegboard.app
browser/scripts/package-dev.sh <src> <out>/Pegboard.app   # pegboard, agent-browser를 넣고 임시 서명
```

- 소스 받기는 30분 남짓(0단계 실측), 빈 상태 빌드는 `-j 8`로 2시간 반~3시간 걸린다(`docs/spike-findings.md` "3a 작업 7"). 디스크는 100GB 이상 비워 둔다. Xcode와 Metal 도구 모음이 있어야 한다.
- `build.sh`는 `nice`로, 동시 작업 8개로 돈다. 바꾸려면 `PEGBOARD_BUILD_JOBS=<n>`. 더 높이면 빌드 기계가 멈춘 적이 있다.
- `fetch.sh`는 이미 받은 트리에서 다시 받지 않는다. 태그가 없으면 그 태그만 받고, `pegboard-work`가 그 태그 위에 있으면 그대로 두고 `gclient sync`만 한다. 익명 요청 제한(HTTP 429) 때문에 `--jobs 4`로 돌고 실패하면 5분 뒤 다시 한다(6번까지).
- depot_tools는 `PATH`, `$DEPOT_TOOLS`, `~/depot_tools` 순으로 찾는다.
- `args.gn`에는 `enable_nacl`이 없다. 이 버전에서는 효과가 없다고 `gn`이 경고한다. 대신 `disable_fieldtrial_testing_config = true`가 있다. 이것이 없으면 Chromium 이름의 빌드가 `testing/variations/fieldtrial_testing_config.json`의 실험을 켜고, 그중 `AperitifHelpers`의 도우미 프로세스가 샌드박스를 못 열고 죽어 네트워크가 안 된다(작업 7에서 겪음).

## 패치 고치기

1. `apply.sh`가 만든 `pegboard-work`에서 고친다. 커밋 제목은 패치 파일 이름이다(`branding.patch`). 기존 패치를 고칠 때는 `git commit --fixup <그 커밋>` 뒤 `git rebase -i --autosquash <태그>`로 합친다. 새 패치는 제목이 `<이름>.patch`인 새 커밋으로 만든다.
2. 새 소스는 `<src>/pegboard/`에 두고 제목이 `pegboard`인 첫 커밋에 합친다. 패치 커밋은 `pegboard/`를 건드리지 않는다.
3. `browser/scripts/export.sh <src>`가 커밋을 `patches/`로 다시 쓰고 `series`를 커밋 순서로 갱신하며, `//pegboard`를 `browser/pegboard/`와 `browser/branding/`으로 되돌려 복사한다. 커밋하지 않은 변경이 있으면 거부한다.
4. 다시 적용해 확인: `PEGBOARD_APPLY_FORCE=1 browser/scripts/apply.sh <src>`(기존 `pegboard-work`를 태그에서 새로 만들고, 옛 것은 `pegboard-work-prev`로 남긴다).
5. 패치가 안 맞으면 `apply.sh`가 멈추고 남은 패치와 이어 가는 명령(`PEGBOARD_APPLY_FROM=<다음 패치>`)을 알려 준다. 고쳐 커밋한 뒤 그 명령으로 나머지를 얹고 `export.sh`로 되돌려 쓴다.

버전 올리기: `CHROMIUM_VERSION`을 바꾸고 `fetch.sh`, `PEGBOARD_APPLY_FORCE=1 apply.sh`. `--3way`가 쓰려면 패치의 원본 블롭(`index` 줄)이 트리에 있어야 하므로, 옛 태그도 받아 둔 트리(`--no-history`가 아닌, 또는 `git fetch origin refs/tags/<옛 태그>`)에서 한다.

`browser/scripts/budget.sh <src>`는 `//pegboard` 밖 업스트림 파일의 추가·삭제 줄 수 합계를 낸다(패치 예산).

## 아이콘

임시 아이콘(단색 사각형에 "P")이다. 3b에서 다시 만든다. 바꾸려면 `branding/icon.swift`를 고치고 `branding/make-icons.sh`를 돌린 뒤 결과 파일을 커밋한다.

## 확인

- 이름·번들 ID: `defaults read <앱>/Contents/Info CFBundleIdentifier` → `app.pegboard.browser`, `CFBundleName` → `Pegboard`.
- 기본 사용자 데이터: `~/Library/Application Support/Pegboard`.
- 파이프 위 CDP: `PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_BROWSER=fork:<앱> cargo test -p pegboard-core --test e2e_link -- --ignored --test-threads=1`.
- 기본 사용자 데이터 디렉터리는 `--user-data-dir`를 줘도 생긴다. 안에는 `Crashpad/`만 있다: 업스트림이 충돌 보고 위치(`DIR_CRASH_DUMPS`)를 기본 사용자 데이터 디렉터리 아래로 정한다.
- 새로 빌드한 앱이 처음 쿠키 저장소를 열 때 키체인 항목 `Pegboard Safe Storage`를 만들며 사용자에게 묻는다. 답할 때까지 모든 HTTP 요청이 멈춘다. 사람이 없는 시험은 `--use-mock-keychain`을 준다.
