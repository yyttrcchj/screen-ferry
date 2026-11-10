; ScreenFerry 的 Windows 安装包，英文和简体中文两种语言。需要 Inno Setup 7.1 或更高版本（从这一版起自带简体中文）。
; 由 `cargo xtask bundle` 编译，它传入：
;   AppVersion  版本号，取自 Cargo.toml
;   SourceDir   组装好的程序目录：screenferry.exe 和 settings\
; 并用 --output-dir 指定安装包放在哪里。设计见 docs/adr/0011-windows-installer.md。

#ifndef AppVersion
  #error 请用 cargo xtask bundle 编译（缺少 AppVersion）
#endif
#ifndef SourceDir
  #error 请用 cargo xtask bundle 编译（缺少 SourceDir）
#endif

#define AppName "ScreenFerry"
#define AppExe "screenferry.exe"
#define SettingsExe "screenferry-settings.exe"
#define Repo "https://github.com/icloudza/screen-ferry"

[Setup]
; 安装包的身份：升级、卸载都靠它认出已经装好的那一份，不能改。
AppId={{CEE535CE-67B9-466B-883F-0E39783CFB8C}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=icloudza
AppPublisherURL={#Repo}
AppSupportURL={#Repo}/issues
AppUpdatesURL={#Repo}/releases
AppCopyright=MIT OR Apache-2.0
VersionInfoVersion={#AppVersion}
VersionInfoDescription={#AppName} Setup
VersionInfoProductName={#AppName}
; 装进 Program Files，这台电脑上的所有用户都能用；要管理员权限，同时登记防火墙规则。
PrivilegesRequired=admin
SetupArchitecture=x64
ArchitecturesAllowed=x64compatible
MinVersion=10.0
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
WizardStyle=modern
SetupIconFile=..\..\assets\icons\app-icon-windows.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
OutputBaseFilename={#AppName}-{#AppVersion}-windows-x64-setup
SetupLogging=yes
; 程序文件被占用时：安装前先请 ScreenFerry 自己退出（见 [Code]），剩下的交给系统的重启管理器关闭。
CloseApplications=yes
RestartApplications=no
; 开机自启登记在当前用户名的“登录时、最高权限”计划任务下：安装时把旧的 HKCU 启动项迁过去，
; 卸载时删掉（见 [Code] 和 [UninstallRun]）。计划任务由管理员登记，登录时不会再弹 UAC。
UsedUserAreasWarning=no
; 按 Windows 的显示语言选：中文系统用简体中文，其他都用英文（排在第一个的语言），不弹出选择语言的对话框。
LanguageDetectionMethod=uilanguage
ShowLanguageDialog=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "chs"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[CustomMessages]
en.AppComment=Share one keyboard and mouse across your computers
chs.AppComment=在多台电脑之间共享一套键盘鼠标
en.FirewallStatus=Configuring Windows Firewall…
chs.FirewallStatus=正在设置 Windows 防火墙…

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; 设置界面整个换新，不留旧版本的资源文件。
Type: filesandordirs; Name: "{app}\settings"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\settings\*"; DestDir: "{app}\settings"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "{cm:AppComment}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
; 防火墙：允许本程序在专用网络和域网络上接受连接（键鼠、剪贴板、邀请、局域网发现），先删后加，重装不会重复。
; 公用网络不放行，连到公用网络时 Windows 会照常询问。
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden waituntilterminated; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#AppName}"" dir=in action=allow program=""{app}\{#AppExe}"" enable=yes profile=private,domain"; Flags: runhidden waituntilterminated
; 安装前正在运行的：装完马上重新启动，不弹出设置界面。
; 用户可能正从另一台电脑控制这台，这样不用等到点“完成”就能接着用键鼠。
Filename: "{app}\{#AppExe}"; Parameters: "--autostart"; Flags: nowait; Check: WasRunning
; 第一次安装：完成页上可以选择启动，打开设置界面开始配对。
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent; Check: not WasRunning

[UninstallRun]
; 先请 ScreenFerry 正常退出（归还键鼠、松开按键），还在的再强行结束，然后删掉防火墙规则。
Filename: "{app}\{#AppExe}"; Parameters: "quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitApp"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe} /IM {#SettingsExe}"; Flags: runhidden waituntilterminated; RunOnceId: "KillApp"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden waituntilterminated; RunOnceId: "RemoveFirewallRule"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

var
  Running: Boolean;

{ 安装前 ScreenFerry 是否在运行：它运行时在缓存目录里写着 ipc.json，退出时删掉。 }
function WasRunning: Boolean;
begin
  Result := Running;
end;

{ 安装前请正在运行的 ScreenFerry 退出，程序文件才能替换：
  先用新版本的 quit 命令请它正常退出（归还键鼠、松开按键，设置界面跟着关闭），
  不支持 quit 的旧版本再强行结束。这样也不会弹出“关闭应用程序”的页面。 }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Running := FileExists(ExpandConstant('{localappdata}\{#AppName}\ipc.json'));
  if Running then
  begin
    ExtractTemporaryFile('{#AppExe}');
    Exec(ExpandConstant('{tmp}\{#AppExe}'), 'quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe} /IM {#SettingsExe} /IM screenferry_settings.exe',
      '', SW_HIDE, ewWaitUntilTerminated, Code);
  end;
  Result := '';
end;

{ 旧版本把开机自启用 HKCU 的 Run 键登记为普通权限。装了新版本后换成最高权限的计划任务，
  以后登录时不会再弹 UAC；迁移失败时保留旧值，等后台以后自己再试。 }
procedure MigrateAutostart;
var
  Command: String;
  Code: Integer;
  Parameters: String;
begin
  if not RegQueryStringValue(HKCU, RunKey, '{#AppName}', Command) then
    Exit;
  Parameters := '/Create /F /SC ONLOGON /RL HIGHEST /IT /TN "' + '{#AppName}' + '" /TR "\"' +
    ExpandConstant('{app}\{#AppExe}') + '\" --autostart"';
  if Exec(ExpandConstant('{sys}\schtasks.exe'), Parameters, '', SW_HIDE,
    ewWaitUntilTerminated, Code) and (Code = 0) then
    RegDeleteValue(HKCU, RunKey, '{#AppName}');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    MigrateAutostart;
end;

{ 卸载时删掉计划任务和旧版本的 Run 登记，登录时不再去找已经删掉的程序。 }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    Exec(ExpandConstant('{sys}\schtasks.exe'),
      '/Delete /TN "' + '{#AppName}' + '" /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
    if RegQueryStringValue(HKCU, RunKey, '{#AppName}', Command)
      and (Pos(Lowercase(ExpandConstant('{app}')), Lowercase(Command)) > 0) then
      RegDeleteValue(HKCU, RunKey, '{#AppName}');
  end;
end;
