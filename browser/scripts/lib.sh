# Shared by the browser/ scripts (sourced, not run).
# shellcheck shell=bash
# The variables are for the scripts that source this file.
# shellcheck disable=SC2034

BROWSER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$BROWSER_DIR/.." && pwd)"
# The branch apply.sh builds on the version tag: one commit for //pegboard, then one per patch.
WORK_BRANCH="pegboard-work"
# The commit subject of the //pegboard copy (every other commit's subject is a patch file name).
PEGBOARD_SUBJECT="pegboard"
# The commit subject of the generated string change (rebrand-strings.py), the last commit.
GENERATED_SUBJECT="rebrand-strings (generated)"
# The development build directory under the Chromium tree (out_dir_for dev).
OUT_DIR="out/Pegboard"
# The most parallel jobs a build may use: more has hung the build machine (kernel panic).
MAX_BUILD_JOBS=8

die() {
  echo "error: $*" >&2
  exit 1
}

chromium_tag() {
  tr -d '[:space:]' < "$BROWSER_DIR/CHROMIUM_VERSION"
}

# Puts depot_tools on PATH: already there, $DEPOT_TOOLS, or ~/depot_tools.
use_depot_tools() {
  if command -v gclient > /dev/null 2>&1; then
    return 0
  fi
  local dir="${DEPOT_TOOLS:-$HOME/depot_tools}"
  [ -x "$dir/gclient" ] || die "depot_tools not found (set DEPOT_TOOLS or run fetch.sh)"
  export PATH="$PATH:$dir"
}

# The Chromium src directory given as the first argument, made absolute.
src_dir_arg() {
  [ $# -ge 1 ] && [ -n "$1" ] || die "usage: $(basename "$0") <chromium-src-dir> ..."
  [ -d "$1" ] || die "$1 is not a directory"
  (cd "$1" && pwd)
}

# Fails unless the tree has no uncommitted changes to tracked files and no untracked files
# under //pegboard (export.sh would skip them, apply.sh would delete them).
require_clean() {
  local src="$1" untracked
  if ! git -C "$src" diff --quiet || ! git -C "$src" diff --cached --quiet; then
    die "$src has uncommitted changes"
  fi
  untracked="$(git -C "$src" ls-files --others --exclude-standard -- pegboard)"
  if [ -n "$untracked" ]; then
    die "$src has untracked files under pegboard/ (commit or remove them): $(echo "$untracked" | head -5 | tr '\n' ' ')"
  fi
}

# The build directory under the Chromium tree for a build kind: dev (development and test,
# browser/args.gn) or release (the build for people, browser/args.release.gn).
out_dir_for() {
  case "$1" in
    dev) echo "$OUT_DIR" ;;
    release) echo "out/Release" ;;
    *) die "unknown build kind: $1 (dev or release)" ;;
  esac
}

# The GN arguments file for a build kind (see out_dir_for).
args_for() {
  case "$1" in
    dev) echo "$BROWSER_DIR/args.gn" ;;
    release) echo "$BROWSER_DIR/args.release.gn" ;;
    *) die "unknown build kind: $1 (dev or release)" ;;
  esac
}
