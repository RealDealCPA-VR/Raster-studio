#!/usr/bin/env bash
# Raster Studio — macOS .app bundle and .dmg builder.
#
# Run from the repository root on a macOS machine:
#   cargo build --release -p studio-desktop
#   apps/studio-desktop/packaging/macos/build-app.sh
#
# Produces target/packaging/RasterStudio-<version>.dmg. Launching the .app on
# a clean machine is the release gate this script cannot check by itself.
set -euo pipefail

cd "$(dirname "$0")/../../.."
VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["packages"][0]["version"])')
BIN=target/release/studio-desktop
APP=target/packaging/RasterStudio.app

test -x "$BIN" || { echo "build first: cargo build --release -p studio-desktop" >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

APP_VERSION="$VERSION" sed -e "s/\${APP_VERSION}/$VERSION/g" \
    apps/studio-desktop/packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/studio-desktop"
codesign --force --sign - "$APP" 2>/dev/null || true # ad-hoc; release signing is P3.6

mkdir -p target/packaging/dmg-blank
hdiutil create -volname "Raster Studio" -srcfolder "$APP" \
    -ov -format UDZO "target/packaging/RasterStudio-$VERSION.dmg"

echo "built target/packaging/RasterStudio-$VERSION.dmg"
