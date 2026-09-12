@echo off
chcp 65001 >nul
title PlayDL 视频下载助手
echo ╔════════════════════════════════════════╗
echo ║    PlayDL 视频下载助手                  ║
echo ║    支持: YouTube / B站 / 抖音 / 快手    ║
echo ╚════════════════════════════════════════╝
echo.

:again
echo.
echo 请粘贴视频链接（或输入 0 退出）:
set /p url="链接> "
if "%url%"=="0" exit
if "%url%"=="" goto again

echo.
echo [1/2] 解析视频...
python -m yt_dlp -f "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/bv*+ba/b" --merge-output-format mp4 -o "%%(title)s.%%(ext)s" "%url%"

echo.
echo [2/2] 完成！
echo 视频已保存到当前目录
echo.
pause
goto again