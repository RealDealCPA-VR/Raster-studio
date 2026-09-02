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

## Signing (P3.6)

Both platforms' signatures need certificates the build machine holds as CI
secrets (`WINDOWS_CERT`, `APPLE_ID` per the workflow hooks). The commands,
so a maintainer with the certificates can execute them:

**Windows (Authenticode)** — after `iscc` produces the installer:

```powershell
signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 \
    /f certificate.pfx /p <password> target/installer/RasterStudio-<version>-Setup.exe
signtool verify /pa /v target/installer/RasterStudio-<version>-Setup.exe
```

**macOS (codesign + notarisation)** — after `build-app.sh` assembles the app
(the script already ad-hoc-signs; a release replaces that identity):

```bash
codesign --force --options runtime --sign "Developer ID Application: <name>" \
    target/packaging/RasterStudio.app
codesign --verify --strict --verbose=2 target/packaging/RasterStudio.app
spctl --assess --type execute target/packaging/RasterStudio.app   # the gate: "accepted"
xcrun notarytool submit RasterStudio-<version>.dmg \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PW" --wait
xcrun stapler staple RasterStudio-<version>.dmg
```

`spctl --assess` answering "accepted" is the macOS release gate; `signtool
verify /pa` is the Windows one. Without the certificates these steps are
host-bound — CI ships unsigned artifacts and says so.
