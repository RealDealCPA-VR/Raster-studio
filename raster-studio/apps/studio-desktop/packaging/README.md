# Packaging

How each platform's installer is produced. Every command runs from the
repository root after `cargo build --release -p studio-desktop`.

| Platform | Command | Output |
| --- | --- | --- |
| Windows | `iscc apps/studio-desktop/packaging/raster-studio.iss` | `target/installer/RasterStudio-<version>-Setup.exe` (Inno Setup) |
| macOS | `apps/studio-desktop/packaging/macos/build-app.sh` | `target/packaging/RasterStudio-<version>.dmg` (`hdiutil`) |
| Linux | `apps/studio-desktop/packaging/linux/build-deb.sh` | `target/packaging/raster-studio_<version>_amd64.deb` (`dpkg-deb`) |

The macOS script assembles `RasterStudio.app` from `Info.plist` (version
substituted from Cargo) and an ad-hoc codesign; proper signing and
notarisation are the P3.6 step. The Linux script stages `usr/bin`,
a `.desktop` entry and an icon, then lets `dpkg-deb` do the arithmetic.

The release gate each script cannot check by itself — launching on a clean
machine — is what release CI (P3.5) exists to run on real runners.
