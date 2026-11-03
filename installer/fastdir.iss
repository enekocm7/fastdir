; Inno Setup script for FastDir.
; Build: iscc /DAppVersion=0.3.0 /DSourceExe=..\target\release\fastdir.exe /DSourceCliExe=..\target\release\fastdir-cli.exe /DIconFile=..\target\fastdir.ico fastdir.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\fastdir.exe"
#endif
#ifndef SourceCliExe
  #define SourceCliExe "..\target\release\fastdir-cli.exe"
#endif
#ifndef IconFile
  #define IconFile "..\target\fastdir.ico"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "FastDir"
#define AppExe "fastdir.exe"
#define AppPublisher "enekocm7"
#define AppUrl "https://github.com/enekocm7/fastdir"

[Setup]
AppId={{6B4F2A31-9C1E-4E7A-B2D5-FA57D1A0C0DE}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default (no UAC prompt); users can pick all-users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=FastDir-{#AppVersion}-setup-x64
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
; FastDir relaunches itself after a silent self-update (see [Run]).
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "contextmenu"; Description: "Add ""Analyze with FastDir"" to the folder right-click menu"; GroupDescription: "Integration:"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
; Console build of the command line (`fastdir-cli scan ...`, `fastdir-cli mcp`).
Source: "{#SourceCliExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Explorer integration: right-click a folder or drive -> Analyze with FastDir.
Root: HKA; Subkey: "Software\Classes\Directory\shell\FastDir"; ValueType: string; ValueName: ""; ValueData: "Analyze with FastDir"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\FastDir"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\FastDir\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Drive\shell\FastDir"; ValueType: string; ValueName: ""; ValueData: "Analyze with FastDir"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Drive\shell\FastDir"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Drive\shell\FastDir\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: contextmenu

; The in-app Settings -> Integration toggle writes these under HKCU even when the
; installer's contextmenu task was not chosen. dontcreatekey: never created at
; install time, but removed on uninstall so no entry points at a deleted exe.
Root: HKCU; Subkey: "Software\Classes\Directory\shell\FastDir"; Flags: dontcreatekey uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Drive\shell\FastDir"; Flags: dontcreatekey uninsdeletekey

[UninstallDelete]
; In-app update download folder, in case the app was not relaunched to clean it.
Type: filesandordirs; Name: "{%TEMP}\FastDir-update"

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Silent in-app update (`/RELAUNCH=1`, passed by FastDir's updater): start the new version.
Filename: "{app}\{#AppExe}"; Flags: nowait skipifnotsilent; Check: RelaunchRequested

[Code]
function RelaunchRequested: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

{ Uninstall removes %APPDATA%\FastDir (config.toml, legacy settings.json).
  Interactive: ask whether to keep it. Silent: removed unless /KEEPDATA is passed. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
  Keep: Boolean;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  DataDir := ExpandConstant('{userappdata}\{#AppName}');
  if not DirExists(DataDir) then
    Exit;
  if UninstallSilent then
    Keep := ExpandConstant('{param:KEEPDATA|0}') <> '0'
  else
    Keep := MsgBox('Also remove your FastDir settings?' + #13#10 + DataDir,
      mbConfirmation, MB_YESNO or MB_DEFBUTTON1) = IDNO;
  if (not Keep) then
    DelTree(DataDir, True, True, True);
end;
