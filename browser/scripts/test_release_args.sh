#!/usr/bin/env bash
# test_release_args.sh: checks that browser/args.release.gn (the build for people) differs from
# browser/args.gn (development and test) in exactly one line, pegboard_test_hooks, and that both
# keep the settings the release depends on.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

dev="$(args_for dev)"
release="$(args_for release)"
failed=0
fail() {
  echo "FAIL: $*" >&2
  failed=1
}

[ -f "$dev" ] || die "$dev does not exist"
[ -f "$release" ] || die "$release does not exist"

# The argument lines: no comments, no blank lines, spaces around '=' normalized.
args_lines() {
  sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e 's/^[[:space:]]*//' "$1" |
    sed -E 's/[[:space:]]*=[[:space:]]*/ = /' | grep -v '^$' | LC_ALL=C sort
}

only_dev="$(LC_ALL=C comm -23 <(args_lines "$dev") <(args_lines "$release"))"
only_release="$(LC_ALL=C comm -13 <(args_lines "$dev") <(args_lines "$release"))"
[ "$only_dev" = "pegboard_test_hooks = true" ] ||
  fail "lines only in args.gn should be exactly 'pegboard_test_hooks = true', got: ${only_dev:-<none>}"
[ "$only_release" = "pegboard_test_hooks = false" ] ||
  fail "lines only in args.release.gn should be exactly 'pegboard_test_hooks = false', got: ${only_release:-<none>}"

for file in "$dev" "$release"; do
  lines="$(args_lines "$file")"
  for want in 'disable_fieldtrial_testing_config = true' 'dcheck_always_on = false' \
    'is_debug = false' 'target_cpu = "arm64"'; do
    grep -Fxq "$want" <<< "$lines" || fail "$(basename "$file") lacks '$want'"
  done
  if grep -Eq '^is_official_build ' <<< "$lines"; then
    fail "$(basename "$file") sets is_official_build (left for after 3c)"
  fi
done

[ "$(out_dir_for dev)" = "out/Pegboard" ] || fail "out_dir_for dev is $(out_dir_for dev)"
[ "$(out_dir_for release)" = "out/Release" ] || fail "out_dir_for release is $(out_dir_for release)"

[ "$failed" -eq 0 ] || exit 1
echo "PASS"
