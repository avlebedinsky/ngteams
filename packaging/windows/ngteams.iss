; ngteams' per-user Windows installer (Inno Setup 7, scripts/release-innosetup.ps1 pins it).
;
; scripts/package-release-windows.ps1 compiles it from the same staged, hash-checked folder as the zip:
;   ISCC.exe /DAppVersion=0.1.0.123 /DStageDir=<stage> /DMarkerDir=<marker folder> /DOutputDir=<dist> ngteams.iss
; Optional signing (docs/ci.md#windows-code-signing): add /DSignTool=ngteams and /Sngteams=<sign command with $f>.
;
; - Per user, no administrator rights: %LOCALAPPDATA%\Programs\ngteams, Start menu, Add/Remove programs and the
;   msteams: link handler in HKCU only. Everything is named ngteams.
; - ngteams is offered as a handler for msteams: links in Settings > Apps > Default apps; Windows keeps the user's
;   current default (Microsoft Teams, say) until they change it.
; - WebView2 Runtime missing: Microsoft's Evergreen bootstrapper is downloaded, its Microsoft signature checked, then
;   run, as Install.ps1 does.
; - An upgrade replaces the previous version's program folders in place; an uninstall removes the program, its
;   shortcut and registrations, and keeps the accounts and data in %LOCALAPPDATA%\ngteams and Credential Manager.
; - The app's own updater (crates/teams-update) runs this installer with /VERYSILENT /UPDATEPID=<pid> /DIR=<folder>:
;   it waits for that process to exit, upgrades the folder and starts ngteams again.

#ifndef AppVersion
  #error Pass /DAppVersion=MAJOR.MINOR.PATCH.RUN
#endif
#ifndef StageDir
  #error Pass /DStageDir=<the staged release folder>
#endif
#ifndef MarkerDir
  #error Pass /DMarkerDir=<the folder holding update-format and release>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

; Fixed forever: Windows finds the installed copy, and an upgrade its predecessor, by this id.
#define AppGuid "{1150C52E-F222-4AA3-B1D0-EB09F8BBBFF7}"
#define UninstallKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\" + AppGuid + "_is1"
; The app's toast activator: SHA-256("ngteams.toast.activator:com.nickalie.ngteams")[0..16] as a GUID
; (activator_id in crates/teams-ui/src/desktop/windows/registration.rs).
#define ToastActivator "81B7D32D-75F7-6DB4-6A45-99728BC2AF17"
#define AppUserModelId "com.nickalie.ngteams"

[Setup]
AppId={{#AppGuid}
AppName=ngteams
AppVersion={#AppVersion}
AppVerName=ngteams {#AppVersion}
AppPublisher=ngteams
AppPublisherURL=https://github.com/nickalie/ngteams
AppSupportURL=https://github.com/nickalie/ngteams/issues
AppUpdatesURL=https://github.com/nickalie/ngteams/releases/latest
VersionInfoVersion={#AppVersion}
VersionInfoProductName=ngteams
VersionInfoProductVersion={#AppVersion}
VersionInfoCompany=ngteams
VersionInfoDescription=ngteams setup
SetupArchitecture=x64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
PrivilegesRequired=lowest
DefaultDirName={autopf}\ngteams
UsePreviousAppDir=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableWelcomePage=no
UninstallDisplayName=ngteams
UninstallDisplayIcon={app}\bin\ngteams.exe
SetupIconFile=ngteams.ico
WizardStyle=modern
WizardSmallImageFile=..\icons\hicolor\64x64\apps\com.nickalie.ngteams.png
WizardImageFile=..\icons\hicolor\128x128\apps\com.nickalie.ngteams.png
WizardImageStretch=no
OutputDir={#OutputDir}
OutputBaseFilename=ngteams-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
ShowLanguageDialog=no
#ifdef SignTool
SignTool={#SignTool}
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

; The previous version's program folders, so nothing it shipped and this one doesn't stays behind. Only in a folder an
; earlier ngteams installer marked; scripts/package-release-windows.ps1 refuses a stage with any other top-level name.
[InstallDelete]
Type: filesandordirs; Name: "{app}\bin"; Check: UpgradingHere
Type: filesandordirs; Name: "{app}\lib"; Check: UpgradingHere
Type: filesandordirs; Name: "{app}\libexec"; Check: UpgradingHere
Type: filesandordirs; Name: "{app}\share"; Check: UpgradingHere
Type: files; Name: "{app}\MEDIA-MANIFEST.json"; Check: UpgradingHere
Type: files; Name: "{app}\LICENSE-MIT"; Check: UpgradingHere
Type: files; Name: "{app}\LICENSE-APACHE"; Check: UpgradingHere

[Files]
; The zip's own setup, removal and README aren't part of an installed copy; its manifest lists them.
Source: "{#StageDir}\*"; DestDir: "{app}"; Excludes: "\Install.ps1,\Unregister.ps1,\README.txt,\PACKAGE-MANIFEST.json"; Flags: ignoreversion recursesubdirs createallsubdirs
; What the updater reads: how this copy was installed, and the release it is (crates/teams-update MARKER, IDENTITY).
Source: "{#MarkerDir}\update-format"; DestDir: "{app}\share\ngteams"; Flags: ignoreversion
Source: "{#MarkerDir}\release"; DestDir: "{app}\share\ngteams"; Flags: ignoreversion

[Icons]
; The app's toast notifications need its shortcut to carry its identity; the app sets the same on its first start.
Name: "{autoprograms}\ngteams"; Filename: "{app}\bin\ngteams.exe"; WorkingDir: "{app}\bin"; Comment: "ngteams"; AppUserModelID: "{#AppUserModelId}"; AppUserModelToastActivatorCLSID: "{#ToastActivator}"
Name: "{autodesktop}\ngteams"; Filename: "{app}\bin\ngteams.exe"; WorkingDir: "{app}\bin"; Comment: "ngteams"; Tasks: desktopicon

; The msteams: handler offered under Default apps, as Install.ps1 registers it. The msteams scheme itself and the
; user's choice for it are left alone.
[Registry]
Root: HKA; Subkey: "Software\Classes\ngteams.msteams"; ValueType: string; ValueData: "ngteams Teams meeting link"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\ngteams.msteams"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\ngteams.msteams\DefaultIcon"; ValueType: string; ValueData: """{app}\bin\ngteams.exe"",0"
Root: HKA; Subkey: "Software\Classes\ngteams.msteams\shell\open\command"; ValueType: string; ValueData: """{app}\bin\ngteams.exe"" ""%1"""
Root: HKA; Subkey: "Software\ngteams"; Flags: uninsdeletekeyifempty
Root: HKA; Subkey: "Software\ngteams\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "ngteams"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\ngteams\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "ngteams: chats, calls and meetings"
Root: HKA; Subkey: "Software\ngteams\Capabilities\URLAssociations"; ValueType: string; ValueName: "msteams"; ValueData: "ngteams.msteams"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "ngteams"; ValueData: "Software\ngteams\Capabilities"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\bin\ngteams.exe"; Description: "Open ngteams"; Flags: nowait postinstall skipifsilent
; The updater's run: the app it replaced starts again, as on Linux.
Filename: "{app}\bin\ngteams.exe"; Flags: nowait; Check: Updating

; Update downloads only: the accounts, settings and logs beside them stay.
[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\ngteams\data\updates"

[Code]
const
  WebView2Client = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2Bootstrapper = 'https://go.microsoft.com/fwlink/p/?LinkId=2124703';
  SYNCHRONIZE = $00100000;
  WAIT_FOR_EXIT = 60000;

var
  Upgrading: Boolean;
  Installed: Boolean;

function OpenProcess(Access: DWORD; Inherit: BOOL; ProcessId: DWORD): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';

{ The app's updater asked for this run: the pid of the app waiting to be replaced. }
function UpdatePid: String;
begin
  Result := ExpandConstant('{param:UPDATEPID|}');
end;

function Updating: Boolean;
begin
  Result := UpdatePid <> '';
end;

function UpgradingHere: Boolean;
begin
  Result := Upgrading;
end;

{ Waits for the updating app to exit, so its files can be replaced. Past the deadline, Restart Manager's close of the
  applications holding them (/CLOSEAPPLICATIONS) takes over. }
procedure WaitForUpdatingApp;
var
  Process: THandle;
  Pid: Integer;
begin
  Pid := StrToIntDef(UpdatePid, 0);
  if Pid <= 0 then
    Exit;
  Process := OpenProcess(SYNCHRONIZE, False, Pid);
  if Process <> 0 then
  begin
    WaitForSingleObject(Process, WAIT_FOR_EXIT);
    CloseHandle(Process);
  end;
end;

{ A newer ngteams installed already is not replaced by this older one, unless /ALLOWDOWNGRADE says so. }
function OlderThanInstalled: Boolean;
var
  Previous: String;
  PreviousVersion, ThisVersion: Int64;
begin
  Result := False;
  if ExpandConstant('{param:ALLOWDOWNGRADE|}') <> '' then
    Exit;
  if RegQueryStringValue(HKA, '{#UninstallKey}', 'DisplayVersion', Previous) and
    StrToVersion(Previous, PreviousVersion) and StrToVersion('{#AppVersion}', ThisVersion) then
    Result := ComparePackedVersion(PreviousVersion, ThisVersion) > 0;
end;

function InitializeSetup: Boolean;
begin
  Result := True;
  WaitForUpdatingApp;
  if OlderThanInstalled then
  begin
    SuppressibleMsgBox('A newer ngteams is installed already. To go back to version {#AppVersion}, uninstall ngteams first; your accounts and data are kept.',
      mbError, MB_OK, IDOK);
    Result := False;
  end;
end;

function WebView2Installed: Boolean;
var
  Version: String;
begin
  Result := (RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\' + WebView2Client, 'pv', Version) and
      (Version <> '') and (Version <> '0.0.0.0')) or
    (RegQueryStringValue(HKCU, 'Software\' + WebView2Client, 'pv', Version) and (Version <> '') and
      (Version <> '0.0.0.0'));
end;

{ Whether Authenticode says FileName is validly signed by Microsoft Corporation, as Install.ps1 checks it. }
function MicrosoftSigned(const FileName: String): Boolean;
var
  Literal, Command: String;
  ResultCode: Integer;
begin
  Literal := FileName;
  StringChangeEx(Literal, '''', '''''', True);
  Command := '$s = Get-AuthenticodeSignature -LiteralPath ''' + Literal + '''; ' +
    'if ($s.Status -eq ''Valid'' -and $s.SignerCertificate.Subject -match ''(^|,\s*)O=Microsoft Corporation(,|$)'') { exit 0 }; exit 1';
  Result := Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Command + '"', '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
end;

{ Microsoft's Evergreen WebView2 bootstrapper, checked and run; the reason it couldn't be, or ''. }
function InstallWebView2: String;
var
  Bootstrapper: String;
  ResultCode: Integer;
begin
  Result := '';
  try
    DownloadTemporaryFile(WebView2Bootstrapper, 'MicrosoftEdgeWebview2Setup.exe', '', nil);
  except
    Result := 'ngteams signs in through the Microsoft WebView2 Runtime, which is missing, and its installer could not be downloaded (' +
      GetExceptionMessage + '). Connect to the internet and run setup again, or install the WebView2 Runtime first.';
    Exit;
  end;
  Bootstrapper := ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe');
  if not MicrosoftSigned(Bootstrapper) then
    Result := 'The downloaded WebView2 Runtime installer is not signed by Microsoft, so it was not run. Install the WebView2 Runtime yourself and run setup again.'
  else if not Exec(Bootstrapper, '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or
    (ResultCode <> 0) or not WebView2Installed then
    Result := 'The Microsoft WebView2 Runtime could not be installed. Install or repair it (your IT department may need to), then run setup again.';
  DeleteFile(Bootstrapper);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Upgrading := FileExists(ExpandConstant('{app}\share\ngteams\update-format'));
  Result := '';
  if not WebView2Installed then
    Result := InstallWebView2;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    Installed := True;
end;

{ A failed update still leaves the user an app: the previous one starts again when it is there. }
procedure DeinitializeSetup;
var
  Executable: String;
  ResultCode: Integer;
begin
  if not Updating or Installed then
    Exit;
  try
    Executable := ExpandConstant('{app}\bin\ngteams.exe');
  except
    Exit;
  end;
  if FileExists(Executable) then
    ExecAsOriginalUser(Executable, '', ExtractFileDir(Executable), SW_SHOWNORMAL, ewNoWait, ResultCode);
end;

{ Whether a process runs from this installation's program folder. }
function AppRunning: Boolean;
var
  Locator, Service, Processes, Process: Variant;
  Folder: String;
  I: Integer;
begin
  Result := False;
  Folder := Lowercase(AddBackslash(ExpandConstant('{app}\bin')));
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Processes := Service.ExecQuery('SELECT ExecutablePath FROM Win32_Process');
    for I := 0 to Processes.Count - 1 do
    begin
      Process := Processes.ItemIndex(I);
      if not VarIsNull(Process.ExecutablePath) and (Pos(Folder, Lowercase(Process.ExecutablePath)) = 1) then
      begin
        Result := True;
        Exit;
      end;
    end;
  except
    Result := False;
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := True;
  while Result and AppRunning do
    Result := SuppressibleMsgBox('ngteams is running. Quit it (including from the notification area), then choose Retry.',
      mbError, MB_RETRYCANCEL, IDCANCEL) = IDRETRY;
end;

{ The registrations the app makes on its first start (crates/teams-ui/src/desktop/windows/registration.rs), when they
  belong to this installation; another copy's stay. }
procedure RemoveAppRegistrations;
var
  Server, Executable: String;
begin
  Executable := '"' + ExpandConstant('{app}\bin\ngteams.exe') + '"';
  if RegQueryStringValue(HKCU, 'Software\Classes\CLSID\{{#ToastActivator}}\LocalServer32', '', Server) and
    (CompareText(Server, Executable) = 0) then
  begin
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\CLSID\{{#ToastActivator}}');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\AppUserModelId\{#AppUserModelId}');
    DelTree(ExpandConstant('{localappdata}\ngteams-desktop\{#AppUserModelId}'), True, True, True);
    RemoveDir(ExpandConstant('{localappdata}\ngteams-desktop'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveAppRegistrations;
end;
