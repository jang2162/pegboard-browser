#!/usr/bin/env bash
# build.sh <src-dir>: copies args.gn to <src>/out/Pegboard, runs gn gen and builds the chrome
# target, then prints how long it took. Runs at low priority with PEGBOARD_BUILD_JOBS parallel
# jobs (default 8: more has hung the build machine).
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
use_depot_tools
jobs="${PEGBOARD_BUILD_JOBS:-8}"
echo "$jobs" | grep -Eq '^[1-9][0-9]*$' || die "PEGBOARD_BUILD_JOBS must be a positive number"

mkdir -p "$src/$OUT_DIR"
if ! cmp -s "$BROWSER_DIR/args.gn" "$src/$OUT_DIR/args.gn"; then
  cp "$BROWSER_DIR/args.gn" "$src/$OUT_DIR/args.gn"
fi

start="$(date +%s)"
(cd "$src" && gn gen "$OUT_DIR")
(cd "$src" && nice -n 10 autoninja -j "$jobs" -C "$OUT_DIR" chrome)
elapsed=$(($(date +%s) - start))
printf '== built in %dh %02dm %02ds (%d s, -j %s)\n' \
  $((elapsed / 3600)) $((elapsed % 3600 / 60)) $((elapsed % 60)) "$elapsed" "$jobs"
echo "== $src/$OUT_DIR/Pegboard.app"
