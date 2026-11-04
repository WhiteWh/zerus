; The per-user Zerus installer (docs/superpowers/specs/2026-10-18-windows-installer-design.md).
; scripts/package-windows-installer.py stages the tested portable zip's files,
; installation.json and uninstall\payload.txt in PayloadDir and compiles this
; script with Inno Setup 6. Nothing here changes a payload file: third-party
; DLLs and EXEs keep their bytes and signatures.
;
; Payload cleanup never touches the staged CLI, session host images,
; configuration, state, desktop settings or drafts. The maintained passive
; registration command stages the stable CLI before registering its own task.
; Restart Manager closes only processes that use files in
; the install folder; the session host runs from its staged image, so it and its
; sessions keep running through an upgrade.

#ifndef AppVersion
  #error Define AppVersion
#endif
#ifndef NumericVersion
  #error Define NumericVersion
#endif
#ifndef PayloadDir
  #error Define PayloadDir
#endif
#ifndef OutputDir
  #error Define OutputDir
#endif
#ifndef OutputName
  #error Define OutputName
#endif
#ifndef IconFile
  #error Define IconFile
#endif
#ifndef TauriDesktop
  #error Define TauriDesktop
#endif

[Setup]
; Never change the AppId: upgrades and the uninstaller find earlier installs by it.
AppId={{6F1B0C52-7E3A-4D8B-9C41-2A5E8F0D3B17}
AppName=Zerus
AppVersion={#AppVersion}
AppVerName=Zerus {#AppVersion}
AppPublisher=Zerus contributors
AppPublisherURL=https://github.com/ufna/zerus
AppSupportURL=https://github.com/ufna/zerus/issues
AppUpdatesURL=https://github.com/ufna/zerus/releases
VersionInfoVersion={#NumericVersion}
VersionInfoProductName=Zerus
VersionInfoProductTextVersion={#AppVersion}
; Per user, never elevated: {autopf} is %LOCALAPPDATA%\Programs.
PrivilegesRequired=lowest
DefaultDirName={autopf}\Zerus
; The folder is the installer's own: an upgrade removes files an older version
; left there, so the user does not choose an arbitrary one.
DisableDirPage=yes
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 10 1903, as the portable zip and the winget manifest require.
MinVersion=10.0.18362
UninstallFilesDir={app}\uninstall
UninstallDisplayName=Zerus
UninstallDisplayIcon={app}\hgs-tray.exe
SetupIconFile={#IconFile}
; Restart Manager asks processes using files in {app} to close; the GUI then
; exits, its drafts already saved. Nothing outside {app} is closed.
CloseApplications=yes
RestartApplications=no
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=no
ChangesEnvironment=no

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

#if TauriDesktop
[Tasks]
Name: "login"; Description: "Start Zerus at login"; Flags: unchecked
#endif

[Icons]
#if TauriDesktop
Name: "{autoprograms}\Zerus"; Filename: "{app}\hgs-tray.exe"; WorkingDir: "{app}"; AppUserModelID: "com.hgdev.hgs-tray"; AppUserModelToastActivatorCLSID: "0F0F70A2-1D40-47D8-AC5E-D6A9B789605F"
#else
Name: "{autoprograms}\Zerus"; Filename: "{app}\hgs-tray.exe"; WorkingDir: "{app}"
#endif

[Run]
#if TauriDesktop
Filename: "{app}\hgs-tray.exe"; Parameters: "--install-login-item"; Tasks: login; Flags: runhidden
#endif
Filename: "{app}\hgs-tray.exe"; Description: "Start Zerus"; Flags: nowait postinstall skipifsilent
; A self-update runs the installer silently with /RELAUNCH after the desktop
; app has exited; the new version starts once its files are in place.
Filename: "{app}\hgs-tray.exe"; Flags: nowait; Check: Relaunch

#if TauriDesktop
[UninstallRun]
Filename: "{app}\hgs-tray.exe"; Parameters: "--remove-login-item"; Flags: runhidden
#endif

[UninstallDelete]
Type: dirifempty; Name: "{app}"

[Code]
const
  { FILE_ATTRIBUTE_REPARSE_POINT: junctions, symbolic links and mount points. }
  ReparsePoint = $400;

var
  Upgrade: Boolean;
  Payload: TStringList;

function Relaunch(): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/RELAUNCH') = 0 then
      Result := True;
end;

{ Only the maintained CLI may stage/register the passive service and Toast
  companion. Neither command starts a task, grants background consent or runs
  native agents. A failed/unknown result is kept for manual status inspection. }
procedure DesktopServiceRegistration(const Installing: Boolean);
var
  Code: Integer;
begin
  if not Installing then
  begin
    Log('UI-only removal keeps the staged CLI, desktop service logon task and Toast registration. UI login is removed separately.');
    Exit;
  end;
  if not Exec(ExpandConstant('{app}\hgs.exe'), 'desktop registration install --json',
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) then
  begin
    Log('Desktop registration could not be invoked; inspect its original status before retrying.');
    Exit;
  end;
  if Code <> 0 then
  begin
    Log('Desktop registration is unavailable (exit ' + IntToStr(Code) + '); selected CLI/state kept.');
    Exit;
  end;
  if not Exec(ExpandConstant('{app}\hgs.exe'), 'desktop notification-registration install --json',
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) then
    Log('Toast registration could not be invoked; inspect status before retrying.')
  else if Code <> 0 then
    Log('Toast registration is unavailable (exit ' + IntToStr(Code) + '); selected CLI/state kept.');
end;

{ WebView2 Evergreen is a shared Microsoft runtime. Detect only its documented
  own/per-machine registration; never install, update or remove it silently.
  https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution }
function RuntimeVersion(const Version: String): Boolean;
var
  I, Dots, Digits: Integer;
  Nonzero: Boolean;
begin
  Result := False;
  if (Length(Version) = 0) or (Length(Version) > 64) then Exit;
  Dots := 0;
  Digits := 0;
  Nonzero := False;
  for I := 1 to Length(Version) do
  begin
    if Version[I] = '.' then
    begin
      if Digits = 0 then Exit;
      Dots := Dots + 1;
      Digits := 0;
    end
    else
    begin
      if (Version[I] < '0') or (Version[I] > '9') then Exit;
      Digits := Digits + 1;
      if Version[I] <> '0' then Nonzero := True;
    end;
  end;
  Result := (Dots = 3) and (Digits > 0) and Nonzero;
end;

function WebView2Installed(): Boolean;
var
  Version: String;
  Key: String;
begin
  Key := 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  Result := (RegQueryStringValue(HKCU, Key, 'pv', Version) and RuntimeVersion(Version));
  if not Result then
    Result := (RegQueryStringValue(HKLM32, Key, 'pv', Version) and RuntimeVersion(Version));
end;

{ Whether Path itself, not what it may lead to, is a reparse point. }
function IsLink(const Path: String): Boolean;
var
  Find: TFindRec;
begin
  Result := False;
  if FindFirst(RemoveBackslashUnlessRoot(Path), Find) then
  try
    Result := (Find.Attributes and ReparsePoint) <> 0;
  finally
    FindClose(Find);
  end;
end;

{ The first reparse point among Path and the folders above it, else ''. The
  installer neither writes nor cleans up through a link anywhere on its path. }
function LinkedPath(const Path: String): String;
var
  Current, Parent: String;
  Depth: Integer;
begin
  Result := '';
  Current := RemoveBackslashUnlessRoot(Path);
  for Depth := 1 to 128 do
  begin
    if IsLink(Current) then
    begin
      Result := Current;
      Exit;
    end;
    Parent := RemoveBackslashUnlessRoot(ExtractFileDir(Current));
    if (Parent = '') or (CompareText(Parent, Current) = 0) then
      Exit;
    Current := Parent;
  end;
  { A path deeper than any real one: refuse rather than guess. }
  Result := Path;
end;

{ The first reparse point in Folder or below it, else ''. Never follows one. }
function FindLink(const Folder: String): String;
var
  Find: TFindRec;
  Path: String;
begin
  Result := '';
  if not FindFirst(AddBackslash(Folder) + '*', Find) then
    Exit;
  try
    repeat
      if (Find.Name <> '.') and (Find.Name <> '..') then
      begin
        Path := AddBackslash(Folder) + Find.Name;
        if (Find.Attributes and ReparsePoint) <> 0 then
          Result := Path
        else if (Find.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
          Result := FindLink(Path);
      end;
    until (Result <> '') or not FindNext(Find);
  finally
    FindClose(Find);
  end;
end;

{ A link on the folder's path or inside it could lead installed files and the
  stale-file sweep anywhere the user can write, such as configuration or state:
  refuse before anything changes. Setup exits with code 7. Windows 11's
  redirection guard is no substitute: Zerus supports Windows 10 1903. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Link: String;
begin
  Result := '';
  #if TauriDesktop
  if not WebView2Installed() then
  begin
    Result := 'Zerus Next requires Microsoft Edge WebView2 Evergreen Runtime. Install it from ' +
      'https://developer.microsoft.com/microsoft-edge/webview2/#download-section and run Setup again. ' +
      'No Zerus files were changed.';
    Log(Result);
    Exit;
  end;
  #endif
  Link := LinkedPath(ExpandConstant('{app}'));
  if Link = '' then
    Link := FindLink(ExpandConstant('{app}'));
  if Link <> '' then
  begin
    Result := 'Setup stopped: ' + Link + ' is a link or junction on the path of the Zerus folder or inside it.' +
      ' Remove it or choose another location, then run Setup again. Nothing was changed.';
    Log(Result);
  end;
end;

{ The installer record scripts/windows_installer.py writes: only a folder an
  earlier installer created, with no link on its path, is cleaned up. }
function OwnedFolder(): Boolean;
var
  Contents: AnsiString;
begin
  Result := (LinkedPath(ExpandConstant('{app}')) = '') and
    LoadStringFromFile(ExpandConstant('{app}\installation.json'), Contents) and
    (Pos('"kind": "installer"', Contents) > 0) and (Pos('"product": "Zerus"', Contents) > 0);
end;

{ Removes files in Folder (relative path Relative) that the new payload does
  not name, and then empty folders. uninstall\ keeps the uninstaller. A link is
  never followed or removed, whatever it points at. }
procedure RemoveStale(const Folder, Relative: String);
var
  Find: TFindRec;
  Path, Name: String;
begin
  if not FindFirst(AddBackslash(Folder) + '*', Find) then
    Exit;
  try
    repeat
      if (Find.Name <> '.') and (Find.Name <> '..') then
      begin
        Path := AddBackslash(Folder) + Find.Name;
        if Relative = '' then
          Name := Find.Name
        else
          Name := Relative + '\' + Find.Name;
        if (Find.Attributes and ReparsePoint) <> 0 then
          Log('Kept ' + Name + ': the clean-up never follows or removes links.')
        else if (Find.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
        begin
          if CompareText(Name, 'uninstall') <> 0 then
          begin
            RemoveStale(Path, Name);
            RemoveDir(Path);
          end;
        end
        else if Payload.IndexOf(Lowercase(Name)) < 0 then
        begin
          if DeleteFile(Path) then
            Log('Removed ' + Name + ', which this version no longer ships.')
          else
            Log('Could not remove ' + Name + ', which this version no longer ships.');
        end;
      end;
    until not FindNext(Find);
  finally
    FindClose(Find);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Lines: TArrayOfString;
  I: Integer;
begin
  if CurStep = ssInstall then
    Upgrade := OwnedFolder();
  #if TauriDesktop
  if CurStep = ssPostInstall then
    DesktopServiceRegistration(True);
  #endif
  { After every file is in place: a failed install rolls back and removes nothing. }
  if (CurStep = ssPostInstall) and Upgrade then
  begin
    if not LoadStringsFromFile(ExpandConstant('{app}\uninstall\payload.txt'), Lines) then
    begin
      Log('The payload list is missing; files of earlier versions stay.');
      Exit;
    end;
    Payload := TStringList.Create;
    try
      for I := 0 to GetArrayLength(Lines) - 1 do
        if Trim(Lines[I]) <> '' then
          Payload.Add(Lowercase(Trim(Lines[I])));
      RemoveStale(ExpandConstant('{app}'), '');
    finally
      Payload.Free;
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  #if TauriDesktop
  if CurUninstallStep = usUninstall then
    DesktopServiceRegistration(False);
  #endif
end;
