#!/usr/bin/env bash
# notices.sh <out-dir>: puts the license notices the app carries in <out-dir>/Notices/
# (the release copies that directory to Contents/Resources/Notices, design R22):
#   Pegboard-LICENSE.txt   this repository's LICENSE (MIT)
#   agent-browser/         every license file of the agent-browser package
#                          (vendor/agent-browser/licenses/, from scripts/fetch-agent-browser.sh)
#   Sparkle/LICENSE        Sparkle's (vendor/sparkle/LICENSE, from scripts/fetch-sparkle.sh)
#   README.txt             where Chromium's notices are (chrome://credits, built into the app)
# Fails, leaving no Notices/, when any of them is missing.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

die() {
  echo "error: $*" >&2
  exit 1
}

[ $# -eq 1 ] && [ -n "$1" ] || die "usage: $(basename "$0") <out-dir>"
[ -d "$1" ] || die "$1 is not a directory"
out="$(cd "$1" && pwd)/Notices"

license="$ROOT/LICENSE"
ab_licenses="$ROOT/vendor/agent-browser/licenses"
sparkle_license="$ROOT/vendor/sparkle/LICENSE"
[ -f "$license" ] || die "no $license"
[ -d "$ab_licenses" ] && [ -n "$(find "$ab_licenses" -type f -print -quit)" ] ||
  die "no license files in $ab_licenses; run scripts/fetch-agent-browser.sh --locked"
[ -f "$ab_licenses/LICENSE" ] || die "no $ab_licenses/LICENSE (agent-browser's Apache-2.0)"
[ -f "$sparkle_license" ] || die "no $sparkle_license; run scripts/fetch-sparkle.sh"

stage="$out.new"
rm -rf "$stage"
mkdir -p "$stage/Sparkle"
cp "$license" "$stage/Pegboard-LICENSE.txt"
cp -R "$ab_licenses" "$stage/agent-browser"
cp "$sparkle_license" "$stage/Sparkle/LICENSE"
cat > "$stage/README.txt" << 'TEXT'
Chromium과 Chromium에 들어 있는 제3자 소프트웨어의 라이선스 고지는 앱 주소창에 chrome://credits 를 열면 볼 수 있습니다.
The license notices of Chromium and the third-party software in it are at chrome://credits (open it in the app's address bar).
TEXT
chmod -R u=rwX,go=rX "$stage"
rm -rf "$out"
mv "$stage" "$out"
echo "notices in $out"
