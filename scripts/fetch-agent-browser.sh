#!/usr/bin/env bash
# Fetch the pinned agent-browser build from the npm registry into vendor/agent-browser/
# and verify it against vendor/agent-browser.lock.
set -euo pipefail

VERSION="0.42.0"
URL="https://registry.npmjs.org/agent-browser/-/agent-browser-${VERSION}.tgz"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/agent-browser"
LOCK="$ROOT/vendor/agent-browser.lock"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$URL" -o "$tmp/ab.tgz"
tar -xzf "$tmp/ab.tgz" -C "$tmp" package/bin/agent-browser-darwin-arm64 package/LICENSE

actual="$(shasum -a 256 "$tmp/package/bin/agent-browser-darwin-arm64" | awk '{print $1}')"
expected=""
[ -f "$LOCK" ] && expected="$(tr -d '[:space:]' < "$LOCK")"

if [ -z "$expected" ]; then
  printf '%s\n' "$actual" > "$LOCK"
  echo "lock file was empty: recorded sha256 $actual"
elif [ "$expected" != "$actual" ]; then
  echo "checksum mismatch: expected $expected, got $actual" >&2
  exit 1
else
  echo "checksum ok"
fi

mkdir -p "$DEST"
install -m 0755 "$tmp/package/bin/agent-browser-darwin-arm64" "$DEST/agent-browser"
cp "$tmp/package/LICENSE" "$DEST/LICENSE"
