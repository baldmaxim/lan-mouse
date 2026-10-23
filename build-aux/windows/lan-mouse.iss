; Inno Setup 6 script for Lan Mouse (per-user install, no admin rights).
; Build: iscc /DAppVersion=X.Y.Z build-aux\windows\lan-mouse.iss
; Expects the assembled folder "lan-mouse-windows" (exe + GTK DLLs) in the repo root.

#ifndef AppVersion
  #error Pass the version with /DAppVersion=X.Y.Z
#endif

#define AppName "Lan Mouse"
#define AppExe "lan-mouse.exe"

[Setup]
; Fixed GUID: identifies the app for upgrades and uninstall. Never change it.
AppId={{072E70A5-EEB7-4290-ABD9-94D277146A1C}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=baldmaxim
AppPublisherURL=https://github.com/baldmaxim/lan-mouse
AppSupportURL=https://github.com/baldmaxim/lan-mouse/issues
AppUpdatesURL=https://github.com/baldmaxim/lan-mouse/releases
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Lan Mouse
DisableProgramGroupPage=yes
DisableDirPage=auto
SetupIconFile=..\lan-mouse.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
OutputDir=..\..
OutputBaseFilename=LanMouse-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
english.AutoStart=Start Lan Mouse when I sign in to Windows (in the tray)
russian.AutoStart=Запускать Lan Mouse при входе в Windows (в трее)

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "autostart"; Description: "{cm:AutoStart}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
; install/uninstall scripts are for the portable zip only
Source: "..\..\lan-mouse-windows\*"; DestDir: "{app}"; Excludes: "install.cmd,install.ps1,uninstall.ps1"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{userstartup}\{#AppName}"; Filename: "{app}\{#AppExe}"; Parameters: "--hidden"; WorkingDir: "{app}"; Tasks: autostart

[Run]
; interactive install: "Launch Lan Mouse" checkbox on the last page
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
; silent install (in-app update): start back in the tray
Filename: "{app}\{#AppExe}"; Parameters: "--hidden"; WorkingDir: "{app}"; Flags: nowait; Check: WizardSilent

[Code]
// Stops running instances (GUI and its daemon child) so files can be replaced.
procedure StopLanMouse();
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopLanMouse();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopLanMouse();
  Result := True;
end;
