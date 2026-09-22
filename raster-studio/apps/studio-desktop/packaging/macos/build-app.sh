#!/usr/bin/env bash
# Raster Studio — macOS .app bundle and .dmg builder.
#
# Run on a macOS machine, from any directory (the script cds to the cargo
# workspace itself):
#   cargo build --release -p studio-desktop
#   apps/studio-desktop/packaging/macos/build-app.sh
#
# Produces target/packaging/RasterStudio-<version>.dmg, with the third-party
# licence notices under Contents/Resources/LICENSES. Launching the .app on a
# clean machine is the release gate this script cannot check by itself.
set -euo pipefail

cd "$(dirname "$0")/../../../.."
# The desktop crate's version by name — `packages[0]` would be whichever
# workspace member cargo lists first, which is not this one.
VERSION=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys;print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="studio-desktop"))')
BIN=target/release/studio-desktop
APP=target/packaging/RasterStudio.app

test -x "$BIN" || { echo "build first: cargo build --release -p studio-desktop" >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/LICENSES"

sed -e "s/\${APP_VERSION}/$VERSION/g" \
    apps/studio-desktop/packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/studio-desktop"
cp -R LICENSES/. "$APP/Contents/Resources/LICENSES/"
codesign --force --sign - "$APP" 2>/dev/null || true # ad-hoc; release signing is P3.6

mkdir -p target/packaging
hdiutil create -volname "Raster Studio" -srcfolder "$APP" \
    -ov -format UDZO "target/packaging/RasterStudio-$VERSION.dmg"

echo "built target/packaging/RasterStudio-$VERSION.dmg"
