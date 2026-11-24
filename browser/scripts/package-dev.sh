#!/usr/bin/env bash
# package-dev.sh <src-dir> <out.app>: a development Pegboard.app. Builds pegboard (release),
# copies the built Pegboard.app to <out.app>, puts pegboard and agent-browser in
# Contents/Helpers/, and signs the bundle ad hoc.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

[ $# -eq 2 ] || die "usage: package-dev.sh <src-dir> <out.app>"
src="$(src_dir_arg "$1")"
out="$2"
app="$src/$OUT_DIR/Pegboard.app"
[ -d "$app" ] || die "$app does not exist (run build.sh)"
case "$out" in
  *.app) ;;
  *) die "<out.app> must end in .app" ;;
esac
out_parent="$(mkdir -p "$(dirname "$out")" && cd "$(dirname "$out")" && pwd -P)"
[ "$out_parent/$(basename "$out")" != "$(cd "$app" && pwd -P)" ] ||
  die "<out.app> is the build's own app; give another path"

(cd "$REPO_ROOT" && cargo build --release -p pegboard-cli)
agent_browser="$REPO_ROOT/vendor/agent-browser/agent-browser"
[ -x "$agent_browser" ] || "$REPO_ROOT/scripts/fetch-agent-browser.sh"

rm -rf "$out"
mkdir -p "$(dirname "$out")"
ditto "$app" "$out"
mkdir -p "$out/Contents/Helpers"
install -m 0755 "$REPO_ROOT/target/release/pegboard" "$out/Contents/Helpers/pegboard"
install -m 0755 "$agent_browser" "$out/Contents/Helpers/agent-browser"
codesign --force --deep -s - "$out"
echo "== $out"
