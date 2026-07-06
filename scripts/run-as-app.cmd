@echo off
cd /d "%~dp0.."

echo Stopping PlumBrowser if running...
taskkill /IM plumbrowser.exe /F >nul 2>&1

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

rem Закрываем окно cmd — браузер уже запущен отдельным процессом.
exit /b 0
