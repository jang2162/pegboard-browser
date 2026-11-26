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

# ---- browser/scripts/release.sh ----

# The secret parts of the fake signing identity: none may reach any output.
FAKE_ID_NAME="Test Person Secretname"
FAKE_TEAM="QQ9TEAM9QQ"
FAKE_SHA1_A="1111111111111111111111111111111111111111"
FAKE_SHA1_B="ABCDEF2222222222222222222222222222222222"
FAKE_PUBKEY="pZ0QnK3Hb0n7kq5e3S8Xz2yQwP4c6Vt1uR9sLmNoAbc="

# The identities the fake security lists, one SHA-1 per argument (each a Developer ID
# Application of the fake person), plus an Apple Development one that never counts.
fake_identities() {
  local i=1 sha
  {
    printf '  %d) %s "Apple Development: %s (%s)"\n' "$i" 3333333333333333333333333333333333333333 \
      "$FAKE_ID_NAME" "$FAKE_TEAM"
    for sha in "$@"; do
      i=$((i + 1))
      printf '  %d) %s "Developer ID Application: %s (%s)"\n' "$i" "$sha" "$FAKE_ID_NAME" "$FAKE_TEAM"
    done
    printf '     %d valid identities found\n' "$i"
  } > "$T/identities"
}

# A fake repository with release.sh and what it reads, fakes of security, codesign, xcrun, gh,
# sign_update and generate_keys, and a world in which every preflight check passes. Each fake
# logs its arguments to $T/<name>.log; the FAKE_* variables make one fail.
release_env() {
  fake_repo browser/scripts/release.sh browser/scripts/lib.sh browser/release/config.sh \
    scripts/fetch-sparkle.sh
  mkdir -p "$T/bin" "$T/repo/vendor/sparkle/bin" "$T/repo/vendor/sparkle/Sparkle.framework" \
    "$T/repo/vendor/agent-browser"
  fake_identities "$FAKE_SHA1_A"

  cat > "$T/bin/security" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/security.log"
[ "$1 $2 $3 $4" = "find-identity -v -p codesigning" ] || exit 2
cat "$T/identities"
SH
  cat > "$T/bin/codesign" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/codesign.log"
if [ -n "${FAKE_CODESIGN_FAIL:-}" ]; then
  echo "$FAKE_CODESIGN_FAIL: errSecInternalComponent" >&2
  exit 1
fi
SH
  cat > "$T/bin/xcrun" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/xcrun.log"
[ "$1 $2 $3 $4" = "notarytool history --keychain-profile pegboard-notary" ] || exit 2
[ -z "${FAKE_NOTARY_FAIL:-}" ] || { echo "No Keychain password item found for profile" >&2; exit 69; }
echo "No submission history."
SH
  cat > "$T/bin/gh" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/gh.log"
case "$*" in
  "auth status")
    [ -z "${FAKE_GH_AUTH_FAIL:-}" ] || { echo "You are not logged into any GitHub hosts." >&2; exit 1; }
    ;;
  "repo view --json nameWithOwner --jq .nameWithOwner") echo "Some-Owner/pegboard-browser" ;;
  "api repos/Some-Owner/pegboard-browser/pages")
    [ -f "$T/pages.json" ] || { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
    cat "$T/pages.json"
    ;;
  "api -H Accept: application/vnd.github.raw repos/Some-Owner/pegboard-browser/contents/appcast.xml?ref=gh-pages")
    [ -z "${FAKE_GH_API_FAIL:-}" ] || { echo "gh: Server Error (HTTP 502)" >&2; exit 1; }
    [ -f "$T/appcast.xml" ] || { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
    cat "$T/appcast.xml"
    ;;
  *) echo "fake gh: unexpected: $*" >&2; exit 2 ;;
esac
SH
  cat > "$T/repo/vendor/sparkle/bin/sign_update" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/sign_update.log"
[ -z "${FAKE_SIGN_UPDATE_FAIL:-}" ] || exit 1
[ "$1" = -p ] && [ -f "$2" ] || exit 2
echo "c2lnbmF0dXJlLW9mLXRoZS1wcm9iZQ=="
SH
  cat > "$T/repo/vendor/sparkle/bin/generate_keys" << 'SH'
#!/usr/bin/env bash
echo "$*" >> "$T/generate_keys.log"
[ "$*" = -p ] || exit 2
echo "$FAKE_PUBKEY"
SH
  chmod +x "$T/bin/"* "$T/repo/vendor/sparkle/bin/"*
  export PATH="$T/bin:$PATH" FAKE_PUBKEY

  echo "$FAKE_PUBKEY" > "$T/repo/browser/release/sparkle-public-ed-key"
  printf '%064d\n' 7 > "$T/repo/vendor/sparkle.lock"
  sed -n 's/^SPARKLE_VERSION="\(.*\)"$/\1/p' "$T/repo/scripts/fetch-sparkle.sh" > "$T/repo/vendor/sparkle/VERSION"
  echo binary > "$T/repo/vendor/agent-browser/agent-browser"
  {
    printf '%064d  agent-browser-0.42.0.tgz\n' 5
    printf '%s  bin/agent-browser-darwin-arm64\n' "$(sha "$T/repo/vendor/agent-browser/agent-browser")"
  } > "$T/repo/vendor/agent-browser.lock"
  cat > "$T/pages.json" << 'JSON'
{"url": "https://api.github.com/repos/Some-Owner/pegboard-browser/pages", "status": "built",
 "cname": null, "html_url": "https://some-owner.github.io/pegboard-browser/",
 "build_type": "legacy", "source": {"branch": "gh-pages", "path": "/"}, "https_enforced": true}
JSON
  unset PEGBOARD_SIGN_IDENTITY PEGBOARD_RELEASE_FROM PEGBOARD_BUILD_JOBS PEGBOARD_SRC
}

# An appcast with one item per build number given.
fake_appcast() {
  local n
  {
    echo '<?xml version="1.0" encoding="utf-8"?>'
    echo '<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:pegboard="https://pegboard.invalid/appcast">'
    echo '<channel><title>Pegboard</title>'
    for n in "$@"; do
      echo "<item><title>156.0.8078.109-pb.$n</title><pegboard:n>$n</pegboard:n><sparkle:version>8078.109.$n</sparkle:version></item>"
    done
    echo '</channel></rss>'
    echo '<!-- sparkle-signatures:'
    echo 'edSignature: c2ln'
    echo 'length: 1'
    echo '-->'
  } > "$T/appcast.xml"
}

# Sources release.sh (its main does not run) so a test can call its functions or replace its
# stages; the stages then only log their names to $T/stages.log.
source_release_with_fake_stages() {
  # shellcheck source=release.sh
  . "$T/repo/browser/scripts/release.sh"
  local s
  for s in preflight build package notarize dmg publish promote; do
    eval "stage_$s() { echo $s >> \"\$T/stages.log\"; }"
  done
}

# Runs release.sh's main in a subshell with errexit on: never inside `if`, `||` or `&&`, where
# bash turns errexit off for everything the subshell runs, so a failing stage would go on.
# Output in $T/out, exit code in MAIN_RC.
run_main() {
  set +e
  (
    set -e
    main "$@"
  ) > "$T/out" 2>&1
  MAIN_RC=$?
  set -e
}

test_release_preflight_passes_with_everything_in_place() {
  release_env
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  grep -q "preflight: PASS" "$T/out" || fail "no PASS: $(cat "$T/out")"
  grep -q "next N: 1" "$T/out" || fail "no appcast yet should say the next N is 1: $(cat "$T/out")"
  grep -q -- "--timestamp -o runtime -s $FAKE_SHA1_A" "$T/codesign.log" ||
    fail "codesign not called with the identity's SHA-1: $(cat "$T/codesign.log")"
  grep -q -- "^-p " "$T/sign_update.log" || fail "sign_update not used"
}

test_release_two_identities_need_PEGBOARD_SIGN_IDENTITY() {
  release_env
  source_release_with_fake_stages
  fake_identities "$FAKE_SHA1_A" "$FAKE_SHA1_B"
  if (sign_identity) > "$T/out" 2>&1; then
    fail "picked one of two identities: $(cat "$T/out")"
  fi
  grep -q PEGBOARD_SIGN_IDENTITY "$T/out" || fail "does not say what to set: $(cat "$T/out")"
  [ "$(PEGBOARD_SIGN_IDENTITY="$FAKE_SHA1_B" sign_identity)" = "$FAKE_SHA1_B" ] ||
    fail "PEGBOARD_SIGN_IDENTITY was not used"
  # Lower case is the same SHA-1.
  [ "$(PEGBOARD_SIGN_IDENTITY="$(echo "$FAKE_SHA1_B" | tr '[:upper:]' '[:lower:]')" sign_identity)" = "$FAKE_SHA1_B" ] ||
    fail "PEGBOARD_SIGN_IDENTITY was not used"
  # Only a valid Developer ID Application identity of the keychain.
  if (PEGBOARD_SIGN_IDENTITY=3333333333333333333333333333333333333333 sign_identity) > "$T/out" 2>&1; then
    fail "took an Apple Development identity"
  fi
  if (PEGBOARD_SIGN_IDENTITY="Developer ID Application" sign_identity) > "$T/out" 2>&1; then
    fail "took a name instead of a SHA-1"
  fi
  # The full preflight says so too, and fails.
  if "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1; then
    fail "preflight passed with two identities"
  fi
  grep -q PEGBOARD_SIGN_IDENTITY "$T/out" || fail "$(cat "$T/out")"
  PEGBOARD_SIGN_IDENTITY="$FAKE_SHA1_B" "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 ||
    fail "$(cat "$T/out")"
  # One identity is taken; none fails.
  fake_identities "$FAKE_SHA1_A"
  [ "$(sign_identity)" = "$FAKE_SHA1_A" ] || fail "the one identity was not taken"
  fake_identities
  if (sign_identity) > "$T/out" 2>&1; then
    fail "passed without a Developer ID Application identity"
  fi
}

test_release_identity_name_is_never_printed() {
  release_env
  local out
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 || fail "$(cat "$T/out")"
  # A failing codesign that names the identity, and two identities.
  FAKE_CODESIGN_FAIL="error: Developer ID Application: $FAKE_ID_NAME ($FAKE_TEAM)" \
    "$T/repo/browser/scripts/release.sh" preflight >> "$T/out" 2>&1 && fail "passed with a failing codesign"
  fake_identities "$FAKE_SHA1_A" "$FAKE_SHA1_B"
  "$T/repo/browser/scripts/release.sh" preflight >> "$T/out" 2>&1 && fail "passed with two identities"
  PEGBOARD_SIGN_IDENTITY="$FAKE_SHA1_B" "$T/repo/browser/scripts/release.sh" preflight >> "$T/out" 2>&1 ||
    fail "$(cat "$T/out")"
  out="$(cat "$T/out")"
  grep -q "codesign" <<< "$out" || fail "the codesign failure was not reported: $out"
  for secret in Secretname "Test Person" "$FAKE_TEAM"; do
    if grep -q "$secret" "$T/out"; then
      fail "printed '$secret': $(grep "$secret" "$T/out")"
    fi
    if grep -rq "$secret" "$T/repo/target" 2> /dev/null; then
      fail "wrote '$secret' under target/"
    fi
  done
}

test_release_preflight_says_what_to_do_for_each_problem() {
  release_env
  rm "$T/repo/browser/release/sparkle-public-ed-key" "$T/pages.json"
  : > "$T/repo/vendor/sparkle.lock"
  echo other > "$T/repo/vendor/agent-browser/agent-browser"
  if FAKE_NOTARY_FAIL=1 FAKE_SIGN_UPDATE_FAIL=1 "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1; then
    fail "passed"
  fi
  local want
  for want in "notarytool store-credentials pegboard-notary" "generate_keys" "sparkle-public-ed-key" \
    "Pages" "fetch-sparkle.sh" "fetch-agent-browser.sh --locked"; do
    grep -q -- "$want" "$T/out" || fail "nothing about '$want': $(cat "$T/out")"
  done
  [ "$(grep -c '^FAIL' "$T/out")" -ge 5 ] || fail "not one line per problem: $(cat "$T/out")"
  # A public key that differs from the keychain's.
  release_env
  echo "AAAAQnK3Hb0n7kq5e3S8Xz2yQwP4c6Vt1uR9sLmNoAbc=" > "$T/repo/browser/release/sparkle-public-ed-key"
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 && fail "passed with another public key"
  grep -q "sparkle-public-ed-key" "$T/out" || fail "$(cat "$T/out")"
  # Pages without enforced HTTPS, from another branch, or on a custom domain.
  release_env
  sed -i '' 's/"https_enforced": true/"https_enforced": false/' "$T/pages.json"
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 && fail "passed without enforced HTTPS"
  grep -q "Enforce HTTPS" "$T/out" || fail "$(cat "$T/out")"
  release_env
  sed -i '' 's/"branch": "gh-pages"/"branch": "main"/' "$T/pages.json"
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 && fail "passed with Pages from main"
  release_env
  sed -i '' 's|"html_url": "https://some-owner.github.io/pegboard-browser/"|"html_url": "https://pages.example.com/"|' "$T/pages.json"
  "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 && fail "passed with a custom domain"
  # gh not logged in.
  release_env
  FAKE_GH_AUTH_FAIL=1 "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 && fail "passed without gh login"
  grep -q "gh auth login" "$T/out" || fail "$(cat "$T/out")"
  true
}

test_release_feed_url_and_slug() {
  release_env
  source_release_with_fake_stages
  [ "$(repo_slug)" = "Some-Owner/pegboard-browser" ] || fail "repo_slug: $(repo_slug)"
  [ "$(feed_url)" = "https://some-owner.github.io/pegboard-browser/appcast.xml" ] ||
    fail "feed_url: $(feed_url)"
}

test_release_n_must_grow() {
  release_env
  source_release_with_fake_stages
  [ "$(appcast_max_n Some-Owner/pegboard-browser)" = 0 ] || fail "no appcast is not 0"
  fake_appcast 1 3 2
  [ "$(appcast_max_n Some-Owner/pegboard-browser)" = 3 ] || fail "max of 1 3 2"
  local n
  for n in 1 3; do
    if "$T/repo/browser/scripts/release.sh" preflight "$n" > "$T/out" 2>&1; then
      fail "N=$n passed with N=3 in the appcast"
    fi
    grep -q "N=$n is not above" "$T/out" || fail "$(cat "$T/out")"
  done
  "$T/repo/browser/scripts/release.sh" preflight 4 > "$T/out" 2>&1 || fail "N=4: $(cat "$T/out")"
  # N is a positive integer.
  for n in 0 04 3a -1 ""; do
    if "$T/repo/browser/scripts/release.sh" preflight "$n" > "$T/out" 2>&1; then
      fail "N='$n' passed"
    fi
  done
  # A failed fetch is not an empty appcast.
  if FAKE_GH_API_FAIL=1 "$T/repo/browser/scripts/release.sh" preflight 4 > "$T/out" 2>&1; then
    fail "passed when the appcast could not be read"
  fi
  # Nor is an appcast with an item that has no build number, or a strange one.
  fake_appcast 1 x
  if (appcast_max_n Some-Owner/pegboard-browser) > "$T/out" 2>&1; then
    fail "read a strange build number: $(cat "$T/out")"
  fi
  fake_appcast 1
  sed -i '' 's|<pegboard:n>1</pegboard:n>||' "$T/appcast.xml"
  if (appcast_max_n Some-Owner/pegboard-browser) > "$T/out" 2>&1; then
    fail "read an item without a build number"
  fi
}

test_release_build_jobs_above_8_are_refused() {
  release_env
  local jobs
  for jobs in 9 12 abc 0; do
    for args in "preflight" "canary 4" "stable 4"; do
      # shellcheck disable=SC2086
      if PEGBOARD_SRC="$T" PEGBOARD_BUILD_JOBS="$jobs" "$T/repo/browser/scripts/release.sh" $args > "$T/out" 2>&1; then
        fail "PEGBOARD_BUILD_JOBS=$jobs passed for $args"
      fi
      grep -q PEGBOARD_BUILD_JOBS "$T/out" || fail "$(cat "$T/out")"
    done
  done
  [ ! -e "$T/gh.log" ] && [ ! -e "$T/codesign.log" ] || fail "ran checks before refusing"
  [ ! -e "$T/repo/target" ] || fail "made a work directory before refusing"
  PEGBOARD_BUILD_JOBS=8 "$T/repo/browser/scripts/release.sh" preflight > "$T/out" 2>&1 || fail "8: $(cat "$T/out")"
}

test_release_stage_order_and_resume() {
  release_env
  source_release_with_fake_stages
  local stages="$T/repo/target/release-pegboard/4/stages"
  export PEGBOARD_SRC="$T"
  run_main canary 4
  [ "$MAIN_RC" -eq 0 ] || fail "$(cat "$T/out")"
  [ "$(tr '\n' ' ' < "$T/stages.log")" = "preflight build package notarize dmg publish " ] ||
    fail "order: $(cat "$T/stages.log")"
  for s in preflight build package notarize dmg publish; do
    [ -f "$stages/$s.done" ] || fail "no $s.done"
  done
  grep -q "publish" "$T/repo/target/release-pegboard/4/release.log" || fail "no release.log"
  # Resume from dmg: only dmg and publish run.
  rm "$T/stages.log"
  PEGBOARD_RELEASE_FROM=dmg run_main canary 4
  [ "$MAIN_RC" -eq 0 ] || fail "$(cat "$T/out")"
  [ "$(tr '\n' ' ' < "$T/stages.log")" = "dmg publish " ] || fail "resume: $(cat "$T/stages.log")"
  # ...but not when a stage before it never finished.
  rm "$T/stages.log" "$stages/notarize.done"
  PEGBOARD_RELEASE_FROM=dmg run_main canary 4
  [ "$MAIN_RC" -ne 0 ] || fail "resumed from dmg without notarize.done"
  grep -q "notarize" "$T/out" || fail "does not name the missing stage: $(cat "$T/out")"
  [ ! -e "$T/stages.log" ] || fail "ran a stage: $(cat "$T/stages.log")"
  # A failing stage leaves no marker, and neither it nor the later stages count as done.
  # shellcheck disable=SC2329 # called by run_stages as "stage_$s"
  stage_package() {
    echo package >> "$T/stages.log"
    false
  }
  run_main canary 4
  [ "$MAIN_RC" -ne 0 ] || fail "passed with a failing stage"
  [ "$(tr '\n' ' ' < "$T/stages.log")" = "preflight build package " ] || fail "after a failure: $(cat "$T/stages.log")"
  for s in package notarize dmg publish; do
    [ ! -e "$stages/$s.done" ] || fail "$s.done is left after package failed"
  done
  [ -f "$stages/build.done" ] || fail "build.done is gone"
  # Unknown stage, or one of the other channel.
  for from in sign promote; do
    PEGBOARD_RELEASE_FROM="$from" run_main canary 4
    [ "$MAIN_RC" -ne 0 ] || fail "PEGBOARD_RELEASE_FROM=$from passed for canary"
  done
  # canary needs the Chromium tree.
  unset PEGBOARD_SRC
  run_main canary 5
  [ "$MAIN_RC" -ne 0 ] || fail "canary ran without PEGBOARD_SRC"
  grep -q PEGBOARD_SRC "$T/out" || fail "$(cat "$T/out")"
  # A bad channel or N.
  for args in "beta 4" "canary" "canary 4 5" "canary 0" "stable x"; do
    # shellcheck disable=SC2086
    PEGBOARD_SRC="$T" run_main $args
    [ "$MAIN_RC" -ne 0 ] || fail "'$args' passed"
  done
}

test_release_stable_only_runs_promote() {
  release_env
  source_release_with_fake_stages
  run_main stable 4
  [ "$MAIN_RC" -eq 0 ] || fail "$(cat "$T/out")"
  [ "$(tr '\n' ' ' < "$T/stages.log")" = "promote " ] || fail "stable ran: $(cat "$T/stages.log")"
  [ -f "$T/repo/target/release-pegboard/4/stages/promote.done" ] || fail "no promote.done"
  for from in dmg publish preflight; do
    PEGBOARD_RELEASE_FROM="$from" run_main stable 4
    [ "$MAIN_RC" -ne 0 ] || fail "PEGBOARD_RELEASE_FROM=$from passed for stable"
  done
}

test_release_unwritten_stages_stop() {
  release_env
  # The real stages after build are not written yet: each stops instead of passing.
  # shellcheck source=release.sh
  . "$T/repo/browser/scripts/release.sh"
  local s
  for s in package notarize dmg publish promote; do
    if ("stage_$s") > "$T/out" 2>&1; then
      fail "stage $s passed"
    fi
  done
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
