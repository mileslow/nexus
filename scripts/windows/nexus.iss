; The Windows installer, built by scripts\bundle.ps1 with Inno Setup.
; It installs for the current user only, so nothing asks for an
; administrator and the app can replace itself when it updates.

#ifndef AppVersion
  #error Pass the version with /DAppVersion=1.2.3
#endif

[Setup]
AppId={{6C0B4B0E-6E0B-4B43-9F0C-2A5D2E3C7A11}
AppName=Nexus
AppVersion={#AppVersion}
AppVerName=Nexus {#AppVersion}
AppPublisher=github.com/mileslow/nexus
AppPublisherURL=https://github.com/mileslow/nexus
AppSupportURL=https://github.com/mileslow/nexus/issues
AppUpdatesURL=https://github.com/mileslow/nexus/releases
VersionInfoVersion={#AppVersion}.0
PrivilegesRequired=lowest
DefaultDirName={autopf}\Nexus
DisableProgramGroupPage=yes
DisableDirPage=yes
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=Nexus-Setup
SetupIconFile=..\..\assets\Nexus.ico
UninstallDisplayIcon={app}\Nexus.exe
UninstallDisplayName=Nexus
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked
Name: "explorer"; Description: "Add ""Open with Nexus"" to the folder menu in Explorer"

[Files]
Source: "..\..\dist\Nexus.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Nexus"; Filename: "{app}\Nexus.exe"
Name: "{autodesktop}\Nexus"; Filename: "{app}\Nexus.exe"; Tasks: desktopicon

[Registry]
; Right-click a folder, or the background of an open folder.
Root: HKA; Subkey: "Software\Classes\Directory\shell\Nexus"; ValueType: string; ValueName: ""; ValueData: "Open with Nexus"; Flags: uninsdeletekey; Tasks: explorer
Root: HKA; Subkey: "Software\Classes\Directory\shell\Nexus"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Nexus.exe"""; Tasks: explorer
Root: HKA; Subkey: "Software\Classes\Directory\shell\Nexus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Nexus.exe"" ""%V"""; Tasks: explorer
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nexus"; ValueType: string; ValueName: ""; ValueData: "Open with Nexus"; Flags: uninsdeletekey; Tasks: explorer
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nexus"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Nexus.exe"""; Tasks: explorer
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nexus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Nexus.exe"" ""%V"""; Tasks: explorer

[Run]
Filename: "{app}\Nexus.exe"; Description: "{cm:LaunchProgram,Nexus}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; What an in-app update leaves until the next start.
Type: files; Name: "{app}\.Nexus.exe.previous"
