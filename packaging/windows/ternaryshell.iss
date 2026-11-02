; Inno Setup script for the Windows installer (TernaryShell-<version>-windows-x64-setup.exe).
;
;   iscc /DAppVersion=1.4.0 /DSourceExe=..\..\target\release\ternaryshell.exe /DOutputDir=..\..\dist packaging\windows\ternaryshell.iss
;
; Installs for the current user by default (no administrator rights needed), or for all users
; when chosen in the first dialog. An update installs over the previous version (same AppId)
; and closes a running TernaryShell first. The in-app updater runs it with /SILENT /update=1, which
; opens TernaryShell again when it is done.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\ternaryshell.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{E18F47EC-5E3F-4881-A972-5F64305785A5}
AppName=TernaryShell
AppVersion={#AppVersion}
AppVerName=TernaryShell {#AppVersion}
AppPublisher=TernaryShell contributors
AppPublisherURL=https://github.com/ilhamnadhif/ternaryshell
AppSupportURL=https://github.com/ilhamnadhif/ternaryshell/issues
AppUpdatesURL=https://github.com/ilhamnadhif/ternaryshell/releases
DefaultDirName={autopf}\TernaryShell
DefaultGroupName=TernaryShell
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=ternaryshell.ico
UninstallDisplayIcon={app}\ternaryshell.exe
UninstallDisplayName=TernaryShell
OutputDir={#OutputDir}
OutputBaseFilename=TernaryShell-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add ternaryshell to PATH (open connections from a terminal: ternaryshell web-1)"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "ternaryshell.exe"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\TernaryShell"; Filename: "{app}\ternaryshell.exe"
Name: "{autodesktop}\TernaryShell"; Filename: "{app}\ternaryshell.exe"; Tasks: desktopicon

[Registry]
; ternaryshell.exe on PATH (per user, or for all users in an administrative install).
Root: HKA; Subkey: "{code:EnvKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

[Run]
Filename: "{app}\ternaryshell.exe"; Description: "{cm:LaunchProgram,TernaryShell}"; Flags: nowait postinstall skipifsilent
; After an update from the app: open TernaryShell again, as the user (not elevated).
Filename: "{app}\ternaryshell.exe"; Flags: nowait runasoriginaluser; Check: IsAppUpdate

[Code]
function IsAppUpdate: Boolean;
begin
  Result := ExpandConstant('{param:update|0}') = '1';
end;

function EnvKey(Param: String): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Paths: String;
  RootKey: Integer;
begin
  if IsAdminInstallMode then RootKey := HKEY_LOCAL_MACHINE else RootKey := HKEY_CURRENT_USER;
  if not RegQueryStringValue(RootKey, EnvKey(''), 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;
