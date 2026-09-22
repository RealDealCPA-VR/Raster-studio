# Packaging

How each platform's installer is produced. Every command runs from the cargo
workspace (`raster-studio/`) after `cargo build --release -p studio-desktop`;
the two shell scripts `cd` there themselves, and the Inno Setup script's
paths are relative to the script file, so any current directory works.

| Platform | Command | Output |
| --- | --- | --- |
| Windows | `iscc /DAppVersion=<version> apps\studio-desktop\packaging\raster-studio.iss` | `target/installer/RasterStudio-<version>-Setup.exe` (Inno Setup) |
| macOS | `apps/studio-desktop/packaging/macos/build-app.sh` | `target/packaging/RasterStudio-<version>.dmg` (`hdiutil`) |
| Linux | `apps/studio-desktop/packaging/linux/build-deb.sh` | `target/packaging/raster-studio_<version>_amd64.deb` (`dpkg-deb`) |

`<version>` is the `studio-desktop` crate version; the shell scripts read it
from `cargo metadata` by package name, and release CI passes the same value
to `iscc` (without `/DAppVersion` the `.iss` falls back to its own constant).
All three bundle `LICENSES/` (the third-party notices): under the install
directory on Windows, `Contents/Resources/LICENSES` in the `.app`, and
`/usr/share/doc/raster-studio` in the `.deb`.

The Windows executable carries its own icon and `VERSIONINFO` — embedded by
`apps/studio-desktop/build.rs` through `embed-resource`, which needs the
Windows SDK's `rc.exe`; a release build fails without it rather than ship an
executable with no icon and no version. The macOS script assembles
`RasterStudio.app` from `Info.plist` (version substituted from Cargo) and an
ad-hoc codesign; proper signing and notarisation are the P3.6 step. (No
`.icns` exists yet, so the `CFBundleIconFile` entry names a file that is not
in the bundle.) The Linux script stages `usr/bin`, a `.desktop` entry and an
icon, then lets `dpkg-deb` do the arithmetic.

The release gate each script cannot check by itself — launching on a clean
machine — is what release CI (P3.5) exists to run on real runners. The
`release` job runs when a `v*` tag is pushed (the workflow's `on.push` lists
`tags: ['v*']`; without that entry a tag push would not start the workflow at
all), or on a manual `workflow_dispatch` with `release_dry_run` ticked, which
builds and uploads all three without a tag. The installers are artifacts of
the workflow run; nothing publishes them to a GitHub Release. As of this
writing the job has not yet run.

## Signing (P3.6)

Both platforms' signatures need certificates the build machine holds as CI
secrets. The workflow's Windows step is wired: with `WINDOWS_CERT` (the
`.pfx`, base64) and `WINDOWS_CERT_PASSWORD` configured it runs the `signtool`
commands below and `verify /pa` gates the step; without them the step
summary says **UNSIGNED** and passes. The macOS step is *not* wired: without
`APPLE_ID` it says **UNSIGNED (ad-hoc)** and passes; with it configured it
fails on purpose until the sequence below is in the workflow, so a dmg never
looks notarised when it is not. The commands, so a maintainer with the
certificates can execute them:

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
