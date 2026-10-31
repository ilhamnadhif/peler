; Inno Setup script for the Windows installer (Peler-<version>-windows-x64-setup.exe).
;
;   iscc /DAppVersion=1.4.0 /DSourceExe=..\..\target\release\peler.exe /DOutputDir=..\..\dist packaging\windows\peler.iss
;
; Installs for the current user by default (no administrator rights needed), or for all users
; when chosen in the first dialog. An update installs over the previous version (same AppId)
; and closes a running Peler first. The in-app updater runs it with /SILENT /update=1, which
; opens Peler again when it is done.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\peler.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{7D3A9E2B-4C1F-4B6E-8A5D-2F9C0E1B6A47}
AppName=Peler
AppVersion={#AppVersion}
AppVerName=Peler {#AppVersion}
AppPublisher=Peler contributors
AppPublisherURL=https://github.com/ilhamnadhif/peler
AppSupportURL=https://github.com/ilhamnadhif/peler/issues
AppUpdatesURL=https://github.com/ilhamnadhif/peler/releases
DefaultDirName={autopf}\Peler
DefaultGroupName=Peler
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=peler.ico
UninstallDisplayIcon={app}\peler.exe
UninstallDisplayName=Peler
OutputDir={#OutputDir}
OutputBaseFilename=Peler-{#AppVersion}-windows-x64-setup
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
Name: "addtopath"; Description: "Add peler to PATH (open connections from a terminal: peler web-1)"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "peler.exe"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Peler"; Filename: "{app}\peler.exe"
Name: "{autodesktop}\Peler"; Filename: "{app}\peler.exe"; Tasks: desktopicon

[Registry]
; peler.exe on PATH (per user, or for all users in an administrative install).
Root: HKA; Subkey: "{code:EnvKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

[Run]
Filename: "{app}\peler.exe"; Description: "{cm:LaunchProgram,Peler}"; Flags: nowait postinstall skipifsilent
; After an update from the app: open Peler again, as the user (not elevated).
Filename: "{app}\peler.exe"; Flags: nowait runasoriginaluser; Check: IsAppUpdate

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
