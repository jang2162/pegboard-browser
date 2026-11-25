#!/usr/bin/env bash
# export.sh <src-dir>: writes the pegboard-work branch back into browser/.
#   - Each commit after the tag except the "pegboard" one and the generated
#     "rebrand-strings (generated)" one (apply.sh makes it again) becomes patches/<subject>
#     (git format-patch), in commit order, and patches/series lists them in that order.
#     Patch files no longer on the branch are removed.
#   - The committed //pegboard is copied back: pegboard/branding to browser/branding/, the rest
#     to browser/pegboard/.
# Fold fixup commits first (git rebase -i --autosquash). Refuses a dirty tree, a commit whose
# subject is not a patch file name, a patch commit that touches //pegboard, and a "pegboard"
# commit that touches anything else.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
tag="$(chromium_tag)"
tag_commit="$(git -C "$src" rev-parse -q --verify "$tag^{commit}")" ||
  die "tag $tag is not in $src"
require_clean "$src"
[ "$(git -C "$src" branch --show-current)" = "$WORK_BRANCH" ] ||
  die "$src is not on $WORK_BRANCH"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/patches" "$tmp/tree"
: > "$tmp/series"

for c in $(git -C "$src" rev-list --reverse "$tag_commit..HEAD"); do
  subject="$(git -C "$src" log -1 --format=%s "$c")"
  files="$(git -C "$src" diff-tree --no-commit-id --name-only -r "$c")"
  if [ "$subject" = "$PEGBOARD_SUBJECT" ]; then
    if echo "$files" | grep -qv '^pegboard/'; then
      die "commit $c ($PEGBOARD_SUBJECT) touches files outside pegboard/"
    fi
    continue
  fi
  if [ "$subject" = "$GENERATED_SUBJECT" ]; then
    if echo "$files" | grep -Evq '\.(grd|grdp|xtb)$'; then
      die "commit $c ($GENERATED_SUBJECT) touches files other than strings"
    fi
    [ "$c" = "$(git -C "$src" rev-parse HEAD)" ] ||
      die "commit $c ($GENERATED_SUBJECT) is not the last commit (move patches below it)"
    echo "== skipped $GENERATED_SUBJECT"
    continue
  fi
  echo "$subject" | grep -Eq '^[A-Za-z0-9._-]+\.patch$' ||
    die "commit $c has subject \"$subject\", not a patch file name (fold fixups first)"
  [ ! -e "$tmp/patches/$subject" ] || die "two commits are named $subject"
  if echo "$files" | grep -q '^pegboard/'; then
    die "commit $c ($subject) touches pegboard/; put new sources in the $PEGBOARD_SUBJECT commit"
  fi
  # Pinned so the patch bytes do not depend on the local git configuration.
  git -C "$src" -c diff.noprefix=false -c diff.mnemonicPrefix=false -c diff.renames=true \
    -c core.abbrev=12 -c diff.algorithm=myers \
    format-patch -1 --stdout --no-numbered --zero-commit --no-signature "$c" \
    > "$tmp/patches/$subject"
  echo "$subject" >> "$tmp/series"
  echo "== exported $subject"
done

# Patches.
for old in "$BROWSER_DIR"/patches/*.patch; do
  [ -e "$old" ] || continue
  [ -e "$tmp/patches/$(basename "$old")" ] || { rm "$old"; echo "== removed $(basename "$old")"; }
done
for p in "$tmp"/patches/*.patch; do
  [ -e "$p" ] || continue
  cp "$p" "$BROWSER_DIR/patches/"
done
cp "$tmp/series" "$BROWSER_DIR/patches/series"

# //pegboard as committed.
git -C "$src" archive HEAD pegboard | tar -x -C "$tmp/tree"
mkdir -p "$tmp/tree/pegboard/branding"
rsync -a --delete "$tmp/tree/pegboard/branding/" "$BROWSER_DIR/branding/"
rsync -a --delete --exclude /branding "$tmp/tree/pegboard/" "$BROWSER_DIR/pegboard/"
echo "== copied //pegboard to browser/pegboard and browser/branding"
