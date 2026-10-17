#!/usr/bin/env bash
# Builds Bird.app (the menu bar app) into app/build/.
#   app/build.sh             build
#   app/build.sh --install   build, copy to ~/Applications and start it
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

if [ "${1:-}" = "--install" ]; then
    target="$HOME/Applications/Bird.app"
    mkdir -p "$HOME/Applications"
    # Quit a running copy first, so the new one starts fresh.
    pkill -x Bird 2>/dev/null || true
    rm -rf "$target"
    cp -R "$app" "$target"
    open "$target"
    echo "installed $target"
fi
