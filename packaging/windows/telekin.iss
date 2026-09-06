; Telekin viewer installer for Windows 10/11 (Inno Setup 6).
;
; Build:  ISCC.exe /DAppVersion=1.0.0 packaging\windows\telekin.iss
;         (packaging\build-windows.ps1 does this after `cargo build --release`)
;
; Upgrades in place: the AppId below is what Inno uses to recognise an existing
; install, so running a newer setup over an older one replaces it, keeps the
; shortcuts, and leaves one entry in Apps & Features. Never change the AppId.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "Telekin"
#define AppPublisher "IRiSH LAB"
#define AppURL "https://github.com/phuwanat-vg/telekin"
#define AppExe "telekin.exe"
#define Root "..\.."

[Setup]
AppId={{7D6C1B9E-4F0A-4E4B-9C1D-2A9E5B8C3F11}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
SetupIconFile={#Root}\packaging\icons\telekin.ico
OutputDir={#Root}\dist
OutputBaseFilename=telekin-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; Per-user by default, with an admin install offered: a lab machine is often
; not one the operator can elevate on, and the app needs nothing system-wide.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; If the viewer is running during an upgrade, close it rather than fail.
CloseApplications=yes
RestartApplications=no
DisableProgramGroupPage=yes
LicenseFile={#Root}\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#Root}\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\packaging\icons\telekin.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\telekin.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\telekin.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
