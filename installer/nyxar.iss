; Установщик Nyxar. Версию передаёт CI: ISCC /DAppVersion=1.2.3 installer\nyxar.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{B7F3A6C2-5D41-4E0B-9C1A-6E2D8F4A1B73}
AppName=Nyxar
AppVersion={#AppVersion}
AppPublisher=Fersiya
DefaultDirName={autopf}\Nyxar
DefaultGroupName=Nyxar
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=Nyxar-Setup-{#AppVersion}
Compression=lzma2/ultra
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\nyxar.exe
WizardStyle=modern

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\dist\nyxar.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Nyxar"; Filename: "{app}\nyxar.exe"
Name: "{autodesktop}\Nyxar"; Filename: "{app}\nyxar.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\nyxar.exe"; Description: "{cm:LaunchProgram,Nyxar}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "schtasks.exe"; Parameters: "/Delete /TN Nyxar /F"; Flags: runhidden; RunOnceId: "DeleteAutostartTask"
