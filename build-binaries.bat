@echo off
REM AIGX 多平台二进制文件构建脚本 (Windows)
REM 支持平台: Linux AMD64, Linux ARM64, Windows AMD64 (exe), Windows ARM64 (exe), macOS AMD64, macOS ARM64

setlocal enabledelayedexpansion

echo ============================================
echo AIGX 多平台二进制文件构建工具 (Windows)
echo ============================================
echo.

REM 默认配置
set VERSION=%date:~0,4%.%date:~5,2%.%date:~8,2%
set BUILD_DIR=build

REM 检查 Rust
where rustc >nul 2>&1
if %errorlevel% neq 0 (
    echo [错误] 未找到 Rust 工具链
    echo 请访问 https://rustup.rs/ 下载安装
    exit /b 1
)

echo [信息] Rust 工具链已安装: %rustc --version%
echo.

REM 安装 target
for %%t in (
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    x86_64-pc-windows-gnu
    aarch64-pc-windows-gnu
    x86_64-apple-darwin
    aarch64-apple-darwin
) do (
    echo [信息] 检查 target: %%t
    rustup target list --installed | findstr /C:"%%t installed" >nul
    if %errorlevel% neq 0 (
        echo [信息] 安装 target: %%t
        rustup target add %%t
    )
)

echo.
echo [信息] Target 安装完成
echo.

REM 构建所有平台
echo [信息] 开始构建所有平台二进制文件...

REM 创建构建目录
if not exist "%BUILD_DIR%" mkdir "%BUILD_DIR%"

REM ============================================
REM 构建 Linux AMD64
REM ============================================
call :build_target x86_64-unknown-linux-gnu "2024-Linux-x86_64"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 构建 Linux ARM64
REM ============================================
call :build_target aarch64-unknown-linux-gnu "2024-Linux-arm64"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 构建所有 Windows AMD64
REM ============================================
echo [信息] 构建 Windows AMD64...
call :build_target x86_64-pc-windows-gnu "2024-Windows-x86_64.exe"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 构建 Windows ARM64
REM ============================================
echo [信息] 构建 Windows ARM64...
call :build_target aarch64-pc-windows-gnu "2024-Windows-arm64.exe"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 构建 macOS AMD64
REM ============================================
echo [信息] 构建 macOS AMD64...
call :build_target x86_64-apple-darwin "2024-macOS-x86_64"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 构建 macOS ARM64
REM ============================================
echo [信息] 构建 macOS ARM64...
call :build_target aarch64-apple-darwin "2024-macOS-arm64"
if %errorlevel% neq 0 exit /b %errorlevel%

REM ============================================
REM 创建安装包
REM ============================================
echo.
echo [信息] 创建安装包...
call :create_packages

echo.
echo ============================================
echo [成功] 所有二进制文件构建完成!
echo 构建目录: %BUILD_DIR%
echo ============================================
exit /b 0

REM ============================================
REM 构建函数: build_target
REM ============================================
:build_target
set TARGET=%~1
set OUTPUT_NAME=%~2

echo [信息] 构建目标: %TARGET%
echo.

if "%TARGET%"=="x86_64-pc-windows-gnu" (
    REM Windows 构建 (需要适当设置环境)
    echo [警告] Windows 跨平台构建需要交叉编译工具链
    echo [信息] 使用 cross 工具编译: %TARGET% -> %OUTPUT_NAME%
    call cargo install cross --git https://github.com/cross-rs/cross --bin cross --locked
    cross build --release --target !TARGET!
) else if "%TARGET%"=="aarch64-pc-windows-gnu" (
    echo [警告] Windows ARM64 跨平台构建需要交叉编译工具链
    echo [信息] 使用 cross 工具编译: %TARGET% -> %OUTPUT_NAME%
    call cargo install cross --git https://github.com/cross-rs/cross --bin cross --locked
    cross build --release --target !TARGET!
) else if "%TARGET%"=="x86_64-apple-darwin" (
    echo [警告] macOS 构建 (Intel) 需要 Intel 单片机
    echo [信息] 正在构建: %TARGET%
    cargo build --release --target %TARGET%
) else if "%TARGET%"=="aarch64-apple-darwin" (
    echo [警告] macOS 构建 (Apple Silicon) 需要 Mac
    echo [信息] 正在构建: %TARGET%
    cargo build --release --target %TARGET%
) else (
    echo [信息] 本地平台构建: %TARGET%
    cargo build --release --target %TARGET%
)

if %errorlevel% neq 0 (
    echo [错误] %TARGET% 构建失败
    exit /b 1
)

REM 复制二进制文件
set SRC_DIR=%BUILD_DIR%\%TARGET%
set BIN_NAME=aigx

if "%TARGET%"=="x86_64-pc-windows-gnu" set BIN_NAME=aigx.exe
if "%TARGET%"=="aarch64-pc-windows-gnu" set BIN_NAME=aigx.exe

if not exist "%SRC_DIR%" (
    echo [错误] 源文件不存在: %SRC_DIR%
    exit /b 1
)

copy "%SRC_DIR%\%BIN_NAME%" "%BUILD_DIR%\%OUTPUT_NAME%" >nul
echo [成功] 复制完成: %OUTPUT_NAME%

exit /b 0

REM ============================================
REM 创建安装包函数: create_packages
REM ============================================
:create_packages
set PACKAGES_DIR=%BUILD_DIR%\packages
if not exist "%PACKAGES_DIR%" mkdir "%PACKAGES_DIR%"

REM 创建 Linux tar.gz 包
if exist "%BUILD_DIR%\2024-Linux-x86_64" (
    cd /d "%BUILD_DIR%"
    tar -czvf "%PACKAGES_DIR%\aigx-linux-x86_64-%VERSION%.tar.gz" /path/to/source "2024-Linux-x86_64" --owner=0 --group=0
    if %errorlevel% neq 0 (
        echo [警告] Linux AMD64 tar 包创建失败，尝试使用 PowerShell
        powershell -Command "Compress-Archive -Path '%BUILD_DIR%\2024-Linux-x86_64' -DestinationPath '%PACKAGES_DIR%\aigx-linux-x86_64-%VERSION%.zip'"
    )
)

REM Windows ZIP 包
if exist "%BUILD_DIR%\2024-Windows-x86_64.exe" (
    powershell -Command "Compress-Archive -Path '%BUILD_DIR%\*Windows*x86*.exe' -DestinationPath '%PACKAGES_DIR%\aigx-windows-x86_64-%VERSION%.zip'"
)

if exist "%BUILD_DIR%\2024-Windows-arm64.exe" (
    powershell -Command "Compress-Archive -Path '%BUILD_DIR%\*Windows*arm*.exe' -DestinationPath '%PACKAGES_DIR%\aigx-windows-arm64-%VERSION%.zip'"
)

echo [信息] 安装包创建完成
exit /b 0

endlocal