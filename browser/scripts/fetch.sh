#!/usr/bin/env bash
# fetch.sh <src-dir>: makes <src-dir> a Chromium checkout at the CHROMIUM_VERSION tag with its
# dependencies synced. Gets depot_tools and the source when missing; on an existing tree it
# only fetches the tag if needed, moves a tree that is not on that tag to it, and runs
# `gclient sync -D`. A pegboard-work branch built on the tag is left where it is.
# googlesource.com rate-limits anonymous fetches (HTTP 429), so syncs run with few jobs and
# are retried.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

[ $# -eq 1 ] || die "usage: fetch.sh <src-dir>"
tag="$(chromium_tag)"
src="$1"

if ! command -v gclient > /dev/null 2>&1; then
  dt="${DEPOT_TOOLS:-$HOME/depot_tools}"
  if [ ! -x "$dt/gclient" ]; then
    echo "== fetching depot_tools into $dt"
    git clone https://chromium.googlesource.com/chromium/tools/depot_tools.git "$dt"
  fi
fi
use_depot_tools
export DEPOT_TOOLS_UPDATE=1

if [ ! -d "$src/.git" ]; then
  [ "$(basename "$src")" = "src" ] || die "a new checkout must be named src (got $src)"
  parent="$(dirname "$src")"
  mkdir -p "$parent"
  [ ! -e "$parent/.gclient" ] || die "$parent/.gclient exists but $src is not a checkout"
  echo "== fetching Chromium into $parent (takes a while)"
  (cd "$parent" && nice -n 10 fetch --nohooks --no-history chromium)
fi
src="$(cd "$src" && pwd)"

if ! git -C "$src" rev-parse -q --verify "refs/tags/$tag" > /dev/null; then
  echo "== fetching tag $tag"
  git -C "$src" fetch --depth 1 origin "refs/tags/$tag:refs/tags/$tag"
fi
tag_commit="$(git -C "$src" rev-parse "$tag^{commit}")"
head_commit="$(git -C "$src" rev-parse HEAD)"
if [ "$head_commit" = "$tag_commit" ]; then
  echo "== $src is at $tag"
elif git -C "$src" merge-base --is-ancestor "$tag_commit" HEAD &&
  [ "$(git -C "$src" branch --show-current)" = "$WORK_BRANCH" ]; then
  echo "== $src is on $WORK_BRANCH over $tag; leaving it"
else
  require_clean "$src"
  echo "== checking out $tag"
  git -C "$src" checkout --detach "$tag_commit"
fi

for attempt in 1 2 3 4 5 6; do
  echo "== gclient sync (attempt $attempt)"
  if (cd "$src" && nice -n 10 gclient sync -D --no-history --jobs 4); then
    echo "== synced $src at $tag"
    exit 0
  fi
  [ "$attempt" -eq 6 ] || { echo "== sync failed; retrying in 300 s"; sleep 300; }
done
die "gclient sync failed 6 times"
