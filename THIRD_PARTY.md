# 서드파티 소프트웨어

배포 앱(`Pegboard.app`)은 아래 둘을 함께 싣고, 그 고지를 `Contents/Resources/Notices/`에 둔다(`browser/release/notices.sh`). Chromium과 그 안의 제3자 코드의 고지는 앱의 `chrome://credits`에 있다(빌드가 만든다).

## agent-browser

- 버전: 0.42.0 (고정)
- 라이선스: Apache-2.0 (전문은 `vendor/agent-browser/LICENSE`, 받을 때 함께 내려받는다). 꾸러미 안의 다른 라이선스 파일(예: axe-core의 `LICENSE-axe-core.txt`, `LICENSE-axe-core-THIRD-PARTY.txt`)도 `vendor/agent-browser/licenses/`에 같은 경로로 받는다.
- 출처: https://registry.npmjs.org/agent-browser/-/agent-browser-0.42.0.tgz (npm 꾸러미 `agent-browser`의 `bin/agent-browser-darwin-arm64`)
- 받는 방법: `scripts/fetch-agent-browser.sh`. `vendor/agent-browser.lock`에 꾸러미(`.tgz`)와 실행 파일의 sha256을 한 줄씩 고정한다. 꾸러미는 풀기 전에 확인한다. 배포는 `--locked`로만 부르고, 그때 잠금 줄이 없으면 받은 값을 적지 않고 실패한다.

## Sparkle

- 버전: 2.10.1 (고정, `scripts/fetch-sparkle.sh`의 `SPARKLE_VERSION`)
- 라이선스: MIT와 그 안에 적힌 외부 라이선스들 (전문은 `vendor/sparkle/LICENSE`, 받을 때 함께 내려받는다)
- 출처: https://github.com/sparkle-project/Sparkle/releases/download/2.10.1/Sparkle-2.10.1.tar.xz
- 받는 방법: `scripts/fetch-sparkle.sh`. `vendor/sparkle.lock`에 압축 파일의 sha256 한 줄을 고정한다(깃허브 릴리스 페이지가 보이는 값과 같다). 잠금이 비어 있으면 값을 적고 실패한다(처음 고정은 사람이 보고 커밋한다). 앱에는 `Sparkle.framework`만 들어가고 `bin/`(`generate_keys`, `sign_update`)은 개발 기기에서만 쓴다.
