; The Windows installer (Inno Setup 6). The release workflow compiles it:
;
;   iscc /DMyAppVersion=0.5.0 scripts\installer.iss
;
; It takes target\release\bkn-proxy.exe and wintun\bin\amd64\wintun.dll and
; writes dist\bkn-proxy-windows-x86_64.exe.
;
; For the current user only, so neither installing nor updating asks for
; administrator rights: the app goes to %LOCALAPPDATA%\Programs\bkn-proxy.
; The app updates itself by running this setup with /SILENT (src/update.rs).
#ifndef MyAppVersion
  #error Pass the app's version: /DMyAppVersion=x.y.z
#endif

[Setup]
; Never change: this is how an update finds the installed app.
AppId={{E873B673-D40B-4235-A1D3-1EA697DF2689}
AppName=bkn-proxy
AppVersion={#MyAppVersion}
AppPublisher=bkn-proxy
AppPublisherURL=https://github.com/baken667/proxy
VersionInfoVersion={#MyAppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\bkn-proxy
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; The language of Windows, without asking.
ShowLanguageDialog=no
; Restart Manager can't close the tunnel helper, which runs as administrator: see StopApp.
CloseApplications=no
WizardStyle=modern
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\bkn-proxy.exe
Compression=lzma2
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=bkn-proxy-windows-x86_64

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
; Asked once; an update keeps the answer.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"

[Files]
Source: "..\target\release\bkn-proxy.exe"; DestDir: "{app}"; Flags: ignoreversion
; The TUN driver the tunnel needs, next to the executable; signed by WireGuard LLC.
Source: "..\wintun\bin\amd64\wintun.dll"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\bkn-proxy"; Filename: "{app}\bkn-proxy.exe"
Name: "{autodesktop}\bkn-proxy"; Filename: "{app}\bkn-proxy.exe"; Tasks: desktopicon

[Run]
; A checkbox on the last page; a silent run (an update) starts the app without asking.
Filename: "{app}\bkn-proxy.exe"; Description: "{cm:LaunchProgram,bkn-proxy}"; Flags: nowait postinstall

[Code]
// The app stays in the notification area and holds its own .exe, which can't
// be replaced while it runs. Stop the app; its tunnel helper runs as
// administrator, out of this setup's reach, but exits by itself once the app
// is gone and takes its network changes with it (src/net.rs). Wait for that,
// ten seconds at most: a running .exe can't be deleted, so a delete that
// works says both are gone.
procedure StopApp;
var
  ResultCode, Tries: Integer;
  Exe: String;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM bkn-proxy.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exe := ExpandConstant('{app}\bkn-proxy.exe');
  for Tries := 1 to 50 do
  begin
    if (not FileExists(Exe)) or DeleteFile(Exe) then
      Break;
    Sleep(200);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopApp;
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    StopApp;
    // "Open at login" (src/prefs.rs) would otherwise point at a removed app.
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'kz.bkn.proxy');
  end;
end;
