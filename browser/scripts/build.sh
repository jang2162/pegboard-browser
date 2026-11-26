#!/usr/bin/env bash
# build.sh <src-dir> [dev|release]: copies the kind's GN arguments (dev: args.gn -> out/Pegboard,
# the default; release: args.release.gn -> out/Release) into the build directory, runs gn gen and
# builds the chrome target, then prints how long it took. Runs at low priority with
# PEGBOARD_BUILD_JOBS parallel jobs (default and most: 8, more has hung the build machine).
set -euo pipefail
. "$(dirname "$0")/lib.sh"

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: build.sh <src-dir> [dev|release]"
src="$(src_dir_arg "$1")"
kind="${2:-dev}"
out_dir="$(out_dir_for "$kind")"
args="$(args_for "$kind")"
[ -f "$args" ] || die "$args does not exist"
jobs="${PEGBOARD_BUILD_JOBS:-$MAX_BUILD_JOBS}"
echo "$jobs" | grep -Eq '^[1-9][0-9]*$' || die "PEGBOARD_BUILD_JOBS must be a positive number"
[ "$jobs" -le "$MAX_BUILD_JOBS" ] ||
  die "PEGBOARD_BUILD_JOBS=$jobs is above $MAX_BUILD_JOBS (more has hung the build machine)"
use_depot_tools

mkdir -p "$src/$out_dir"
if ! cmp -s "$args" "$src/$out_dir/args.gn"; then
  cp "$args" "$src/$out_dir/args.gn"
fi

start="$(date +%s)"
(cd "$src" && gn gen "$out_dir")
(cd "$src" && nice -n 10 autoninja -j "$jobs" -C "$out_dir" chrome)
elapsed=$(($(date +%s) - start))
printf '== built in %dh %02dm %02ds (%d s, -j %s, %s)\n' \
  $((elapsed / 3600)) $((elapsed % 3600 / 60)) $((elapsed % 60)) "$elapsed" "$jobs" "$kind"
app="$src/$out_dir/Pegboard.app"
[ -d "$app" ] || die "the build has no $app (is the branding patch applied?)"
echo "== $app"
