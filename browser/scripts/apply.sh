#!/usr/bin/env bash
# apply.sh <src-dir>: builds the pegboard-work branch on the clean CHROMIUM_VERSION tag.
#   1. browser/pegboard/ is copied to <src>/pegboard/ and browser/branding/ to
#      <src>/pegboard/branding/, one commit (subject "pegboard").
#   2. Each patch in patches/series, in order, goes in with `git apply --3way`, one commit per
#      patch whose subject is the patch file name and whose body and author are the patch's.
# An existing pegboard-work branch is not replaced unless PEGBOARD_APPLY_FORCE=1.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
tag="$(chromium_tag)"
tag_commit="$(git -C "$src" rev-parse -q --verify "$tag^{commit}")" ||
  die "tag $tag is not in $src (run fetch.sh)"
require_clean "$src"
if git -C "$src" rev-parse -q --verify "refs/heads/$WORK_BRANCH" > /dev/null &&
  [ "${PEGBOARD_APPLY_FORCE:-}" != "1" ]; then
  die "$WORK_BRANCH exists in $src; set PEGBOARD_APPLY_FORCE=1 to rebuild it from $tag"
fi

# Commits made here carry a fixed committer; authors come from the patches.
g() {
  git -C "$src" -c user.name=Pegboard -c user.email=noreply@pegboard.invalid \
    -c commit.gpgsign=false "$@"
}

g checkout -q -B "$WORK_BRANCH" "$tag_commit"
echo "== $WORK_BRANCH at $tag"

rm -rf "$src/pegboard"
mkdir -p "$src/pegboard/branding"
cp -R "$BROWSER_DIR/pegboard/." "$src/pegboard/"
cp -R "$BROWSER_DIR/branding/." "$src/pegboard/branding/"
g add -A pegboard
GIT_AUTHOR_NAME=Pegboard GIT_AUTHOR_EMAIL=noreply@pegboard.invalid \
  g commit -q -m "$PEGBOARD_SUBJECT"
echo "== $PEGBOARD_SUBJECT: copied browser/pegboard and browser/branding"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
while IFS= read -r line || [ -n "$line" ]; do
  name="${line%%#*}"
  name="$(echo "$name" | tr -d '[:space:]')"
  [ -n "$name" ] || continue
  file="$BROWSER_DIR/patches/$name"
  [ -f "$file" ] || die "series names $name but patches/$name does not exist"
  git -C "$src" mailinfo -k "$tmp/msg" "$tmp/diff" < "$file" > "$tmp/info"
  author="$(sed -n 's/^Author: //p' "$tmp/info")"
  email="$(sed -n 's/^Email: //p' "$tmp/info")"
  date="$(sed -n 's/^Date: //p' "$tmp/info")"
  if ! g apply --3way "$tmp/diff"; then
    die "$name does not apply to $tag; resolve in $src, commit as \"$name\", then run export.sh"
  fi
  { echo "$name"; echo; cat "$tmp/msg"; } > "$tmp/commit-msg"
  GIT_AUTHOR_NAME="${author:-Pegboard}" GIT_AUTHOR_EMAIL="${email:-noreply@pegboard.invalid}" \
    GIT_AUTHOR_DATE="${date:-}" g commit -q --cleanup=strip -F "$tmp/commit-msg"
  echo "== applied $name"
done < "$BROWSER_DIR/patches/series"
echo "== done: $(git -C "$src" rev-list --count "$tag_commit..HEAD") commits on $WORK_BRANCH"
