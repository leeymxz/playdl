@echo off
title PlayDL 下载器 - 命令行终端
cd /d "%~dp0"
echo ╔══════════════════════════════════════════╗
echo ║         PlayDL 高速下载器                ║
echo ║         v0.1.0                          ║
echo ╚══════════════════════════════════════════╝
echo.
echo 可用命令:
echo   playdl --help         查看帮助
echo   playdl --version      查看版本
echo   playdl ^<URL^>          下载文件
echo   pdl ^<URL^>            缩写版
echo   playdl interactive    启动 TUI 交互界面
echo.
echo 示例:
echo   playdl https://example.com/file.zip
echo.
echo ══════════════════════════════════════════
echo.
cmd /k