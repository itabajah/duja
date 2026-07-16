; Inno Setup script for the Duja Windows installer.
;
; Per-user install (no UAC): everything lands under the user's Programs folder
; and the autostart entry is HKCU, matching Duja's own in-app autostart exactly
; (duja-platform writes HKCU\...\Run value "Duja" = the quoted exe path), so the
; installer's "launch at login" task and the in-app toggle are one setting.
;
; The release workflow passes the version:  ISCC /DMyAppVersion=0.1.0 duja.iss
; Paths are relative to this file (packaging\windows\), so the repo root is ..\..
;
; Output:  ..\..\target\dist\duja-setup-<ver>.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

#define MyAppName "Duja"
#define MyAppPublisher "Duja contributors"
#define MyAppURL "https://github.com/itabajah/duja"
#define MyAppExeName "duja.exe"
#define MyAppAumId "io.github.itabajah.duja"

[Setup]
; A fixed AppId (never change it across releases) so upgrades and uninstall
; recognise a prior install. Generated once for Duja.
AppId={{7A1E4F2C-9B3D-4C6A-8E21-D0A100000001}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
; Per-user, no elevation. {autopf} resolves to the user's Programs folder here.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\Duja
DefaultGroupName=Duja
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE-MIT
OutputDir=..\..\target\dist
OutputBaseFilename=duja-setup-{#MyAppVersion}
SetupIconFile=..\..\crates\duja-app\assets\duja.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName} {#MyAppVersion}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes

[Tasks]
Name: "autostart"; Description: "Launch Duja automatically at login"; Flags: unchecked
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\..\target\release\duja.exe";    DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\dujactl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT";                DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE";             DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md";                  DestDir: "{app}"; Flags: ignoreversion

[Icons]
; AppUserModelID must match the process id Duja sets, so update toasts resolve
; the Duja identity for the installed copy.
Name: "{group}\Duja"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppAumId}"
Name: "{userdesktop}\Duja"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppAumId}"; Tasks: desktopicon
Name: "{group}\Uninstall Duja"; Filename: "{uninstallexe}"

[Registry]
; Mirrors duja-platform's autostart exactly: HKCU Run, value "Duja", quoted path.
; Removed on uninstall or when the task is unchecked on reinstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "Duja"; ValueData: """{app}\{#MyAppExeName}"""; \
  Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch Duja now"; \
  Flags: nowait postinstall skipifsilent
