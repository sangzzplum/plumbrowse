@echo off
cd /d "%~dp0.."

echo Stopping PlumBrowser if running...
taskkill /IM plumbrowser.exe /F >nul 2>&1

rem CDP for docked DevTools — must be set before the process starts.
set "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222"

if /I "%~1"=="release" (
  echo cargo build --release...
  cargo build --release
  if errorlevel 1 exit /b 1
  start "PlumBrowser" /D "%CD%" "%CD%\target\release\plumbrowser.exe"
) else (
  echo cargo build...
  cargo build
  if errorlevel 1 exit /b 1
  start "PlumBrowser" /D "%CD%" "%CD%\target\debug\plumbrowser.exe"
)

rem Close cmd — browser runs as its own process.
exit /b 0
