#!/usr/bin/env bash
# test_release.sh [name-filter]: tests of the release scripts that need no network, no
# signing identity and no browser. Each test_* function runs in its own subshell, in a new
# temporary directory, with fakes (curl, ...) first on PATH; a script under test is copied
# into a fake repository there, so the real vendor/ is never touched.
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPTS_DIR/../.." && pwd)"

fail() {
  echo "    $*" >&2
  exit 1
}

# A fake repository at $T/repo with the given repository files copied in.
fake_repo() {
  local rel
  mkdir -p "$T/repo/vendor"
  for rel in "$@"; do
    mkdir -p "$T/repo/$(dirname "$rel")"
    cp "$REPO_ROOT/$rel" "$T/repo/$rel"
  done
}

# A curl that serves $FAKE_CURL_FILE for any URL and logs its arguments to $T/curl.log.
fake_curl() {
  mkdir -p "$T/bin"
  cat > "$T/bin/curl" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_CURL_LOG"
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$out" ] || exit 2
cp "$FAKE_CURL_FILE" "$out"
SH
  chmod +x "$T/bin/curl"
  export PATH="$T/bin:$PATH" FAKE_CURL_LOG="$T/curl.log"
}

sha() {
  shasum -a 256 "$1" | awk '{print $1}'
}

# A Sparkle release archive in the layout of the real one.
fake_sparkle_archive() {
  local d="$T/sparkle-src"
  mkdir -p "$d/Sparkle.framework/Versions/B/Resources" "$d/bin"
  echo framework > "$d/Sparkle.framework/Versions/B/Sparkle"
  ln -s B "$d/Sparkle.framework/Versions/Current"
  ln -s Versions/Current/Sparkle "$d/Sparkle.framework/Sparkle"
  printf '#!/bin/sh\n' > "$d/bin/sign_update"
  printf '#!/bin/sh\n' > "$d/bin/generate_keys"
  chmod +x "$d/bin/sign_update" "$d/bin/generate_keys"
  echo "Sparkle license" > "$d/LICENSE"
  tar -cJf "$T/sparkle.tar.xz" -C "$d" Sparkle.framework bin LICENSE
  export FAKE_CURL_FILE="$T/sparkle.tar.xz"
}

# An npm package of agent-browser with a binary and license files in several places.
fake_agent_browser_archive() {
  local d="$T/ab-src"
  mkdir -p "$d/package/bin" "$d/package/cli/src/native/a11y" "$d/package/licenses"
  echo binary > "$d/package/bin/agent-browser-darwin-arm64"
  echo "Apache License 2.0" > "$d/package/LICENSE"
  echo "axe" > "$d/package/cli/src/native/a11y/LICENSE-axe-core.txt"
  echo "notice" > "$d/package/NOTICE"
  echo "dep" > "$d/package/licenses/dep.txt"
  echo "readme" > "$d/package/README.md"
  tar -czf "$T/ab.tgz" -C "$d" package
  export FAKE_CURL_FILE="$T/ab.tgz"
}

# ---- scripts/fetch-sparkle.sh ----

test_sparkle_lock_mismatch_fails() {
  fake_repo scripts/fetch-sparkle.sh
  fake_curl
  fake_sparkle_archive
  printf '%064d\n' 0 > "$T/repo/vendor/sparkle.lock"
  if "$T/repo/scripts/fetch-sparkle.sh" > "$T/out" 2>&1; then
    fail "a wrong lock passed"
  fi
  grep -q "checksum mismatch" "$T/out" || fail "no mismatch message: $(cat "$T/out")"
  [ ! -e "$T/repo/vendor/sparkle" ] || fail "something was unpacked"
  [ "$(tr -d '[:space:]' < "$T/repo/vendor/sparkle.lock")" = "$(printf '%064d' 0)" ] ||
    fail "the lock was changed"
  grep -q -- "--proto =https" "$T/curl.log" || fail "curl without --proto =https: $(cat "$T/curl.log")"
  grep -q "https://github.com/sparkle-project/Sparkle/releases/download/" "$T/curl.log" ||
    fail "not the release URL: $(cat "$T/curl.log")"
}

test_empty_sparkle_lock_records_and_fails() {
  fake_repo scripts/fetch-sparkle.sh
  fake_curl
  fake_sparkle_archive
  : > "$T/repo/vendor/sparkle.lock"
  if "$T/repo/scripts/fetch-sparkle.sh" > "$T/out" 2>&1; then
    fail "an empty lock passed"
  fi
  grep -q "review and commit" "$T/out" || fail "no 'review and commit': $(cat "$T/out")"
  [ "$(tr -d '[:space:]' < "$T/repo/vendor/sparkle.lock")" = "$(sha "$T/sparkle.tar.xz")" ] ||
    fail "the archive's sha256 was not recorded"
  [ ! -e "$T/repo/vendor/sparkle" ] || fail "unpacked before a person reviewed the pin"
  # A missing lock file is the same.
  rm "$T/repo/vendor/sparkle.lock"
  if "$T/repo/scripts/fetch-sparkle.sh" > "$T/out" 2>&1; then
    fail "a missing lock passed"
  fi
  [ -s "$T/repo/vendor/sparkle.lock" ] || fail "nothing recorded"
}

test_sparkle_with_the_right_lock_is_unpacked() {
  fake_repo scripts/fetch-sparkle.sh
  fake_curl
  fake_sparkle_archive
  sha "$T/sparkle.tar.xz" > "$T/repo/vendor/sparkle.lock"
  "$T/repo/scripts/fetch-sparkle.sh" > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  local v="$T/repo/vendor/sparkle"
  [ -f "$v/LICENSE" ] && [ -x "$v/bin/sign_update" ] && [ -x "$v/bin/generate_keys" ] ||
    fail "missing files: $(find "$v")"
  [ -L "$v/Sparkle.framework/Versions/Current" ] || fail "the framework's symbolic links were not kept"
  [ "$(cat "$v/Sparkle.framework/Sparkle")" = framework ] || fail "framework content"
  [ ! -e "$T/repo/vendor/sparkle.new" ] || fail "the staging directory was left"
}

test_sparkle_lock_must_be_one_sha256() {
  fake_repo scripts/fetch-sparkle.sh
  fake_curl
  fake_sparkle_archive
  echo "not-a-sum" > "$T/repo/vendor/sparkle.lock"
  if "$T/repo/scripts/fetch-sparkle.sh" > "$T/out" 2>&1; then
    fail "a malformed lock passed"
  fi
  [ ! -e "$T/repo/vendor/sparkle" ] || fail "something was unpacked"
}

# ---- scripts/fetch-agent-browser.sh ----

test_agent_browser_locked_refuses_an_empty_lock() {
  fake_repo scripts/fetch-agent-browser.sh
  fake_curl
  fake_agent_browser_archive
  : > "$T/repo/vendor/agent-browser.lock"
  if "$T/repo/scripts/fetch-agent-browser.sh" --locked > "$T/out" 2>&1; then
    fail "--locked passed with an empty lock"
  fi
  grep -q -- "--locked" "$T/out" || fail "no reason given: $(cat "$T/out")"
  [ ! -s "$T/repo/vendor/agent-browser.lock" ] || fail "--locked recorded a sum"
  [ ! -e "$T/repo/vendor/agent-browser/agent-browser" ] || fail "installed anyway"
  # A lock with only the binary's line (the form before 3c) is not enough either.
  printf '%s  bin/agent-browser-darwin-arm64\n' \
    "$(cd "$T/ab-src/package" && sha bin/agent-browser-darwin-arm64)" > "$T/repo/vendor/agent-browser.lock"
  if "$T/repo/scripts/fetch-agent-browser.sh" --locked > "$T/out" 2>&1; then
    fail "--locked passed without the package's sum"
  fi
}

test_agent_browser_records_an_empty_lock_without_locked() {
  fake_repo scripts/fetch-agent-browser.sh
  fake_curl
  fake_agent_browser_archive
  : > "$T/repo/vendor/agent-browser.lock"
  "$T/repo/scripts/fetch-agent-browser.sh" > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  grep -q "review and commit" "$T/out" || fail "recorded silently"
  grep -q "^$(sha "$T/ab.tgz")  agent-browser-0.42.0.tgz$" "$T/repo/vendor/agent-browser.lock" ||
    fail "package sum: $(cat "$T/repo/vendor/agent-browser.lock")"
  # And now --locked passes with what was recorded.
  "$T/repo/scripts/fetch-agent-browser.sh" --locked > "$T/out" 2>&1 || fail "$(cat "$T/out")"
}

test_agent_browser_lock_mismatch_fails() {
  fake_repo scripts/fetch-agent-browser.sh
  fake_curl
  fake_agent_browser_archive
  printf '%064d  agent-browser-0.42.0.tgz\n' 0 > "$T/repo/vendor/agent-browser.lock"
  if "$T/repo/scripts/fetch-agent-browser.sh" --locked > "$T/out" 2>&1; then
    fail "a wrong package sum passed"
  fi
  grep -q "checksum mismatch for agent-browser-0.42.0.tgz" "$T/out" || fail "$(cat "$T/out")"
  [ ! -e "$T/repo/vendor/agent-browser" ] || fail "installed anyway"
  # Malformed lines and other names are refused before anything is fetched.
  rm -f "$T/curl.log"
  printf 'abc  agent-browser-0.42.0.tgz\n' > "$T/repo/vendor/agent-browser.lock"
  if "$T/repo/scripts/fetch-agent-browser.sh" > "$T/out" 2>&1; then
    fail "a malformed lock passed"
  fi
  [ ! -e "$T/curl.log" ] || fail "fetched with a malformed lock"
  printf '%064d  agent-browser-0.41.0.tgz\n' 0 > "$T/repo/vendor/agent-browser.lock"
  if "$T/repo/scripts/fetch-agent-browser.sh" > "$T/out" 2>&1; then
    fail "a lock of another version passed"
  fi
}

test_agent_browser_keeps_every_license_file() {
  fake_repo scripts/fetch-agent-browser.sh
  fake_curl
  fake_agent_browser_archive
  {
    printf '%s  agent-browser-0.42.0.tgz\n' "$(sha "$T/ab.tgz")"
    printf '%s  bin/agent-browser-darwin-arm64\n' "$(sha "$T/ab-src/package/bin/agent-browser-darwin-arm64")"
  } > "$T/repo/vendor/agent-browser.lock"
  "$T/repo/scripts/fetch-agent-browser.sh" --locked > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  local l="$T/repo/vendor/agent-browser/licenses"
  for f in LICENSE NOTICE cli/src/native/a11y/LICENSE-axe-core.txt licenses/dep.txt; do
    [ -f "$l/$f" ] || fail "no $f in licenses/: $(cd "$l" && find . -type f)"
  done
  [ ! -e "$l/README.md" ] || fail "README.md is not a license file"
  [ -x "$T/repo/vendor/agent-browser/agent-browser" ] || fail "no binary"
  grep -q -- "--proto =https" "$T/curl.log" || fail "curl without --proto =https"
}

# ---- browser/release/notices.sh ----

test_notices_has_every_license() {
  fake_repo browser/release/notices.sh LICENSE
  mkdir -p "$T/repo/vendor/agent-browser/licenses/cli" "$T/repo/vendor/sparkle"
  echo apache > "$T/repo/vendor/agent-browser/licenses/LICENSE"
  echo axe > "$T/repo/vendor/agent-browser/licenses/cli/LICENSE-axe-core.txt"
  echo sparkle > "$T/repo/vendor/sparkle/LICENSE"
  mkdir "$T/out-dir"
  "$T/repo/browser/release/notices.sh" "$T/out-dir" > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  local n="$T/out-dir/Notices"
  [ "$(cd "$n" && find . -mindepth 1 -maxdepth 1 | LC_ALL=C sort | tr '\n' ' ')" = \
    "./Pegboard-LICENSE.txt ./README.txt ./Sparkle ./agent-browser " ] ||
    fail "top level: $(cd "$n" && find . -mindepth 1 -maxdepth 1)"
  cmp -s "$n/Pegboard-LICENSE.txt" "$REPO_ROOT/LICENSE" || fail "Pegboard-LICENSE.txt"
  [ "$(cat "$n/agent-browser/LICENSE")" = apache ] || fail "agent-browser/LICENSE"
  [ "$(cat "$n/agent-browser/cli/LICENSE-axe-core.txt")" = axe ] || fail "agent-browser's other files"
  [ "$(cat "$n/Sparkle/LICENSE")" = sparkle ] || fail "Sparkle/LICENSE"
  grep -q "chrome://credits" "$n/README.txt" || fail "README.txt does not point at chrome://credits"
  [ "$(grep -c "chrome://credits" "$n/README.txt")" -eq 2 ] || fail "README.txt: one line in each language"
  # Again: the same result, nothing left over.
  "$T/repo/browser/release/notices.sh" "$T/out-dir" > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  [ ! -e "$n.new" ] || fail "staging left"
}

test_notices_fail_without_a_license() {
  fake_repo browser/release/notices.sh LICENSE
  mkdir -p "$T/repo/vendor/agent-browser/licenses" "$T/out-dir"
  echo apache > "$T/repo/vendor/agent-browser/licenses/LICENSE"
  # No Sparkle.
  if "$T/repo/browser/release/notices.sh" "$T/out-dir" > "$T/out" 2>&1; then
    fail "passed without Sparkle's license"
  fi
  grep -q "fetch-sparkle.sh" "$T/out" || fail "$(cat "$T/out")"
  [ ! -e "$T/out-dir/Notices" ] || fail "left a partial Notices/"
  # No agent-browser license.
  mkdir -p "$T/repo/vendor/sparkle"
  echo sparkle > "$T/repo/vendor/sparkle/LICENSE"
  rm "$T/repo/vendor/agent-browser/licenses/LICENSE"
  if "$T/repo/browser/release/notices.sh" "$T/out-dir" > "$T/out" 2>&1; then
    fail "passed without agent-browser's license"
  fi
  [ ! -e "$T/out-dir/Notices" ] || fail "left a partial Notices/"
}

# ---- browser/release/config.sh ----

test_config_has_the_release_settings() {
  # shellcheck source=../release/config.sh
  . "$REPO_ROOT/browser/release/config.sh"
  [ "$NOTARY_PROFILE" = pegboard-notary ] && [ "$CANARY_CHANNEL" = canary ] &&
    [ "$CHECK_INTERVAL" = 86400 ] && [ "$APPCAST_FILE" = appcast.xml ] &&
    [ "$APPCAST_KEEP" = 20 ] && [ "$PAGES_WAIT_SECS" = 600 ] && [ "$MAX_BUILD_JOBS" = 8 ] ||
    fail "config.sh values"
}

# ---- runner ----

filter="${1:-}"
passed=0
failed=0
for t in $(declare -F | awk '{print $3}' | grep '^test_' | LC_ALL=C sort); do
  [ -z "$filter" ] || [[ "$t" == *"$filter"* ]] || continue
  T="$(mktemp -d)"
  # Not in an `if`: there errexit would be off inside the test.
  set +e
  (
    set -e
    export T
    cd "$T"
    "$t"
  )
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    echo "ok   ${t#test_}"
    passed=$((passed + 1))
  else
    echo "FAIL ${t#test_}"
    failed=$((failed + 1))
  fi
  rm -rf "$T"
done
echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ] && [ "$passed" -gt 0 ]
