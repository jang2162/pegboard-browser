#!/usr/bin/env bash
# Regenerates the temporary app icon (mac/app.icns, mac/Assets.car) from icon.swift.
# Needs Xcode (swift, actool) and iconutil. The outputs are committed; run this only to
# change the icon.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

swiftc -O -o "$tmp/icon" "$here/icon.swift"

iconset="$tmp/app.iconset"
appicon="$tmp/Assets.xcassets/AppIcon.appiconset"
mkdir -p "$iconset" "$appicon"
printf '{"info":{"author":"xcode","version":1}}\n' > "$tmp/Assets.xcassets/Contents.json"

images=""
for pt in 16 32 128 256 512; do
  for scale in 1 2; do
    px=$((pt * scale))
    if [ "$scale" -eq 1 ]; then name="icon_${pt}x${pt}.png"; else name="icon_${pt}x${pt}@2x.png"; fi
    "$tmp/icon" "$px" "$iconset/$name"
    cp "$iconset/$name" "$appicon/$name"
    images="$images{\"idiom\":\"mac\",\"size\":\"${pt}x${pt}\",\"scale\":\"${scale}x\",\"filename\":\"$name\"},"
  done
done
printf '{"images":[%s],"info":{"author":"xcode","version":1}}\n' "${images%,}" > "$appicon/Contents.json"

mkdir -p "$here/mac" "$tmp/car"
iconutil -c icns -o "$here/mac/app.icns" "$iconset"
xcrun actool --compile "$tmp/car" --platform macosx --minimum-deployment-target 12.0 \
  --app-icon AppIcon --output-partial-info-plist "$tmp/partial.plist" \
  --output-format human-readable-text --errors --warnings "$tmp/Assets.xcassets" > "$tmp/actool.log"
cp "$tmp/car/Assets.car" "$here/mac/Assets.car"
echo "wrote $here/mac/app.icns and $here/mac/Assets.car"
