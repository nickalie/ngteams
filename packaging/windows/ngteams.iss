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
;   putting them back when it fails; an uninstall removes the program, and its shortcut and registrations unless they
;   now open another copy (a zip's), and keeps the accounts and data in %LOCALAPPDATA%\ngteams and Credential Manager.
; - The app's own updater (crates/teams-update) runs this installer with /VERYSILENT /UPDATEPID=<pid> /DIR=<folder>:
;   it waits for that process to exit, upgrades the folder and starts ngteams again, the previous version when the
;   upgrade fails. An app older than `teams_update::Restart::directory` starts it in bin, where the Start menu starts
;   the app: a folder a process works in can't be moved, so what bin holds is moved aside instead (MoveItemAside).

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

{ Says that the previous version's Item went aside by its contents (MoveContents): its own folder stayed in place. }
function ContentsMark(const Item: String): String;
begin
  Result := PreviousItem(Item) + '.contents';
end;

{ The names of the items in the folder Folder. }
function FolderItems(const Folder: String): TArrayOfString;
var
  Found: TFindRec;
  Count: Integer;
begin
  Count := 0;
  SetArrayLength(Result, 0);
  if FindFirst(AddBackslash(Folder) + '*', Found) then
  try
    repeat
      if (Found.Name <> '.') and (Found.Name <> '..') then
      begin
        SetArrayLength(Result, Count + 1);
        Result[Count] := Found.Name;
        Count := Count + 1;
      end;
    until not FindNext(Found);
  finally
    FindClose(Found);
  end;
end;

{ Moves each item the folder From holds into the folder Into, by renames on one disk; False at the first that can't
  go. A folder a process works in can't be moved, but what it holds can: an app started from the Start menu works in
  bin, and an older app starts this installer in the same folder. }
function MoveContents(const From, Into: String): Boolean;
var
  Names: TArrayOfString;
  I: Integer;
begin
  Result := ForceDirectories(Into);
  if not Result then
    Exit;
  Names := FolderItems(From);
  for I := 0 to GetArrayLength(Names) - 1 do
    if not RenameFile(AddBackslash(From) + Names[I], AddBackslash(Into) + Names[I]) then
    begin
      Log('Could not move ' + AddBackslash(From) + Names[I] + '.');
      Result := False;
      Exit;
    end;
end;

{ Puts back the previous version's Item that went aside by its contents: each of its items replaces the one of the
  same name in place, and nothing else there is deleted (a move aside cut short left the rest of it there). }
function MergeBack(const Item: String): Boolean;
var
  Names: TArrayOfString;
  I: Integer;
  Installed: String;
begin
  Result := True;
  Names := FolderItems(PreviousItem(Item));
  for I := 0 to GetArrayLength(Names) - 1 do
  begin
    Installed := AddBackslash(InstalledItem(Item)) + Names[I];
    if DirExists(Installed) then
      DelTree(Installed, True, True, True)
    else if FileExists(Installed) then
      DeleteFile(Installed);
    if not RenameFile(AddBackslash(PreviousItem(Item)) + Names[I], Installed) then
      Result := False;
  end;
  if Result and RemoveDir(PreviousItem(Item)) then
    DeleteFile(ContentsMark(Item));
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
    if FileExists(ContentsMark(Items[I])) then
    begin
      if not DirExists(PreviousItem(Items[I])) or MergeBack(Items[I]) then
      begin
        DeleteFile(ContentsMark(Items[I]));
        Log('Put the previous version''s ' + Items[I] + ' back.');
      end
      else
        Log('The previous version''s ' + Items[I] + ' could not be put back; it is in ' + PreviousItem(Items[I]) + '.');
    end
    else if ItemExists(PreviousItem(Items[I])) then
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

{ Deletes one of the previous version's items kept in {#PreviousFolder}. }
procedure DeletePreviousItem(const Item: String);
begin
  if DirExists(PreviousItem(Item)) then
    DelTree(PreviousItem(Item), True, True, True)
  else if FileExists(PreviousItem(Item)) then
    DeleteFile(PreviousItem(Item));
  if not ItemExists(PreviousItem(Item)) then
    DeleteFile(ContentsMark(Item));
end;

{ Deletes the previous version kept in {#PreviousFolder} once the new one is in. share, which holds its marker, goes
  last, and only once everything else is gone: a deletion cut short (Setup killed) or failing (a file held open) leaves
  share kept with its marker, share alone, or nothing, which RecoverInterruptedUpgrade tells apart from a move or
  put-back cut short, so the next run finishes the deletion rather than putting a partial previous version back. }
procedure DiscardPrevious;
var
  Items: TArrayOfString;
  I: Integer;
begin
  Items := ProgramItems;
  for I := 0 to GetArrayLength(Items) - 1 do
    if Items[I] <> 'share' then
      DeletePreviousItem(Items[I]);
  for I := 0 to GetArrayLength(Items) - 1 do
    if (Items[I] <> 'share') and ItemExists(PreviousItem(Items[I])) then
    begin
      Log('The previous version''s ' + Items[I] + ' could not be deleted from ' + ExpandConstant('{app}\{#PreviousFolder}') + '; its share is kept, so the next setup run finishes the deletion.');
      Exit;
    end;
  DeletePreviousItem('share');
  if RemoveDir(ExpandConstant('{app}\{#PreviousFolder}')) then
    Log('Removed the previous version kept in ' + ExpandConstant('{app}\{#PreviousFolder}') + '.');
end;

{ An upgrade cut short (Setup killed, the power cut), or a deletion of the previous version that failed, left
  {#PreviousFolder} behind. It is touched only when this installer's marker is in the installation or in that folder;
  otherwise it isn't ngteams', and Setup stops rather than make that folder an installation whose next upgrade would
  take it for its own. Where the marker is tells what was cut short, as share (which holds it) moves aside after bin,
  lib and libexec and goes back before them:
  - both markers: the new version is complete (its marker is installed last), so the previous one goes (also what a
    failed DiscardPrevious leaves);
  - the kept marker alone: the copying, or the putting back, was cut short, so the previous version goes back;
  - the installed marker alone: the move aside or the putting back was cut short, so what is kept goes back, unless
    share is kept too, which only DiscardPrevious leaves (once all else is gone).
  Why Setup stops (the folder is foreign, or still there afterwards), or ''. }
function RecoverInterruptedUpgrade: String;
var
  Folder: String;
  InPlace, Kept: Boolean;
begin
  Result := '';
  Folder := ExpandConstant('{app}\{#PreviousFolder}');
  if not DirExists(Folder) then
    Exit;
  InPlace := FileExists(InstalledItem(Marker));
  Kept := FileExists(PreviousItem(Marker));
  if not InPlace and not Kept then
  begin
    Result := Folder + ' was not made by ngteams setup, and is left alone. Move it out of that folder, or install ngteams elsewhere, then run setup again.';
    Exit;
  end;
  Log('Found the previous version an interrupted upgrade kept in ' + Folder + '.');
  if InPlace and (Kept or ItemExists(PreviousItem('share'))) then
    DiscardPrevious
  else
    PutPreviousBack;
  if DirExists(Folder) then
    Result := 'An interrupted upgrade left ' + Folder + ', and it could not be put back or removed. Quit ngteams (including from the notification area) and close any of its files that are open, then run setup again.';
end;

{ Moves the previous version's Item into {#PreviousFolder}: by one rename, or when Item is a folder a process works in,
  which stays, by moving what it holds. That is marked first, so that a put back merges it back. }
function MoveItemAside(const Item: String): Boolean;
begin
  Result := RenameFile(InstalledItem(Item), PreviousItem(Item));
  if Result or not DirExists(InstalledItem(Item)) or not SaveStringToFile(ContentsMark(Item), '', False) then
    Exit;
  Log(InstalledItem(Item) + ' could not be moved (a process works in it): moving what it holds instead.');
  Result := MoveContents(InstalledItem(Item), PreviousItem(Item));
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
    if ItemExists(InstalledItem(Items[I])) and not MoveItemAside(Items[I]) then
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
  Result := RecoverInterruptedUpgrade;
  if Result <> '' then
    Exit;
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
      DiscardPrevious
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

{ Whether the shortcut at Path opens Executable. }
function ShortcutOpens(const Path, Executable: String): Boolean;
var
  Shell, Link: Variant;
begin
  Result := False;
  try
    Shell := CreateOleObject('WScript.Shell');
    Link := Shell.CreateShortcut(Path);
    Result := CompareText(Link.TargetPath, Executable) = 0;
  except
    Result := False;
  end;
end;

procedure RemoveShortcut(const Path, Executable: String);
begin
  if not FileExists(Path) then
    Exit;
  if ShortcutOpens(Path, Executable) then
    DeleteFile(Path)
  else
    Log('Kept ' + Path + ': it opens another copy of ngteams.');
end;

{ What [Icons] and [Registry] made: the shortcuts, the msteams: handler and the Default apps entry, when they belong to
  this installation, as Unregister.ps1 checks them; another copy's (a zip's Install.ps1 writes the same names) stay. }
procedure RemoveInstallerRegistrations;
var
  Executable, Command: String;
begin
  Executable := ExpandConstant('{app}\bin\ngteams.exe');
  RemoveShortcut(ExpandConstant('{autoprograms}\ngteams.lnk'), Executable);
  RemoveShortcut(ExpandConstant('{autodesktop}\ngteams.lnk'), Executable);
  if not RegQueryStringValue(HKA, 'Software\Classes\ngteams.msteams\shell\open\command', '', Command) then
    Exit;
  if CompareText(Command, '"' + Executable + '" "%1"') <> 0 then
  begin
    Log('Kept the msteams: handler: it opens another copy of ngteams.');
    Exit;
  end;
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
