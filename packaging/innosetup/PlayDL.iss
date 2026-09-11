; PlayDL Windows 安装程序 — Inno Setup 脚本
; 使用方式：
;   1. 安装 Inno Setup: https://jrsoftware.org/isdl.php
;   2. 编译 PlayDL: cargo build --release --bin playdl --bin pdl
;   3. 右键 PlayDL.iss → Compile
;   输出: packaging\dist\PlayDL-Setup-0.1.0.exe

#define MyAppName "PlayDL"
#define MyAppShortName "PlayDL"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "leeymxz"
#define MyAppURL "https://github.com/leeymxz/playdl"
#define MyAppExeName "playdl.exe"

[Setup]
AppId={{F3E8C2A1-4D5B-6C7F-8A9B-0C1D2E3F4A5B}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=PlayDL-Setup-{#MyAppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
DisableProgramGroupPage=yes
SetupIconFile=..\..\docs\playdl.ico
UninstallDisplayIcon={app}\bin\playdl.exe
UninstallDisplayName={#MyAppName}
VersionInfoVersion={#MyAppVersion}
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription=PlayDL 高速多源下载加速器
AppCopyright=Copyright (C) 2026 leeymxz

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式:"; Flags: checkedonce
Name: "addtopath"; Description: "添加到系统 PATH（可在任意终端使用 playdl / pdl）"; GroupDescription: "系统设置:"; Flags: checkedonce

[Files]
; 主程序
Source: "..\..\target\release\playdl.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\..\target\release\pdl.exe"; DestDir: "{app}\bin"; Flags: ignoreversion

; 真正的桌面 GUI 程序
Source: "..\..\target\release\playdl-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion

; Logo 与图标
Source: "..\..\docs\logo.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\docs\playdl.ico"; DestDir: "{app}"; Flags: ignoreversion

; 文档与许可
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSING.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\PlayDL 下载器"; Filename: "{app}\playdl-desktop.exe"; WorkingDir: "{app}"; IconFilename: "{app}\playdl.ico"
Name: "{group}\PlayDL CLI终端"; Filename: "{app}\bin\playdl.exe"; WorkingDir: "{app}"; IconFilename: "{app}\playdl.ico"
Name: "{group}\卸载 PlayDL"; Filename: "{uninstallexe}"; IconFilename: "{app}\playdl.ico"
Name: "{commondesktop}\PlayDL 下载器"; Filename: "{app}\playdl-desktop.exe"; Tasks: desktopicon; WorkingDir: "{app}"; IconFilename: "{app}\playdl.ico"

[Run]
; 安装完成后运行选项
Filename: "{app}\playdl-desktop.exe"; Description: "启动 PlayDL 下载器"; Flags: postinstall nowait skipifsilent unchecked
Filename: "{app}\bin\playdl.exe"; Parameters: "--help"; Description: "查看命令行帮助"; Flags: postinstall nowait skipifsilent unchecked

; 添加到系统 PATH（需要管理员权限）
Filename: "cmd.exe"; Parameters: "/C setx PATH ""%PATH%;{app}\bin"" /M"; Flags: runhidden; Tasks: addtopath

[UninstallRun]
; 从 PATH 中移除
Filename: "cmd.exe"; Parameters: "/C setx PATH ""%PATH:;{app}\bin=%"" /M"; Flags: runhidden; Tasks: addtopath

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Registry]
; 注册 Apps & features 条目
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: string; ValueName: "DisplayName"; ValueData: "PlayDL 下载管理器"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: string; ValueName: "UninstallString"; ValueData: """{uninstallexe}"""
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: string; ValueName: "DisplayIcon"; ValueData: "{app}\bin\playdl.exe,0"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: string; ValueName: "Publisher"; ValueData: "{#MyAppPublisher}"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: string; ValueName: "URLInfoAbout"; ValueData: "{#MyAppURL}"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: dword; ValueName: "NoModify"; ValueData: "1"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\PlayDL"; ValueType: dword; ValueName: "NoRepair"; ValueData: "1"

[Code]
// 自定义欢迎页 — 显示 Logo
function InitializeSetup: Boolean;
begin
  Result := True;
end;

// 安装完成后弹窗提示
procedure CurStepChanged(CurStep: TSetupStep);
var
  ErrorCode: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    if MsgBox('PlayDL 安装完成！是否立即启动？', mbConfirmation, MB_YESNO) = IDYES then
    begin
      ShellExec('open', ExpandConstant('{app}\PlayDL.bat'), '', '', SW_SHOWNORMAL, ewNoWait, ErrorCode);
    end;
  end;
end;