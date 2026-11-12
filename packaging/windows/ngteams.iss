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
; - An upgrade replaces the previous version's program folders in place, keeping them aside until it has succeeded and
;   putting them back when it fails; an uninstall removes the program, its shortcut and registrations, and keeps the
;   accounts and data in %LOCALAPPDATA%\ngteams and Credential Manager.
; - The app's own updater (crates/teams-update) runs this installer with /VERYSILENT /UPDATEPID=<pid> /DIR=<folder>:
;   it waits for that process to exit, upgrades the folder and starts ngteams again, the previous version when the
;   upgrade fails.

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
; The program's top-level items, which an upgrade replaces (MovePreviousAside). Only in a folder an earlier ngteams
; installer marked; scripts/package-release-windows.ps1 refuses a stage with any other top-level name.
#define ProgramItems "bin|lib|libexec|share|MEDIA-MANIFEST.json|LICENSE-MIT|LICENSE-APACHE"
; Where an upgrade keeps them until it has succeeded, inside the installation.
#define PreviousFolder "previous-version"

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
; One installer at a time for this user: a second one waiting on its welcome page can't overwrite the first one's work.
SetupMutex=ngteams-setup-1150C52E-F222-4AA3-B1D0-EB09F8BBBFF7
#ifdef SignTool
SignTool={#SignTool}
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; The zip's own setup, removal and README aren't part of an installed copy; its manifest lists them.
Source: "{#StageDir}\*"; DestDir: "{app}"; Excludes: "\Install.ps1,\Unregister.ps1,\README.txt,\PACKAGE-MANIFEST.json"; Flags: ignoreversion recursesubdirs createallsubdirs
; What the updater reads: how this copy was installed, and the release it is (crates/teams-update MARKER, IDENTITY).
Source: "{#MarkerDir}\update-format"; DestDir: "{app}\share\ngteams"; Flags: ignoreversion
Source: "{#MarkerDir}\release"; DestDir: "{app}\share\ngteams"; Flags: ignoreversion

; The shortcuts and registrations below are removed by the uninstaller's own code (RemoveInstallerRegistrations), not
; by Inno's uninstall log: a failed upgrade's rollback replays that log, and would remove the previous version's too.
[Icons]
; The app's toast notifications need its shortcut to carry its identity; the app sets the same on its first start.
Name: "{autoprograms}\ngteams"; Filename: "{app}\bin\ngteams.exe"; WorkingDir: "{app}\bin"; Comment: "ngteams"; AppUserModelID: "{#AppUserModelId}"; AppUserModelToastActivatorCLSID: "{#ToastActivator}"; Flags: uninsneveruninstall
Name: "{autodesktop}\ngteams"; Filename: "{app}\bin\ngteams.exe"; WorkingDir: "{app}\bin"; Comment: "ngteams"; Tasks: desktopicon; Flags: uninsneveruninstall

; The msteams: handler offered under Default apps, as Install.ps1 registers it. The msteams scheme itself and the
; user's choice for it are left alone.
[Registry]
Root: HKA; Subkey: "Software\Classes\ngteams.msteams"; ValueType: string; ValueData: "ngteams Teams meeting link"
Root: HKA; Subkey: "Software\Classes\ngteams.msteams"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\ngteams.msteams\DefaultIcon"; ValueType: string; ValueData: """{app}\bin\ngteams.exe"",0"
Root: HKA; Subkey: "Software\Classes\ngteams.msteams\shell\open\command"; ValueType: string; ValueData: """{app}\bin\ngteams.exe"" ""%1"""
Root: HKA; Subkey: "Software\ngteams\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "ngteams"
Root: HKA; Subkey: "Software\ngteams\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "ngteams: chats, calls and meetings"
Root: HKA; Subkey: "Software\ngteams\Capabilities\URLAssociations"; ValueType: string; ValueName: "msteams"; ValueData: "ngteams.msteams"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "ngteams"; ValueData: "Software\ngteams\Capabilities"

[Run]
Filename: "{app}\bin\ngteams.exe"; Description: "Open ngteams"; Flags: nowait postinstall skipifsilent
; The updater's run: the app it replaced starts again, as on Linux.
Filename: "{app}\bin\ngteams.exe"; Flags: nowait; Check: Updating

; Update downloads only: the accounts, settings and logs beside them stay. A previous version an interrupted upgrade
; kept goes too.
[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\ngteams\data\updates"
Type: filesandordirs; Name: "{app}\{#PreviousFolder}"

[Code]
const
  WebView2Client = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2Bootstrapper = 'https://go.microsoft.com/fwlink/p/?LinkId=2124703';
  SYNCHRONIZE = $00100000;
  WAIT_FOR_EXIT = 60000;
  { How this copy was installed (crates/teams-update MARKER); [Files] installs it after the program files. }
  Marker = 'share\ngteams\update-format';

var
  { The previous version's program items are in {#PreviousFolder} until this run ends. }
  MovedAside: Boolean;
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

{ Whether the command line has the switch /Name, bare or with a value; a param constant sees only /Name=value. }
function SwitchGiven(const Name: String): Boolean;
var
  I: Integer;
  Argument: String;
begin
  Result := False;
  for I := 1 to ParamCount do
  begin
    Argument := Uppercase(ParamStr(I));
    if (Argument = '/' + Name) or (Pos('/' + Name + '=', Argument) = 1) then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

{ Waits for the updating app to exit, so its files can be replaced. Past the deadline, a process still running from
  the installation stops the upgrade before anything is changed (PrepareToInstall). }
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
  if SwitchGiven('ALLOWDOWNGRADE') then
    Exit;
  if RegQueryStringValue(HKA, '{#UninstallKey}', 'DisplayVersion', Previous) and
    StrToVersion(Previous, PreviousVersion) and StrToVersion('{#AppVersion}', ThisVersion) then
    Result := ComparePackedVersion(PreviousVersion, ThisVersion) > 0;
end;

function DowngradeRefused: String;
begin
  Result := 'A newer ngteams is installed already. To go back to version {#AppVersion}, uninstall ngteams first; your accounts and data are kept.';
end;

function InitializeSetup: Boolean;
begin
  Result := True;
  WaitForUpdatingApp;
  if OlderThanInstalled then
  begin
    SuppressibleMsgBox(DowngradeRefused, mbError, MB_OK, IDOK);
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

{ Whether a process runs from a folder inside Folder (bin, libexec, a previous version kept aside), or with Executable
  given, from that file; the uninstaller's own first phase, in Folder itself, doesn't count. }
function Running(const Folder, Executable: String): Boolean;
var
  Locator, Service, Processes, Process: Variant;
  Prefix, Path: String;
  I: Integer;
begin
  Result := False;
  Prefix := Lowercase(AddBackslash(Folder));
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Processes := Service.ExecQuery('SELECT ExecutablePath FROM Win32_Process');
    for I := 0 to Processes.Count - 1 do
    begin
      Process := Processes.ItemIndex(I);
      if not VarIsNull(Process.ExecutablePath) then
      begin
        Path := Lowercase(Process.ExecutablePath);
        if ((Executable = '') and (Pos(Prefix, Path) = 1) and (Pos('\', Copy(Path, Length(Prefix) + 1, Length(Path))) > 0)) or
          ((Executable <> '') and (CompareText(Path, Executable) = 0)) then
        begin
          Result := True;
          Exit;
        end;
      end;
    end;
  except
    Result := False;
  end;
end;

function RunningFrom(const Folder: String): Boolean;
begin
  Result := Running(Folder, '');
end;

{ The program's top-level items ({#ProgramItems}). }
function ProgramItems: TArrayOfString;
var
  Rest: String;
  Bar, Count: Integer;
begin
  Rest := '{#ProgramItems}|';
  Count := 0;
  while Rest <> '' do
  begin
    Bar := Pos('|', Rest);
    SetArrayLength(Result, Count + 1);
    Result[Count] := Copy(Rest, 1, Bar - 1);
    Count := Count + 1;
    Rest := Copy(Rest, Bar + 1, Length(Rest));
  end;
end;

function InstalledItem(const Item: String): String;
begin
  Result := ExpandConstant('{app}\') + Item;
end;

function PreviousItem(const Item: String): String;
begin
  Result := ExpandConstant('{app}\{#PreviousFolder}\') + Item;
end;

function ItemExists(const Path: String): Boolean;
begin
  Result := FileExists(Path) or DirExists(Path);
end;

{ Puts the previous version's items kept in {#PreviousFolder} back in place, over whatever a failed upgrade left
  there, and removes that folder once it is empty. }
procedure PutPreviousBack;
var
  Items: TArrayOfString;
  I: Integer;
begin
  Items := ProgramItems;
  for I := GetArrayLength(Items) - 1 downto 0 do
    if ItemExists(PreviousItem(Items[I])) then
    begin
      if DirExists(InstalledItem(Items[I])) then
        DelTree(InstalledItem(Items[I]), True, True, True)
      else if FileExists(InstalledItem(Items[I])) then
        DeleteFile(InstalledItem(Items[I]));
      if RenameFile(PreviousItem(Items[I]), InstalledItem(Items[I])) then
        Log('Put the previous version''s ' + Items[I] + ' back.')
      else
        Log('The previous version''s ' + Items[I] + ' could not be put back; it is in ' + PreviousItem(Items[I]) + '.');
    end;
  RemoveDir(ExpandConstant('{app}\{#PreviousFolder}'));
end;

{ An upgrade that was cut short (Setup killed, the power cut) left the previous version aside: it goes back when the
  installation holds no complete version of its own (no marker), and goes otherwise. }
procedure RecoverInterruptedUpgrade;
var
  Folder: String;
begin
  Folder := ExpandConstant('{app}\{#PreviousFolder}');
  if not DirExists(Folder) then
    Exit;
  Log('Found the previous version an interrupted upgrade kept in ' + Folder + '.');
  if not FileExists(InstalledItem(Marker)) and FileExists(PreviousItem(Marker)) then
    PutPreviousBack;
  DelTree(Folder, True, True, True);
end;

{ Moves the previous version's program items into {#PreviousFolder}, by renames on one disk, where they stay until the
  upgrade has succeeded (DeinitializeSetup); nothing is deleted. The reason they couldn't be, with everything put
  back, or ''. }
function MovePreviousAside: String;
var
  Items: TArrayOfString;
  I: Integer;
begin
  Result := '';
  { its files can be moved, but a process still running from them would go on running the previous version }
  if RunningFrom(ExpandConstant('{app}')) then
  begin
    Result := 'ngteams is still running.';
    Exit;
  end;
  if not CreateDir(ExpandConstant('{app}\{#PreviousFolder}')) then
  begin
    Result := ExpandConstant('{app}\{#PreviousFolder}') + ' could not be created.';
    Exit;
  end;
  MovedAside := True;
  Items := ProgramItems;
  for I := 0 to GetArrayLength(Items) - 1 do
    if ItemExists(InstalledItem(Items[I])) then
      if not RenameFile(InstalledItem(Items[I]), PreviousItem(Items[I])) then
      begin
        Result := 'A file in ' + InstalledItem(Items[I]) + ' is in use.';
        PutPreviousBack;
        MovedAside := False;
        Exit;
      end;
  Log('Moved the previous version aside into ' + ExpandConstant('{app}\{#PreviousFolder}') + '.');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Reason: String;
begin
  Result := '';
  { checked again: another installer may have put a newer version in since this one started }
  if OlderThanInstalled then
  begin
    Result := DowngradeRefused;
    Exit;
  end;
  if not WebView2Installed then
  begin
    Result := InstallWebView2;
    if Result <> '' then
      Exit;
  end;
  RecoverInterruptedUpgrade;
  { an upgrade only in a folder an earlier ngteams installer marked }
  if not FileExists(InstalledItem(Marker)) then
    Exit;
  repeat
    Reason := MovePreviousAside;
  until (Reason = '') or (SuppressibleMsgBox(Reason + ' Quit ngteams (including from the notification area) and close any of its files that are open, then choose Retry.',
    mbError, MB_RETRYCANCEL, IDCANCEL) <> IDRETRY);
  if Reason <> '' then
    Result := Reason + ' Quit ngteams (including from the notification area) and close any of its files that are open, then run setup again. The installed version is unchanged.';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    Installed := True;
end;

{ The installation folder, also when Setup stopped before it was settled (the updater's /DIR then). }
function InstallationFolder: String;
begin
  try
    Result := ExpandConstant('{app}');
  except
    Result := RemoveQuotes(ExpandConstant('{param:DIR|}'));
  end;
end;

{ A failed upgrade still leaves the user an app: the previous version kept aside goes back in place (a complete new
  version, its marker installed, stays even past a late failure; anything less was rolled back), and the updater's
  run starts it again. }
procedure DeinitializeSetup;
var
  Executable: String;
  ResultCode: Integer;
begin
  if MovedAside then
  begin
    if Installed or FileExists(InstalledItem(Marker)) then
      DelTree(ExpandConstant('{app}\{#PreviousFolder}'), True, True, True)
    else
      PutPreviousBack;
  end;
  if not Updating or Installed or (InstallationFolder = '') then
    Exit;
  Executable := AddBackslash(InstallationFolder) + 'bin\ngteams.exe';
  if FileExists(Executable) and not Running(InstallationFolder, Executable) then
  begin
    Log('Starting ngteams again: ' + Executable);
    ExecAsOriginalUser(Executable, '', ExtractFileDir(Executable), SW_SHOWNORMAL, ewNoWait, ResultCode);
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := True;
  while Result and RunningFrom(ExpandConstant('{app}')) do
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

{ What [Icons] and [Registry] made: the shortcuts, the msteams: handler and the Default apps entry. }
procedure RemoveInstallerRegistrations;
begin
  DeleteFile(ExpandConstant('{autoprograms}\ngteams.lnk'));
  DeleteFile(ExpandConstant('{autodesktop}\ngteams.lnk'));
  RegDeleteKeyIncludingSubkeys(HKA, 'Software\Classes\ngteams.msteams');
  RegDeleteKeyIncludingSubkeys(HKA, 'Software\ngteams\Capabilities');
  RegDeleteKeyIfEmpty(HKA, 'Software\ngteams');
  RegDeleteValue(HKA, 'Software\RegisteredApplications', 'ngteams');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveInstallerRegistrations;
    RemoveAppRegistrations;
  end;
end;
