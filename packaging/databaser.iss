; Inno Setup script for Databaser.
;
; Installs per-user (no UAC) into %LOCALAPPDATA%\Programs\Databaser. That
; location is writable by the user, which is what lets the app's own updater
; (self_update, see src/update.rs) replace databaser.exe in place afterwards.
; Keep this directory stable: the uninstall record and the no-prompt scheduled
; task both reference the installed executable path.

#define MyAppName "Databaser"
#define MyAppPublisher "AbrahamX3"
#define MyAppExeName "databaser.exe"
#define MyAppUrl "https://github.com/AbrahamX3/databaser"

; Overridden by CI: ISCC.exe /DMyAppVersion=1.2.3
#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

[Setup]
AppId={{B7B3F2A1-4C0E-4D9A-9E6B-2F5C1A7D3E84}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppUrl}
AppSupportURL={#MyAppUrl}/issues
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; Per-user install only: never offer an all-users install, which would need
; elevation and break in-place self-updates.
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=Databaser-Setup-{#MyAppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
LicenseFile=..\LICENSE.md
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Handle a running instance whose exe is locked, but let the app decide when to
; come back so we do not relaunch it mid-update.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
