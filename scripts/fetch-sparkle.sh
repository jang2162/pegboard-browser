#!/usr/bin/env bash
# Fetch the pinned Sparkle release (the updater framework bundled into the release app) from
# its GitHub release into vendor/sparkle/, after checking the archive against
# vendor/sparkle.lock (one line: the archive's SHA-256).
#
# vendor/sparkle/ then holds Sparkle.framework (goes into the app), bin/ (generate_keys,
# sign_update: used on the build machine only, never bundled) and LICENSE.
#
# An empty or missing lock file is not filled in silently: the script records the archive's
# SHA-256, prints "review and commit" and fails, so a person looks at the first pin (compare
# it with the digest on the release page) before anything uses it.
set -euo pipefail

# The newest Sparkle 2 release that is at least two weeks old when pinned (2.10.1, released
# 2026-10-25; 2.9 or later is needed for signed appcasts).
SPARKLE_VERSION="2.10.1"
ARCHIVE="Sparkle-${SPARKLE_VERSION}.tar.xz"
URL="https://github.com/sparkle-project/Sparkle/releases/download/${SPARKLE_VERSION}/${ARCHIVE}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/sparkle"
LOCK="$ROOT/vendor/sparkle.lock"

die() {
  echo "error: $*" >&2
  exit 1
}

[ $# -eq 0 ] || die "usage: $(basename "$0") (no arguments)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/download" "$tmp/unpacked"

curl -fsSL --proto '=https' --tlsv1.2 "$URL" -o "$tmp/download/$ARCHIVE"
actual="$(shasum -a 256 "$tmp/download/$ARCHIVE" | awk '{print $1}')"

expected=""
[ -f "$LOCK" ] && expected="$(tr -d '[:space:]' < "$LOCK")"
if [ -z "$expected" ]; then
  printf '%s\n' "$actual" > "$LOCK"
  echo "vendor/sparkle.lock was empty: recorded sha256 $actual of $ARCHIVE"
  echo "review and commit: compare it with the digest on the release page of Sparkle $SPARKLE_VERSION, then run this again"
  exit 1
fi
[[ "$expected" =~ ^[0-9a-f]{64}$ ]] || die "vendor/sparkle.lock is not one SHA-256: $expected"
[ "$expected" = "$actual" ] || die "checksum mismatch for $ARCHIVE: expected $expected, got $actual"
echo "checksum ok ($ARCHIVE)"

# Unpacked only after the check, in a new empty directory.
tar -xJf "$tmp/download/$ARCHIVE" -C "$tmp/unpacked"
for want in Sparkle.framework bin/sign_update bin/generate_keys LICENSE; do
  [ -e "$tmp/unpacked/$want" ] || die "$ARCHIVE has no $want"
done
[ ! -L "$tmp/unpacked/Sparkle.framework" ] || die "$ARCHIVE: Sparkle.framework is a symbolic link"

stage="$DEST.new"
rm -rf "$stage"
mkdir -p "$stage"
# ditto keeps the framework's symbolic links and its code signature.
ditto "$tmp/unpacked/Sparkle.framework" "$stage/Sparkle.framework"
ditto "$tmp/unpacked/bin" "$stage/bin"
cp "$tmp/unpacked/LICENSE" "$stage/LICENSE"
printf '%s\n' "$SPARKLE_VERSION" > "$stage/VERSION"
rm -rf "$DEST"
mv "$stage" "$DEST"
echo "Sparkle $SPARKLE_VERSION in vendor/sparkle"
