#!/usr/bin/env bash
# Raster Studio — Linux .deb builder.
#
# Run from the repository root on a Linux machine:
#   cargo build --release -p studio-desktop
#   apps/studio-desktop/packaging/linux/build-deb.sh
#
# Produces target/packaging/raster-studio_<version>_amd64.deb. Installing and
# launching on a clean machine is the release gate this script cannot check by
# itself.
set -euo pipefail

cd "$(dirname "$0")/../../../.."
VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["packages"][0]["version"])')
BIN=target/release/studio-desktop
STAGE=target/packaging/deb/raster-studio

test -x "$BIN" || { echo "build first: cargo build --release -p studio-desktop" >&2; exit 1; }

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/icons/hicolor/256x256/apps"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: raster-studio
Version: $VERSION
Section: graphics
Priority: optional
Architecture: amd64
Depends: libx11-6, libwayland-client0, libxkbcommon0
Maintainer: Raster Studio <raster-studio@localhost>
Description: A layered raster editor
 Layered editing with adjustment layers, masks, selections, filters and a
 non-destructive GPU pipeline.
EOF

install -m 755 "$BIN" "$STAGE/usr/bin/raster-studio"
cat > "$STAGE/usr/share/applications/raster-studio.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Raster Studio
GenericName=Raster Editor
Exec=raster-studio %F
Icon=raster-studio
Categories=Graphics;RasterGraphics;
MimeType=image/png;image/jpeg;image/webp;image/tiff;
EOF

test -f assets/raster-studio.png && \
    install -m 644 assets/raster-studio.png \
        "$STAGE/usr/share/icons/hicolor/256x256/apps/raster-studio.png" || true

dpkg-deb --build "$STAGE" "target/packaging/raster-studio_${VERSION}_amd64.deb"
echo "built target/packaging/raster-studio_${VERSION}_amd64.deb"
