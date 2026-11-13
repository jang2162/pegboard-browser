#!/usr/bin/env bash
# Checks that real-browser tests do not depend on the order they run in (they share one
# browser per test binary). Runs on a machine that may launch a browser.
#
#   scripts/test-reverse.sh <package> <test-binary>            # one test per process, last test first
#   scripts/test-reverse.sh <package> <test-binary> shuffle N  # the whole binary in one process, in the order of seed N
#
# Example: scripts/test-reverse.sh pegboard-core e2e_actionlog
#
# Mode 1 runs each `#[ignore]` test alone (name filter, exact match) in reverse name order, so a
# test that only passes after another one has run fails here. Mode 2 keeps the shared browser
# and changes the order: the default run is by name, so state a test leaves behind reaches the
# tests after it by name; a seed that puts them the other way round finds the opposite
# dependence. Mode 2 uses the test harness's `--shuffle`, which needs RUSTC_BOOTSTRAP=1 on a
# stable toolchain (set below).
set -euo pipefail

pkg=${1:?usage: test-reverse.sh <package> <test-binary> [shuffle SEED]}
bin=${2:?usage: test-reverse.sh <package> <test-binary> [shuffle SEED]}
mode=${3:-reverse}

cd "$(dirname "$0")/.."

if [ "$mode" = shuffle ]; then
  seed=${4:?shuffle needs a seed}
  RUSTC_BOOTSTRAP=1 cargo test -p "$pkg" --test "$bin" -- --ignored --test-threads=1 \
    -Zunstable-options --shuffle-seed "$seed"
  exit
fi

tests=$(cargo test -p "$pkg" --test "$bin" -- --ignored --list 2>/dev/null \
  | sed -n 's/^\(.*\): test$/\1/p' | sort -r)
[ -n "$tests" ] || { echo "no ignored tests in $pkg $bin" >&2; exit 1; }

failed=()
for t in $tests; do
  echo "=== $t"
  if ! cargo test -p "$pkg" --test "$bin" -- --ignored --exact --test-threads=1 "$t"; then
    failed+=("$t")
  fi
done
if [ ${#failed[@]} -gt 0 ]; then
  echo "FAILED when run alone: ${failed[*]}" >&2
  exit 1
fi
echo "all $(echo "$tests" | wc -l | tr -d " ") ok"
