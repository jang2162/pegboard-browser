#!/usr/bin/env bash
# release.sh: builds, signs, notarizes and publishes Pegboard (phase 3c design 4.1).
#
#   release.sh preflight [<N>]   checks what a release needs, in the foreground (see preflight)
#   release.sh canary <N>        preflight build package notarize dmg publish
#   release.sh stable <N>        promote: build N, already out as canary, becomes stable
#
# N is the build number (1, 2, ...; shared by every channel, only grows). The work directory is
# <repo>/target/release-pegboard/<N>/: release.log, and stages/<stage>.done for each finished
# stage. PEGBOARD_RELEASE_FROM=<stage> starts at that stage, when every stage before it is done.
# Without it every stage runs again from the first.
#
# Environment: PEGBOARD_SRC (the Chromium src directory; canary needs it), PEGBOARD_SIGN_IDENTITY
# (the SHA-1 of the Developer ID Application identity, needed only when the keychain has more
# than one), PEGBOARD_BUILD_JOBS (at most MAX_BUILD_JOBS).
#
# A release runs for hours: start it detached (nohup ... &) and watch release.log, but run
# `release.sh preflight` in a terminal first, so a person answers the keychain prompts
# (Always Allow) for the signing key and the update key.
#
# Never printed or written anywhere: the signing identity's name and team (found at run time,
# used by SHA-1 only), the notary credentials, the update key's private half.
set -euo pipefail
SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPTS_DIR/lib.sh"
# shellcheck source=../release/config.sh
. "$BROWSER_DIR/release/config.sh"

CANARY_STAGES="preflight build package notarize dmg publish"
STABLE_STAGES="promote"
SPARKLE_BIN="$REPO_ROOT/vendor/sparkle/bin"
# The update key's public half (generate_keys -p); the private half stays in the keychain.
PUBLIC_KEY_FILE="$BROWSER_DIR/release/sparkle-public-ed-key"
PAGES_BRANCH="gh-pages"
# The namespace of the appcast's own elements (pegboard:n, the build number of an item).
APPCAST_NS="https://pegboard.invalid/appcast"

usage() {
  die "usage: release.sh preflight [<N>] | release.sh <canary|stable> <N>"
}

check_build_jobs() {
  local jobs="${PEGBOARD_BUILD_JOBS:-$MAX_BUILD_JOBS}"
  [[ "$jobs" =~ ^[1-9][0-9]*$ ]] || die "PEGBOARD_BUILD_JOBS must be a positive number"
  [ "$jobs" -le "$MAX_BUILD_JOBS" ] ||
    die "PEGBOARD_BUILD_JOBS=$jobs is above $MAX_BUILD_JOBS (more has hung the build machine)"
}

check_n() {
  [[ "$1" =~ ^[1-9][0-9]*$ ]] || die "N must be a whole number from 1, without leading zeros: '$1'"
}

# Hides signing identity names and team IDs in text from codesign and friends.
redact() {
  sed -E -e 's/(Developer ID Application|Developer ID Installer|Apple Development|Apple Distribution|Mac Developer)(: [^"]*)?/\1: <hidden>/g' \
    -e 's/\([A-Z0-9]{10}\)/(<hidden>)/g'
}

# The SHA-1 of the Developer ID Application identity to sign with: the one valid one in the
# keychain, or PEGBOARD_SIGN_IDENTITY (which must be one of them) when there are several.
# Prints only the SHA-1, never the identity's name.
sign_identity() {
  local list ids want count
  list="$(security find-identity -v -p codesigning 2> /dev/null)" ||
    die "security find-identity -v -p codesigning failed"
  ids="$(printf '%s\n' "$list" |
    sed -nE 's/^[[:space:]]*[0-9]+\)[[:space:]]+([0-9A-Fa-f]{40})[[:space:]]+"Developer ID Application: .*$/\1/p' |
    tr '[:lower:]' '[:upper:]' | LC_ALL=C sort -u)"
  if [ -n "${PEGBOARD_SIGN_IDENTITY:-}" ]; then
    want="$(printf '%s' "$PEGBOARD_SIGN_IDENTITY" | tr '[:lower:]' '[:upper:]')"
    [[ "$want" =~ ^[0-9A-F]{40}$ ]] ||
      die "PEGBOARD_SIGN_IDENTITY must be the SHA-1 (40 hex digits) of a Developer ID Application identity"
    grep -qx "$want" <<< "$ids" ||
      die "PEGBOARD_SIGN_IDENTITY is not a valid Developer ID Application identity in the keychain (security find-identity -v -p codesigning)"
    echo "$want"
    return 0
  fi
  count="$(grep -c . <<< "$ids" || true)"
  case "$count" in
    0) die "no valid Developer ID Application identity in the keychain (security find-identity -v -p codesigning)" ;;
    1) echo "$ids" ;;
    *) die "$count Developer ID Application identities in the keychain: set PEGBOARD_SIGN_IDENTITY to the SHA-1 of the one to use (security find-identity -v -p codesigning lists them)" ;;
  esac
}

# owner/repo of this repository on GitHub.
repo_slug() {
  local slug
  slug="$(cd "$REPO_ROOT" && gh repo view --json nameWithOwner --jq .nameWithOwner)" ||
    die "gh repo view failed (gh auth login?)"
  [[ "$slug" =~ ^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$ ]] || die "gh repo view gave a strange name: $slug"
  echo "$slug"
}

# The appcast's address on GitHub Pages, the app's SUFeedURL (Pages names the owner in lower case).
feed_url() {
  local slug owner
  slug="$(repo_slug)" || exit 1
  owner="$(printf '%s' "${slug%%/*}" | tr '[:upper:]' '[:lower:]')"
  echo "https://$owner.github.io/${slug#*/}/$APPCAST_FILE"
}

# The largest build number in the appcast on the gh-pages branch, 0 when there is no appcast
# yet. Any other failure to read it fails: an unreadable appcast is not an empty one.
appcast_max_n() {
  local slug="$1" tmp rc=0
  tmp="$(mktemp -d)"
  if gh api -H "Accept: application/vnd.github.raw" \
    "repos/$slug/contents/$APPCAST_FILE?ref=$PAGES_BRANCH" > "$tmp/appcast.xml" 2> "$tmp/err"; then
    python3 -I - "$tmp/appcast.xml" "$APPCAST_NS" << 'PY' || rc=1
import re, sys
import xml.etree.ElementTree as ET

path, ns = sys.argv[1], sys.argv[2]
try:
    root = ET.parse(path).getroot()
except ET.ParseError as e:
    sys.exit(f"the appcast is not XML: {e}")
ns_seen = []
for item in root.iter("item"):
    found = item.findall(f"{{{ns}}}n")
    if len(found) != 1:
        sys.exit("an appcast item does not have exactly one pegboard:n")
    text = (found[0].text or "").strip()
    if not re.fullmatch(r"[1-9][0-9]*", text):
        sys.exit(f"an appcast item's pegboard:n is not a build number: {text!r}")
    ns_seen.append(int(text))
print(max(ns_seen, default=0))
PY
  elif grep -q "HTTP 404" "$tmp/err"; then
    echo 0
  else
    cat "$tmp/err" >&2
    rc=1
  fi
  rm -rf "$tmp"
  return "$rc"
}

# The rustc flags of the release build of pegboard: the build machine's paths (the
# repository, cargo's and rustup's homes, which hold the user name) are mapped to fixed
# names, so the binary does not carry them. One flag per line (CARGO_ENCODED_RUSTFLAGS form,
# with the line breaks as its separator), so a path with spaces stays one flag.
release_rustflags() {
  printf -- '--remap-path-prefix=%s=/pegboard\n' "$REPO_ROOT"
  printf -- '--remap-path-prefix=%s=/cargo\n' "${CARGO_HOME:-$HOME/.cargo}"
  printf -- '--remap-path-prefix=%s=/rustup\n' "${RUSTUP_HOME:-$HOME/.rustup}"
}

# Builds pegboard for the app as release version $1 (the display version, e.g.
# 156.0.8078.109-pb.3) in RELEASE_CARGO_TARGET_DIR; prints the binary's path.
cargo_release_build() {
  local version="$1" flags
  flags="$(release_rustflags | tr '\n' '\037')"
  flags="${flags%$'\037'}"
  (cd "$REPO_ROOT" && PEGBOARD_RELEASE_VERSION="$version" CARGO_ENCODED_RUSTFLAGS="$flags" \
    cargo build --release --locked -p pegboard-cli --target-dir "$REPO_ROOT/$RELEASE_CARGO_TARGET_DIR" >&2)
  echo "$REPO_ROOT/$RELEASE_CARGO_TARGET_DIR/release/pegboard"
}

# ---- preflight ----

PREFLIGHT_PROBLEMS=0
pf_ok() {
  echo "ok    $*"
}
pf_fail() {
  echo "FAIL  $*"
  PREFLIGHT_PROBLEMS=$((PREFLIGHT_PROBLEMS + 1))
}

# The bundled programs are the locked ones (offline: what the fetch scripts put in vendor/).
preflight_bundles() {
  local want have lock_bin
  want="$(sed -n 's/^SPARKLE_VERSION="\(.*\)"$/\1/p' "$REPO_ROOT/scripts/fetch-sparkle.sh")"
  have="$(cat "$REPO_ROOT/vendor/sparkle/VERSION" 2> /dev/null || true)"
  if [[ "$(tr -d '[:space:]' < "$REPO_ROOT/vendor/sparkle.lock" 2> /dev/null)" =~ ^[0-9a-f]{64}$ ]] &&
    [ -n "$want" ] && [ "$have" = "$want" ] &&
    [ -d "$REPO_ROOT/vendor/sparkle/Sparkle.framework" ] && [ -x "$SPARKLE_BIN/sign_update" ] &&
    [ -x "$SPARKLE_BIN/generate_keys" ]; then
    pf_ok "Sparkle $have from the locked archive in vendor/sparkle"
  else
    pf_fail "vendor/sparkle is not the locked Sparkle ${want:-?} (vendor/sparkle.lock one SHA-256): run scripts/fetch-sparkle.sh"
  fi
  lock_bin="$(awk '$2 == "bin/agent-browser-darwin-arm64" {print $1}' "$REPO_ROOT/vendor/agent-browser.lock" 2> /dev/null || true)"
  if grep -Eq '^[0-9a-f]{64}  agent-browser-[0-9.]+\.tgz$' "$REPO_ROOT/vendor/agent-browser.lock" 2> /dev/null &&
    [[ "$lock_bin" =~ ^[0-9a-f]{64}$ ]] && [ -f "$REPO_ROOT/vendor/agent-browser/agent-browser" ] &&
    [ "$(shasum -a 256 "$REPO_ROOT/vendor/agent-browser/agent-browser" | awk '{print $1}')" = "$lock_bin" ]; then
    pf_ok "agent-browser in vendor/agent-browser matches vendor/agent-browser.lock"
  else
    pf_fail "vendor/agent-browser does not match vendor/agent-browser.lock: run scripts/fetch-agent-browser.sh --locked"
  fi
}

# GitHub Pages serves the gh-pages branch's root, over enforced HTTPS, at <owner>.github.io/<repo>/.
preflight_pages() {
  local slug="$1" tmp expected why
  tmp="$(mktemp -d)"
  expected="$(feed_url)"
  expected="${expected%"$APPCAST_FILE"}"
  if ! gh api "repos/$slug/pages" > "$tmp/pages.json" 2> "$tmp/err"; then
    pf_fail "GitHub Pages is off: repository Settings > Pages > Deploy from a branch, $PAGES_BRANCH, / (root), then Enforce HTTPS"
  elif why="$(python3 -I - "$tmp/pages.json" "$PAGES_BRANCH" "$expected" << 'PY'
import json, sys

pages = json.load(open(sys.argv[1]))
branch, expected = sys.argv[2], sys.argv[3]
source = pages.get("source") or {}
problems = []
if pages.get("build_type") not in (None, "legacy"):
    problems.append(f"it is built by {pages.get('build_type')}, not deployed from a branch")
if source.get("branch") != branch or source.get("path") != "/":
    problems.append(f"its source is {source.get('branch')} {source.get('path')}, not {branch} / (root)")
if pages.get("https_enforced") is not True:
    problems.append("Enforce HTTPS is off")
if (pages.get("html_url") or "").lower() != expected.lower():
    problems.append(f"it is served at {pages.get('html_url')}, not {expected} (no custom domain)")
print("; ".join(problems))
PY
  )" && [ -z "$why" ]; then
    pf_ok "GitHub Pages serves $PAGES_BRANCH at $expected over enforced HTTPS"
  else
    pf_fail "GitHub Pages: ${why:-could not read its settings} (Settings > Pages: Deploy from a branch, $PAGES_BRANCH, / (root), Enforce HTTPS)"
  fi
  rm -rf "$tmp"
}

# Checks everything a release needs and says, one line each, what to do about what is missing.
# Signs a small file with the identity and with the update key, so the keychain asks now.
preflight() {
  local n="${1:-}" tmp id="" sig key want slug="" max
  PREFLIGHT_PROBLEMS=0
  tmp="$(mktemp -d)"

  if id="$(sign_identity 2> "$tmp/err")"; then
    pf_ok "one Developer ID Application identity (SHA-1 ${id:0:8}...)"
    cp /usr/bin/true "$tmp/probe"
    if codesign --force --timestamp -o runtime -s "$id" "$tmp/probe" > "$tmp/codesign.out" 2>&1 &&
      codesign --verify --strict "$tmp/probe" >> "$tmp/codesign.out" 2>&1; then
      pf_ok "codesign --timestamp -o runtime with it"
    else
      pf_fail "codesign with the identity failed (run preflight in a terminal and answer the keychain prompt with Always Allow): $(redact < "$tmp/codesign.out" | tail -3 | tr '\n' ' ')"
    fi
  else
    id=""
    pf_fail "$(sed 's/^error: //' "$tmp/err" | redact)"
    pf_fail "codesign: not tried without an identity"
  fi

  if xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" > /dev/null 2>&1; then
    pf_ok "notarytool profile $NOTARY_PROFILE"
  else
    pf_fail "notarytool cannot use the profile $NOTARY_PROFILE: run xcrun notarytool store-credentials $NOTARY_PROFILE (browser/release/README.md)"
  fi

  printf 'pegboard release preflight\n' > "$tmp/update"
  if [ -x "$SPARKLE_BIN/sign_update" ] && sig="$("$SPARKLE_BIN/sign_update" -p "$tmp/update" 2> /dev/null)" &&
    [ -n "$sig" ]; then
    pf_ok "sign_update signs with the keychain's update key"
  else
    pf_fail "sign_update cannot sign with an update key in the keychain: run vendor/sparkle/bin/generate_keys once (browser/release/README.md), answer the keychain prompt with Always Allow"
  fi
  want="$(tr -d '[:space:]' < "$PUBLIC_KEY_FILE" 2> /dev/null || true)"
  key=""
  if [ -x "$SPARKLE_BIN/generate_keys" ]; then
    key="$("$SPARKLE_BIN/generate_keys" -p 2> /dev/null | tr -d '[:space:]')" || key=""
  fi
  if [ -z "$want" ]; then
    pf_fail "browser/release/sparkle-public-ed-key is missing: put the public key that vendor/sparkle/bin/generate_keys prints in it"
  elif ! [[ "$want" =~ ^[A-Za-z0-9+/]{43}=$ ]]; then
    pf_fail "browser/release/sparkle-public-ed-key is not one EdDSA public key (base64, 44 characters)"
  elif [ "$key" != "$want" ]; then
    pf_fail "browser/release/sparkle-public-ed-key is not the public half of the keychain's update key (vendor/sparkle/bin/generate_keys -p)"
  else
    pf_ok "browser/release/sparkle-public-ed-key is the keychain's update key"
  fi

  if gh auth status > /dev/null 2>&1; then
    pf_ok "gh is logged in"
    if slug="$(repo_slug 2> "$tmp/err")"; then
      preflight_pages "$slug"
    else
      slug=""
      pf_fail "$(cat "$tmp/err")"
    fi
  else
    pf_fail "gh is not logged in: run gh auth login"
  fi

  preflight_bundles

  if [ -z "$slug" ]; then
    pf_fail "N: the appcast was not read (needs gh)"
  elif ! max="$(appcast_max_n "$slug" 2> "$tmp/err")"; then
    pf_fail "N: could not read $APPCAST_FILE on $PAGES_BRANCH: $(tr '\n' ' ' < "$tmp/err")"
  elif [ -z "$n" ]; then
    pf_ok "the appcast's largest N is $max (next N: $((max + 1)))"
  elif [ "$n" -gt "$max" ]; then
    pf_ok "N=$n is above the appcast's largest N ($max)"
  else
    pf_fail "N=$n is not above the appcast's largest N ($max): use $((max + 1)) or more"
  fi

  rm -rf "$tmp"
  if [ "$PREFLIGHT_PROBLEMS" -eq 0 ]; then
    echo "preflight: PASS"
  else
    echo "preflight: $PREFLIGHT_PROBLEMS problem(s)"
    return 1
  fi
}

# ---- stages (each reads only what the stages before it made) ----
# CHANNEL, N, WORK and (canary) PEGBOARD_SRC are set by run_release.

stage_preflight() {
  preflight "$N"
}

stage_build() {
  "$SCRIPTS_DIR/build.sh" "$PEGBOARD_SRC" release
  [ -d "$PEGBOARD_SRC/$(out_dir_for release)/Pegboard.app" ] || die "the release build has no Pegboard.app"
}

stage_package() {
  die "the package stage is not written yet (phase 3c task 6)"
}

stage_notarize() {
  die "the notarize stage is not written yet (phase 3c task 7)"
}

stage_dmg() {
  die "the dmg stage is not written yet (phase 3c task 7)"
}

stage_publish() {
  die "the publish stage is not written yet (phase 3c task 8)"
}

stage_promote() {
  die "the promote stage is not written yet (phase 3c task 8)"
}

run_stages() {
  local from="$1" stages="$2" s running=0
  for s in $stages; do
    if [ "$s" = "$from" ]; then
      running=1
    fi
    [ "$running" -eq 1 ] || continue
    echo "== $s ($(date '+%Y-%m-%d %H:%M:%S'))"
    "stage_$s"
    : > "$WORK/stages/$s.done"
    echo "== $s done ($(date '+%Y-%m-%d %H:%M:%S'))"
  done
}

run_release() {
  local stages from s seen=0 before=1
  CHANNEL="$1"
  N="$2"
  case "$CHANNEL" in
    canary) stages="$CANARY_STAGES" ;;
    stable) stages="$STABLE_STAGES" ;;
  esac
  from="${PEGBOARD_RELEASE_FROM:-${stages%% *}}"
  for s in $stages; do
    if [ "$s" = "$from" ]; then
      seen=1
    fi
  done
  [ "$seen" -eq 1 ] || die "PEGBOARD_RELEASE_FROM=$from is not a stage of $CHANNEL ($stages)"
  if [ "$CHANNEL" = canary ]; then
    [ -n "${PEGBOARD_SRC:-}" ] || die "set PEGBOARD_SRC to the Chromium src directory"
    [ -d "$PEGBOARD_SRC" ] || die "PEGBOARD_SRC=$PEGBOARD_SRC is not a directory"
    PEGBOARD_SRC="$(cd "$PEGBOARD_SRC" && pwd)"
  fi
  WORK="$REPO_ROOT/target/release-pegboard/$N"
  mkdir -p "$WORK/stages"
  # The stages before the first one run must be done; it and the ones after it are not, until
  # they run again.
  for s in $stages; do
    if [ "$s" = "$from" ]; then
      before=0
    fi
    if [ "$before" -eq 1 ]; then
      [ -f "$WORK/stages/$s.done" ] ||
        die "PEGBOARD_RELEASE_FROM=$from needs the $s stage done first ($WORK/stages/$s.done is missing)"
    else
      rm -f "$WORK/stages/$s.done"
    fi
  done
  echo "== release.sh $CHANNEL $N from $from: $WORK" | tee -a "$WORK/release.log"
  run_stages "$from" "$stages" 2>&1 | tee -a "$WORK/release.log"
}

main() {
  check_build_jobs
  [ $# -ge 1 ] || usage
  case "$1" in
    preflight)
      [ $# -le 2 ] || usage
      [ $# -lt 2 ] || check_n "$2"
      [ -z "${PEGBOARD_RELEASE_FROM:-}" ] || die "PEGBOARD_RELEASE_FROM is for release.sh <canary|stable> <N>"
      preflight "${2:-}"
      ;;
    canary | stable)
      [ $# -eq 2 ] || usage
      check_n "$2"
      run_release "$1" "$2"
      ;;
    *) usage ;;
  esac
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
