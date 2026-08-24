; Raster Studio — Inno Setup script (Windows installer).
;
; Build: run from the repository root after `cargo build --release -p studio-desktop`:
;   iscc raster-studio/apps/studio-desktop/packaging/raster-studio.iss
;
; Produces a slim installer that drops the binary, a Start-menu shortcut and an
; uninstaller. The version is read from the script constant below; bump it when
; the crate version changes so installs stay distinguishable.

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
OutputDir=..\..\..\target\installer
OutputBaseFilename=RasterStudio-{#AppVersion}-Setup
SetupIconFile=..\..\assets\raster-studio.ico
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern

[Files]
Source: "..\..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
