@echo off
title PlayDL 下载器
cd /d "%~dp0"

:menu
cls
echo.
echo   ╔══════════════════════════════════════╗
echo   ║       PlayDL 高速下载器 v0.1.0       ║
echo   ╚══════════════════════════════════════╝
echo.
echo   1. 下载文件
echo   2. 多连接加速下载
echo   3. 断点续传
echo   4. 查看帮助
echo   5. 退出
echo.
set /p choice="请选择 (1-5): "

if "%choice%"=="1" goto download
if "%choice%"=="2" goto fast
if "%choice%"=="3" goto resume
if "%choice%"=="4" goto help
if "%choice%"=="5" goto end
goto menu

:download
cls
echo.
set /p url="请输入下载链接: "
if "%url%"=="" goto menu
echo.
playdl "%url%"
echo.
pause
goto menu

:fast
cls
echo.
set /p url="请输入下载链接: "
if "%url%"=="" goto menu
set /p conns="连接数 (推荐 4-8): "
if "%conns%"=="" set conns=4
echo.
playdl -x %conns% "%url%"
echo.
pause
goto menu

:resume
cls
echo.
set /p url="请输入下载链接: "
if "%url%"=="" goto menu
echo.
playdl -c "%url%"
echo.
pause
goto menu

:help
cls
playdl --help
echo.
pause
goto menu

:end
exit