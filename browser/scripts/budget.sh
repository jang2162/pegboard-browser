#!/usr/bin/env bash
# budget.sh <src-dir>: how much of upstream Chromium the work branch changes. Sums the lines
# added and deleted against the CHROMIUM_VERSION tag in files outside //pegboard.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
tag="$(chromium_tag)"
tag_commit="$(git -C "$src" rev-parse -q --verify "$tag^{commit}")" ||
  die "tag $tag is not in $src"
git -C "$src" diff --numstat "$tag_commit" HEAD -- . ':(exclude)pegboard' |
  awk '{ files++; add += $1; del += $2 }
       END { printf "upstream files %d, added %d, deleted %d, total %d\n",
             files, add, del, add + del }'
