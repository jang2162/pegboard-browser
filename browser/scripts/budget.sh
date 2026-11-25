#!/usr/bin/env bash
# budget.sh <src-dir>: how much of upstream Chromium the work branch changes. Sums the lines
# added and deleted in files outside //pegboard, separately for
#   - the hand-written patches (the tag against the commit before the generated one), and
#   - the generated "rebrand-strings (generated)" commit (apply.sh makes it; not a patch).
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
tag="$(chromium_tag)"
tag_commit="$(git -C "$src" rev-parse -q --verify "$tag^{commit}")" ||
  die "tag $tag is not in $src"

sum() {
  git -C "$src" diff --numstat "$1" "$2" -- . ':(exclude)pegboard' |
    awk -v what="$3" '{ files++; add += $1; del += $2 }
         END { printf "%s: upstream files %d, added %d, deleted %d, total %d\n",
               what, files, add, del, add + del }'
}

head="$(git -C "$src" rev-parse HEAD)"
if [ "$(git -C "$src" log -1 --format=%s HEAD)" = "$GENERATED_SUBJECT" ]; then
  sum "$tag_commit" "$head^" "patches"
  sum "$head^" "$head" "generated"
else
  for c in $(git -C "$src" rev-list "$tag_commit..HEAD"); do
    [ "$(git -C "$src" log -1 --format=%s "$c")" != "$GENERATED_SUBJECT" ] ||
      die "$GENERATED_SUBJECT is not the last commit (move patches below it)"
  done
  sum "$tag_commit" "$head" "patches"
  echo "generated: none ($GENERATED_SUBJECT is not on the branch)"
fi
