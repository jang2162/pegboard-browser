#!/usr/bin/env bash
# Fetch the pinned agent-browser build from the npm registry into vendor/agent-browser/ and
# verify it against vendor/agent-browser.lock.
#
# The lock has one line per checked file, "<sha256>  <name>":
#   <sha256>  agent-browser-<version>.tgz         the npm package, checked before unpacking
#   <sha256>  bin/agent-browser-darwin-arm64      the binary taken from it
# A line that is missing is recorded (and said so: review and commit it), except with
# --locked, which fails instead; the release (release.sh) runs only that form.
#
# Besides the binary, the package's license files are kept in vendor/agent-browser/licenses/
# at their paths in the package (LICENSE*, LICENCE*, NOTICE*, THIRD_PARTY*, THIRD-PARTY*,
# COPYING* and everything in a licenses/ directory), for the app's notices.
set -euo pipefail

VERSION="0.42.0"
TARBALL="agent-browser-${VERSION}.tgz"
URL="https://registry.npmjs.org/agent-browser/-/${TARBALL}"
BINARY="bin/agent-browser-darwin-arm64"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/agent-browser"
LOCK="$ROOT/vendor/agent-browser.lock"

die() {
  echo "error: $*" >&2
  exit 1
}

locked=0
case "${1:-}" in
  "") ;;
  --locked) locked=1 ;;
  *) die "usage: $(basename "$0") [--locked]" ;;
esac
[ $# -le 1 ] || die "usage: $(basename "$0") [--locked]"

# The lock's SHA-256 for a name, empty when it has none. Every line must be well formed, and
# no name may have two lines.
lock_sum() {
  local want="$1" sum name rest seen=" "
  [ -f "$LOCK" ] || return 0
  while read -r sum name rest; do
    [ -n "$sum" ] || continue
    [[ "$sum" =~ ^[0-9a-f]{64}$ ]] && [ -n "$name" ] && [ -z "$rest" ] ||
      die "vendor/agent-browser.lock: a line is not '<sha256>  <name>': $sum $name $rest"
    case "$name" in
      "$TARBALL" | "$BINARY") ;;
      *) die "vendor/agent-browser.lock names $name, not $TARBALL or $BINARY (another version?)" ;;
    esac
    case "$seen" in
      *" $name "*) die "vendor/agent-browser.lock has two lines for $name" ;;
    esac
    seen="$seen$name "
    if [ "$name" = "$want" ]; then
      echo "$sum"
      return 0
    fi
  done < "$LOCK"
}

recorded=0
# Checks a file against the lock, or records it there when the lock has no line for it.
check() {
  local name="$1" file="$2" expected actual
  actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  expected="$(lock_sum "$name")"
  if [ -z "$expected" ]; then
    [ "$locked" -eq 0 ] || die "vendor/agent-browser.lock has no sha256 for $name (--locked)"
    printf '%s  %s\n' "$actual" "$name" >> "$LOCK"
    echo "recorded sha256 $actual of $name in vendor/agent-browser.lock"
    recorded=1
  elif [ "$expected" != "$actual" ]; then
    die "checksum mismatch for $name: expected $expected, got $actual"
  else
    echo "checksum ok ($name)"
  fi
}

# Validates the whole lock before anything is downloaded.
lock_sum "" > /dev/null

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/download" "$tmp/unpacked"

curl -fsSL --proto '=https' --tlsv1.2 "$URL" -o "$tmp/download/$TARBALL"
check "$TARBALL" "$tmp/download/$TARBALL"

# The package's license files: by name, never by content.
licenses=()
while IFS= read -r member; do
  case "$member" in
    package/*) ;;
    *) continue ;;
  esac
  [ "${member%/}" = "$member" ] || continue
  case "/$member/" in
    */../* | */./* | *//*) die "$TARBALL has a strange member name: $member" ;;
  esac
  base="$(basename "$member")"
  if grep -Eiq '^(licen[cs]e|notice|third[-_]party|copying)' <<< "$base" ||
    grep -Eiq '/licen[cs]es/' <<< "/$member"; then
    licenses+=("$member")
  fi
done < <(tar -tzf "$tmp/download/$TARBALL")
[ "${#licenses[@]}" -gt 0 ] || die "$TARBALL has no license file"

# The whole package into the new empty directory (no member names on the command line: tar
# would read them as patterns), then the chosen files by their literal paths. Each must be a
# regular file, never a link: a link's target is not what the lock or the notices vouch for.
tar -xzf "$tmp/download/$TARBALL" -C "$tmp/unpacked"
for member in "package/$BINARY" "${licenses[@]}"; do
  [ -f "$tmp/unpacked/$member" ] && [ ! -L "$tmp/unpacked/$member" ] ||
    die "$TARBALL: $member is not a regular file"
done
check "$BINARY" "$tmp/unpacked/package/$BINARY"

mkdir -p "$DEST"
install -m 0755 "$tmp/unpacked/package/$BINARY" "$DEST/agent-browser"
rm -f "$DEST/LICENSE"
if [ -f "$tmp/unpacked/package/LICENSE" ]; then
  cp "$tmp/unpacked/package/LICENSE" "$DEST/LICENSE"
fi
rm -rf "$DEST/licenses"
for member in "${licenses[@]}"; do
  rel="${member#package/}"
  mkdir -p "$DEST/licenses/$(dirname "$rel")"
  cp "$tmp/unpacked/$member" "$DEST/licenses/$rel"
done
echo "agent-browser $VERSION in vendor/agent-browser (${#licenses[@]} license files)"
if [ "$recorded" -eq 1 ]; then
  echo "review and commit vendor/agent-browser.lock"
fi
