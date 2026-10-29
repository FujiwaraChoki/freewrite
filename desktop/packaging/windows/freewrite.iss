; Inno Setup script for the Windows installer.
; Build the exe first (cargo build --release), then: iscc packaging\windows\freewrite.iss

#define AppVersion GetEnv("FREEWRITE_VERSION")
#if AppVersion == ""
  #define AppVersion "1.0.0"
#endif

[Setup]
AppId={{8C4E2D1A-6F3B-4B8E-9A57-2E1D0F6C9B41}
AppName=freewrite
AppVersion={#AppVersion}
AppPublisher=freewrite
AppPublisherURL=https://www.freewrite.io/
DefaultDirName={autopf}\freewrite
DefaultGroupName=freewrite
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\..\target\installer
OutputBaseFilename=freewrite-setup-{#AppVersion}
SetupIconFile=freewrite.ico
UninstallDisplayIcon={app}\freewrite.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\freewrite.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\freewrite"; Filename: "{app}\freewrite.exe"
Name: "{autodesktop}\freewrite"; Filename: "{app}\freewrite.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\freewrite.exe"; Description: "{cm:LaunchProgram,freewrite}"; Flags: nowait postinstall skipifsilent
