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
# The build directory under the Chromium tree.
OUT_DIR="out/Pegboard"

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
