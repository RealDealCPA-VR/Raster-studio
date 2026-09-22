#!/usr/bin/env bash
# Raster Studio — Linux .deb builder.
#
# Run on a Linux machine, from any directory (the script cds to the cargo
# workspace itself):
#   cargo build --release -p studio-desktop
#   apps/studio-desktop/packaging/linux/build-deb.sh
#
# Produces target/packaging/raster-studio_<version>_amd64.deb, with the
# third-party licence notices under /usr/share/doc/raster-studio. Installing
# and launching on a clean machine is the release gate this script cannot
# check by itself.
set -euo pipefail

cd "$(dirname "$0")/../../../.."
# The desktop crate's version by name — `packages[0]` would be whichever
# workspace member cargo lists first, which is not this one.
VERSION=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys;print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="studio-desktop"))')
BIN=target/release/studio-desktop
STAGE=target/packaging/deb/raster-studio

test -x "$BIN" || { echo "build first: cargo build --release -p studio-desktop" >&2; exit 1; }

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/icons/hicolor/256x256/apps" "$STAGE/usr/share/doc/raster-studio"

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

# Debian policy puts a package's licence texts under /usr/share/doc/<package>.
for f in LICENSES/*; do
    install -m 644 "$f" "$STAGE/usr/share/doc/raster-studio/$(basename "$f")"
done

test -f assets/raster-studio.png && \
    install -m 644 assets/raster-studio.png \
        "$STAGE/usr/share/icons/hicolor/256x256/apps/raster-studio.png" || true

dpkg-deb --build "$STAGE" "target/packaging/raster-studio_${VERSION}_amd64.deb"
echo "built target/packaging/raster-studio_${VERSION}_amd64.deb"
