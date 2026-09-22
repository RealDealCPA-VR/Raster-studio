; Raster Studio — Inno Setup script (Windows installer).
;
; Build after `cargo build --release -p studio-desktop`. Every path below is
; relative to this file, so the script runs from any directory; release CI
; runs it from the cargo workspace (raster-studio/):
;   iscc /DAppVersion=<version> apps\studio-desktop\packaging\raster-studio.iss
;
; `/DAppVersion` is the crate version from Cargo (`cargo metadata`), which is
; what CI passes. Without it the fallback below is used — keep that in step
; with apps/studio-desktop/Cargo.toml so a hand-built installer is still
; distinguishable from the last one.
;
; Produces a slim installer that drops the binary, the third-party licence
; notices, a Start-menu shortcut and an uninstaller. The executable already
; carries its icon and VERSIONINFO (embedded by apps/studio-desktop/build.rs),
; so Explorer, the taskbar and Add/Remove Programs show the right face.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#define AppName "Raster Studio"
#define AppExe "studio-desktop.exe"

[Setup]
AppId={{D9B2A1E7-4C3F-4A0B-9E26-7B0A35C0F1A2}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Raster Studio
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2
SolidCompression=yes
; ..\..\.. from packaging/ is the cargo workspace root (raster-studio/), where
; target/, assets/ and LICENSES/ live.
OutputDir=..\..\..\target\installer
OutputBaseFilename=RasterStudio-{#AppVersion}-Setup
SetupIconFile=..\..\..\assets\raster-studio.ico
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern

[Files]
Source: "..\..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; The third-party notices ship next to the binary they describe.
Source: "..\..\..\LICENSES\*"; DestDir: "{app}\LICENSES"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
