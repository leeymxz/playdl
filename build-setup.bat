@echo off
chcp 65001 >nul
title PlayDL Setup Builder

echo ========================================
echo     PlayDL Setup Builder
echo ========================================
echo.

:: Check Inno Setup
set ISCC=
if exist "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" set ISCC="C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
if exist "C:\Program Files\Inno Setup 6\ISCC.exe" set ISCC="C:\Program Files\Inno Setup 6\ISCC.exe"
if exist "C:\Program Files (x86)\Inno Setup 5\ISCC.exe" set ISCC="C:\Program Files (x86)\Inno Setup 5\ISCC.exe"

if "%ISCC%"=="" (
    echo [1/3] Downloading Inno Setup 6...
    powershell -Command "Invoke-WebRequest -Uri 'https://jrsoftware.org/download.php/is.exe' -OutFile '%TEMP%\is.exe' -UseBasicParsing"
    if errorlevel 1 (
        echo ERROR: Download failed. Install manually: https://jrsoftware.org/isdl.php
        pause
        exit /b 1
    )
    echo Installing...
    start /wait "" "%TEMP%\is.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
    if exist "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" set ISCC="C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
    if "%ISCC%"=="" (
        echo ERROR: Install failed.
        pause
        exit /b 1
    )
)

echo [1/3] OK - Inno Setup ready

:: Build playdl
if not exist "target\release\playdl.exe" (
    echo [2/3] Compiling PlayDL...
    cargo build --release --bin playdl --bin pdl
    if errorlevel 1 (
        echo ERROR: Build failed.
        pause
        exit /b 1
    )
)
echo [2/3] OK - PlayDL compiled

:: Build setup
echo [3/3] Building installer...
if not exist "dist" mkdir dist
cd packaging\innosetup
%ISCC% PlayDL.iss
if errorlevel 1 (
    echo ERROR: Setup build failed.
    pause
    exit /b 1
)

echo.
echo ========================================
echo     DONE! Setup file created in:
echo     packaging\dist\PlayDL-Setup-*.exe
echo ========================================
echo.
pause