@echo off
chcp 65001 >nul
title PlayDL 全量打包工具
echo ============================================
echo   PlayDL 全量打包 (Windows 绿色版 + 扩展)
echo ============================================
echo.

set DIR=%~dp0
set VER=0.3.4

:: ---- 1. 打包 Windows 绿色版 ----
echo [1/3] 打包 Windows 绿色版...
set WIN_DIR=%DIR%build\playdl-%VER%-windows-amd64
if exist "%WIN_DIR%" rmdir /s /q "%WIN_DIR%"
mkdir "%WIN_DIR%\bin"

copy /Y "%DIR%target\release\playdl.exe" "%WIN_DIR%\bin\" >nul
copy /Y "%DIR%target\release\pdl.exe" "%WIN_DIR%\bin\" >nul
copy /Y "%DIR%target\release\playdl-gui.exe" "%WIN_DIR%\" >nul
copy /Y "%DIR%target\release\playdl-host.exe" "%WIN_DIR%\bin\" >nul
copy /Y "%DIR%target\release\yt-dlp.exe" "%WIN_DIR%\bin\" >nul
copy /Y "%DIR%docs\logo.png" "%WIN_DIR%\" >nul
copy /Y "%DIR%docs\playdl.ico" "%WIN_DIR%\" >nul
copy /Y "%DIR%README.md" "%WIN_DIR%\" >nul

:: GUI 脚本 + 扩展
xcopy /E /I /Y "%DIR%extensions" "%WIN_DIR%\extensions" >nul
copy /Y "%DIR%gui\PlayDL.bat" "%WIN_DIR%\" >nul 2>nul
copy /Y "%DIR%gui\PlayDL.ps1" "%WIN_DIR%\" >nul 2>nul

:: 压缩
if exist "%DIR%dist\playdl-%VER%-windows-amd64.zip" del "%DIR%dist\playdl-%VER%-windows-amd64.zip"
powershell -Command "Compress-Archive -Path '%WIN_DIR%\*' -DestinationPath '%DIR%dist\playdl-%VER%-windows-amd64.zip' -Force"
echo   OK: dist\playdl-%VER%-windows-amd64.zip

:: ---- 2. 打包扩展 ----
echo [2/3] 打包浏览器扩展...
if exist "%DIR%dist\playdl-extension-chrome.zip" del "%DIR%dist\playdl-extension-chrome.zip"
if exist "%DIR%dist\playdl-extension-firefox.zip" del "%DIR%dist\playdl-extension-firefox.zip"
powershell -Command "Compress-Archive -Path '%DIR%extensions\chrome\*' -DestinationPath '%DIR%dist\playdl-extension-chrome.zip' -Force"
powershell -Command "Compress-Archive -Path '%DIR%extensions\firefox\*' -DestinationPath '%DIR%dist\playdl-extension-firefox.zip' -Force"
echo   OK: dist\playdl-extension-chrome.zip
echo   OK: dist\playdl-extension-firefox.zip

:: ---- 3. 打包安装包 ----
echo [3/3] 打包 Windows 安装包...
cd /d "%DIR%packaging\innosetup"
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" PlayDL.iss
echo   OK: packaging\dist\PlayDL-%VER%-windows-x64-setup.exe

echo.
echo ============================================
echo   全量打包完成！
echo   dist\ 下的产物:
echo     - playdl-%VER%-windows-amd64.zip
echo     - playdl-extension-chrome.zip
echo     - playdl-extension-firefox.zip
echo     - PlayDL-%VER%-windows-x64-setup.exe
echo ============================================
pause