@echo off
chcp 65001 >nul
title PlayDL 构建工具
echo ============================================
echo   PlayDL v0.1.0 - 构建脚本
echo ============================================
echo.

:: -------- 检查 Rust --------
where rustc >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    echo [错误] 未安装 Rust！
    echo 请访问 https://rustup.rs/ 安装
    pause
    exit /b 1
)
echo [OK] Rust 已安装
rustc --version

:: -------- 编译 Release --------
echo.
echo [1/3] 正在编译 Release 版本...
echo.
cargo build --release --bin playdl --bin pdl
if %ERRORLEVEL% NEQ 0 (
    echo [错误] 编译失败！
    pause
    exit /b 1
)
echo.
echo [OK] 编译成功!
echo     - target\release\playdl.exe
echo     - target\release\pdl.exe

:: -------- 复制到 dist --------
echo.
echo [2/3] 整理输出文件...
if not exist dist mkdir dist
copy /Y target\release\playdl.exe dist\playdl.exe >nul
copy /Y target\release\pdl.exe dist\pdl.exe >nul
copy /Y README.md dist\README.md >nul
echo [OK] dist\ 目录已准备好

:: -------- 测试运行 --------
echo.
echo [3/3] 验证构建...
dist\playdl.exe --version 2>nul
if %ERRORLEVEL% EQU 0 (
    echo [OK] PlayDL 构建验证通过！
) else (
    echo [警告] 验证命令未输出版本信息
)

echo.
echo ============================================
echo   构建完成！
echo   可执行文件: dist\playdl.exe
echo   缩写命令:   dist\pdl.exe
echo   GUI 脚本:   gui\PlayDL.bat
echo.
echo   安装包: 安装 Inno Setup 后
echo           右键 packaging\innosetup\PlayDL.iss
echo           选择 "Compile"
echo ============================================
pause