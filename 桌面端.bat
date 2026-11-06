@echo off
chcp 65001 >nul 2>nul
cd /d "%~dp0desktop"

where cargo >nul 2>nul
if errorlevel 1 (
  echo [ERROR] cargo not found. Please install the Rust toolchain first.
  pause
  exit /b 1
)

echo Building the desktop shell (first build can take several minutes)...
cargo build --release
if errorlevel 1 (
  echo [ERROR] Build failed. See the messages above.
  pause
  exit /b 1
)

echo Build OK. Publishing the double-clickable exe...
copy /Y "target\release\qiaoxiang-guide-desktop.exe" "..\QiaoxiangGuide.exe" >nul
if errorlevel 1 (
  echo [ERROR] Failed to publish QiaoxiangGuide.exe
  pause
  exit /b 1
)
start "" /D "%~dp0" "%~dp0QiaoxiangGuide.exe"
