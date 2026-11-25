#!/usr/bin/env bash
# apply.sh <src-dir>: builds the pegboard-work branch on the clean CHROMIUM_VERSION tag.
#   1. browser/pegboard/ is copied to <src>/pegboard/ and browser/branding/ to
#      <src>/pegboard/branding/, one commit (subject "pegboard").
#   2. Each patch in patches/series, in order, goes in with `git apply --3way`, one commit per
#      patch whose subject is the patch file name and whose body and author are the patch's.
#   3. scripts/rebrand-strings.py changes the product name in the strings, one commit (subject
#      "rebrand-strings (generated)"). export.sh does not turn it into a patch.
# An existing pegboard-work branch is not replaced unless PEGBOARD_APPLY_FORCE=1; the old one
# is kept as pegboard-work-prev.
# After a conflict: resolve it in <src>, commit it with the patch name as subject, then run
#   PEGBOARD_APPLY_FROM=<next patch> apply.sh <src-dir>
# to apply the rest of the series on the current pegboard-work without rebuilding it.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

src="$(src_dir_arg "$@")"
tag="$(chromium_tag)"
tag_commit="$(git -C "$src" rev-parse -q --verify "$tag^{commit}")" ||
  die "tag $tag is not in $src (run fetch.sh)"
require_clean "$src"
from="${PEGBOARD_APPLY_FROM:-}"
if [ -n "$from" ]; then
  [ "$(git -C "$src" branch --show-current)" = "$WORK_BRANCH" ] ||
    die "PEGBOARD_APPLY_FROM needs $src on $WORK_BRANCH"
  grep -qx "$from" < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$BROWSER_DIR/patches/series") ||
    die "PEGBOARD_APPLY_FROM=$from is not in patches/series"
elif git -C "$src" rev-parse -q --verify "refs/heads/$WORK_BRANCH" > /dev/null &&
  [ "${PEGBOARD_APPLY_FORCE:-}" != "1" ]; then
  die "$WORK_BRANCH exists in $src; set PEGBOARD_APPLY_FORCE=1 to rebuild it from $tag"
fi

# Commits made here carry a fixed committer; authors come from the patches.
g() {
  git -C "$src" -c user.name=Pegboard -c user.email=noreply@pegboard.invalid \
    -c commit.gpgsign=false "$@"
}

if [ -z "$from" ]; then
  if prev="$(git -C "$src" rev-parse -q --verify "refs/heads/$WORK_BRANCH")"; then
    g branch -f "$WORK_BRANCH-prev" "$prev"
    echo "== old $WORK_BRANCH kept as $WORK_BRANCH-prev ($prev)"
  fi
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
else
  echo "== continuing on $WORK_BRANCH from $from"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
names=()
while IFS= read -r line || [ -n "$line" ]; do
  name="${line%%#*}"
  name="$(echo "$name" | tr -d '[:space:]')"
  [ -n "$name" ] && names+=("$name")
done < "$BROWSER_DIR/patches/series"

skipping="${from:+1}"
for i in "${!names[@]}"; do
  name="${names[$i]}"
  if [ -n "$skipping" ]; then
    [ "$name" = "$from" ] || continue
    skipping=""
  fi
  file="$BROWSER_DIR/patches/$name"
  [ -f "$file" ] || die "series names $name but patches/$name does not exist"
  git -C "$src" mailinfo -k "$tmp/msg" "$tmp/diff" < "$file" > "$tmp/info"
  author="$(sed -n 's/^Author: //p' "$tmp/info")"
  email="$(sed -n 's/^Email: //p' "$tmp/info")"
  date="$(sed -n 's/^Date: //p' "$tmp/info")"
  if ! g apply --3way "$tmp/diff"; then
    echo "error: $name does not apply on $WORK_BRANCH ($tag)." >&2
    echo "  Make the change in $src by hand (conflict markers if --3way left any), then:
    git -C $src add -A && git -C $src commit -m $name" >&2
    rest=("${names[@]:$((i + 1))}")
    if [ ${#rest[@]} -gt 0 ]; then
      echo "  Patches not applied yet: ${rest[*]}" >&2
      echo "  Then: PEGBOARD_APPLY_FROM=${rest[0]} $0 $src" >&2
    fi
    echo "  Finally run export.sh to write the resolved patch back." >&2
    exit 1
  fi
  { echo "$name"; echo; cat "$tmp/msg"; } > "$tmp/commit-msg"
  GIT_AUTHOR_NAME="${author:-Pegboard}" GIT_AUTHOR_EMAIL="${email:-noreply@pegboard.invalid}" \
    GIT_AUTHOR_DATE="${date:-}" g commit -q --cleanup=strip -F "$tmp/commit-msg"
  echo "== applied $name"
done

python3 "$BROWSER_DIR/scripts/rebrand-strings.py" "$src"
g add -A -- chrome/app components
if g diff --cached --quiet; then
  die "rebrand-strings.py changed nothing"
fi
GIT_AUTHOR_NAME=Pegboard GIT_AUTHOR_EMAIL=noreply@pegboard.invalid \
  g commit -q -m "$GENERATED_SUBJECT" \
  -m "Made by browser/scripts/rebrand-strings.py; apply.sh makes it again, export.sh skips it."
echo "== $GENERATED_SUBJECT"
echo "== done: $(git -C "$src" rev-list --count "$tag_commit..HEAD") commits on $WORK_BRANCH"
