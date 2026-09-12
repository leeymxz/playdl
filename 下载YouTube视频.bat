@echo off
chcp 65001 >nul
title PlayDL + yt-dlp 一键下载 YouTube 视频
echo ================================================
echo   PlayDL + yt-dlp 下载 YouTube 视频
echo ================================================
echo.

set /p url="请输入 YouTube 链接: "
if "%url%"=="" exit

echo.
echo [1/2] 用 yt-dlp 解析视频链接...
python -m yt_dlp -f "bestvideo[ext=mp4]+bestaudio[ext=m4a]/best" -g "%url%" > "%TEMP%\ytdl_urls.txt" 2>nul
if errorlevel 1 (
    echo 解析失败，请检查链接
    pause
    exit /b 1
)

echo [2/2] 用 PlayDL 下载...
playdl --json -o "%%(title)s.mp4" "%url%"

echo.
echo 下载完成！文件在当前目录
pause