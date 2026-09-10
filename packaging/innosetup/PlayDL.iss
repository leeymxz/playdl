; PlayDL Windows 安装程序 - Inno Setup 脚本
; 1. 安装 Inno Setup: https://jrsoftware.org/isdl.php
; 2. 先编译 playdl: cargo build --release
; 3. 右键本文件 -> Compile

#define MyAppName "PlayDL"
#define MyAppShortName "PlayDL"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "Your Name"
#define MyAppURL "https://github.com/leeymxz/playdl"
#define MyAppExeName "playdl.exe"

[Setup]
AppId={{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}
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
SetupIconFile=..\docs\logo.png

; 使用 PlayDL Logo 作为安装程序图标
; 注：Inno Setup 需要 .ico 格式图标，这里用 logo.png 转
; 你可以用在线工具把 docs/logo.png 转成 .ico

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式:"; Flags: checkedonce
Name: "addtopath"; Description: "添加到系统 PATH（可在任意终端使用 playdl）"; GroupDescription: "系统设置:"; Flags: checkedonce

[Files]
; 主程序
Source: "..\target\release\playdl.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\target\release\pdl.exe"; DestDir: "{app}\bin"; Flags: ignoreversion

; GUI 脚本
Source: "..\gui\PlayDL.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\gui\PlayDL.bat"; DestDir: "{app}"; Flags: ignoreversion

; Logo 和文档
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\logo.png"; DestDir: "{app}"; Flags: ignoreversion

; 许可证文件
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\PlayDL 下载器"; Filename: "{app}\PlayDL.bat"; WorkingDir: "{app}"; IconFilename: "{app}\logo.png"
Name: "{group}\PlayDL CLI (终端)"; Filename: "{app}\bin\playdl.exe"; Parameters: "--help"; WorkingDir: "{app}"
Name: "{group}\卸载 PlayDL"; Filename: "{uninstallexe}"
Name: "{commondesktop}\PlayDL"; Filename: "{app}\PlayDL.bat"; Tasks: desktopicon; WorkingDir: "{app}"; IconFilename: "{app}\logo.png"

[Run]
; 安装后运行选项
Filename: "{app}\PlayDL.bat"; Description: "启动 PlayDL 下载器"; Flags: postinstall nowait skipifsilent shellexec
Filename: "{app}\bin\playdl.exe"; Parameters: "--help"; Description: "查看命令行帮助"; Flags: postinstall nowait skipifsilent

; 添加到系统 PATH
Filename: "cmd.exe"; Parameters: "/C setx PATH ""%PATH%;{app}\bin"" /M"; Flags: runhidden; Tasks: addtopath

[UninstallRun]
; 从 PATH 中移除
Filename: "cmd.exe"; Parameters: "/C setx PATH ""%PATH:;{app}\bin=%"" /M"; Flags: runhidden; Tasks: addtopath

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Messages]
ChineseSimplified.WelcomeLabel2=这个安装程序将安装 [name/ver] 到你的电脑上。%n%nPlayDL 是一款高速多源下载加速器。
ChineseSimplified.FinishedLabel=安装完成！你可以通过桌面快捷方式或终端使用 PlayDL。

[Code]
function InitializeSetup: Boolean;
begin
  Result := True;
end;