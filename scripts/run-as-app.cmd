@echo off
setlocal
cd /d "%~dp0.."

if /I "%~1"=="release" (
  echo cargo build --release...
  cargo build --release
  if errorlevel 1 exit /b 1
  start "" "%CD%\target\release\plumbrowser.exe"
) else (
  echo cargo build...
  cargo build
  if errorlevel 1 exit /b 1
  start "" "%CD%\target\debug\plumbrowser.exe"
)

endlocal
