#!/usr/bin/env bash
# Builds Bird.app (the menu bar app) into app/build/. It installs nothing:
# move it to /Applications yourself if you want to use that build. Releases
# update the Bird.app in /Applications (`bird self-update`).
# Needs only the Xcode command line tools. The app is signed ad hoc, which is
# enough to run it on this Mac.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

swift build -c release
bin="$(swift build -c release --show-bin-path)/BirdBar"

app="$here/build/Bird.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Bird"
cp "$here/Info.plist" "$app/Contents/Info.plist"
# The app icon: icon-art.webp placed on the macOS icon grid by make-icon.swift.
rm -rf "$here/build/Bird.iconset"
swift "$here/make-icon.swift" "$here/icon-art.webp" "$here/build/Bird.iconset"
iconutil -c icns -o "$app/Contents/Resources/Bird.icns" "$here/build/Bird.iconset"
codesign --force --sign - --options runtime "$app"
echo "built $app"
