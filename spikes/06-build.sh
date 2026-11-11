#!/bin/bash
# Spike 06 (A10): fetch Chromium at the current Extended Stable tag, build it once, and time every phase.
# Throwaway. Source lives outside this repo in ~/chromium. Runs at low priority.
set -u
TAG="${TAG:-156.0.8078.96}"
ROOT="$HOME/chromium"
DEPOT="$HOME/depot_tools"
LOG="$(cd "$(dirname "$0")" && pwd)/out/06-build-log.txt"
mkdir -p "$(dirname "$LOG")"
export PATH="$PATH:$DEPOT"
export DEPOT_TOOLS_UPDATE=1

phase() { # phase <name> <command...>
  local name="$1"; shift
  local start; start=$(date +%s)
  echo "== $name: start $(date '+%F %T')" >> "$LOG"
  "$@" >> "$LOG.$name.out" 2>&1
  local code=$?
  echo "== $name: exit $code, $(( $(date +%s) - start )) s, free $(df -g "$HOME" | tail -1 | awk '{print $4}') GB" >> "$LOG"
  return $code
}

echo "# tag $TAG, started $(date '+%F %T'), free $(df -g "$HOME" | tail -1 | awk '{print $4}') GB" >> "$LOG"
[ -d "$DEPOT" ] || phase depot_tools git clone https://chromium.googlesource.com/chromium/tools/depot_tools.git "$DEPOT" || exit 1
mkdir -p "$ROOT" && cd "$ROOT" || exit 1
[ -d src ] || phase fetch nice -n 10 caffeinate -i fetch --nohooks --no-history chromium || exit 1
cd src || exit 1
git rev-parse -q --verify "refs/tags/$TAG" > /dev/null || phase tag_fetch git fetch --depth 1 origin "refs/tags/$TAG:refs/tags/$TAG" || exit 1
phase tag_checkout git checkout -f "$TAG" || exit 1
# The anonymous googlesource quota returns HTTP 429 when many repos are fetched at once: go slower and retry.
synced=1
for attempt in 1 2 3 4 5 6 7 8; do
  if phase "sync$attempt" nice -n 10 caffeinate -i gclient sync -D --no-history -j 3; then synced=0; break; fi
  echo "== sync attempt $attempt failed, waiting 300 s" >> "$LOG"
  sleep 300
done
[ $synced -eq 0 ] || exit 1
echo "== src size: $(du -sh "$ROOT/src" | cut -f1)" >> "$LOG"
phase gn_gen gn gen out/Default --args='is_debug=false symbol_level=0 is_component_build=false target_cpu="arm64"' || exit 1
phase build nice -n 10 caffeinate -i autoninja -C out/Default chrome || exit 1
echo "== out size: $(du -sh "$ROOT/src/out/Default" | cut -f1)" >> "$LOG"
echo "# finished $(date '+%F %T')" >> "$LOG"
