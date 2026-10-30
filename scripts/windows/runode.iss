; Windows 安装包。用法：iscc /DAppVersion=0.2.0 /DSourceDir=<放着 runode.exe 和 rn.exe 的目录>
; /DOutputDir=<输出目录> scripts\windows\runode.iss。装到当前用户的目录，不要管理员权限；
; 自动更新（`runode_update`）靠 unins000.exe 认出是这个安装包装的，AppId 不能改。

#ifndef AppVersion
  #error AppVersion 没给
#endif

[Setup]
AppId={{3EB80B53-F8D0-41F0-8B16-9505695EC9E1}
AppName=Runode
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
AppPublisher=Runode
AppPublisherURL=https://github.com/runode-dev/runode
DefaultDirName={autopf}\Runode
DefaultGroupName=Runode
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile={#SourcePath}\..\..\apps\desktop\assets\compiled\runode.ico
UninstallDisplayIcon={app}\runode.exe
OutputDir={#OutputDir}
OutputBaseFilename=Runode-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
DisableProgramGroupPage=yes
WizardStyle=modern

[Files]
Source: "{#SourceDir}\runode.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\rn.exe"; DestDir: "{app}"; Flags: ignoreversion
; rn.exe 的副本：shell 按 PATHEXT 先找 .com，终端里敲 runode 跑的是控制台程序，输出回到终端；
; 不带参数时它照样打开窗口。
Source: "{#SourceDir}\rn.exe"; DestDir: "{app}"; DestName: "runode.com"; Flags: ignoreversion
Source: "{#SourceDir}\conpty.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\OpenConsole.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\conpty-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Runode"; Filename: "{app}\runode.exe"

[Registry]
; 命令行 `rn` 要在 PATH 上。
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Check: NeedsPathEntry
; 登录自启（`runode service install host`，`runode_autostart` 的 `run_key`）登记的值，安装时不写，卸载时删掉。
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; \
  ValueName: "Runode Host"; Flags: dontcreatekey uninsdeletevalue

[Run]
; 交互安装时问一句要不要打开；静默安装（自动更新、脚本）直接打开，/NORUN 不打开。
Filename: "{app}\runode.exe"; Description: "Run Runode"; Flags: nowait postinstall skipifsilent
Filename: "{app}\runode.exe"; Flags: nowait; Check: LaunchAfterSilentInstall

[Code]
function HasParam(const Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
      Result := True;
end;

function LaunchAfterSilentInstall: Boolean;
begin
  Result := WizardSilent and not HasParam('/NORUN');
end;

function NeedsPathEntry: Boolean;
var
  Path: String;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Path := '';
  Result := Pos(';' + Uppercase(ExpandConstant('{app}')) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Entry: String;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then Exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then Exit;
  Entry := ';' + ExpandConstant('{app}');
  P := Pos(Uppercase(Entry), Uppercase(Path));
  if P > 0 then
  begin
    Delete(Path, P, Length(Entry));
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
  end;
end;
