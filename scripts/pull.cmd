@echo off
REM Windows: забрать с GitHub и пересобрать.
REM   scripts\pull.cmd

cd /d "%~dp0.."

echo Resetting Cargo.lock if locally modified (cargo build on Windows)...
git diff --quiet Cargo.lock
if errorlevel 1 (
  git checkout -- Cargo.lock
)

echo git pull...
git pull origin main
if errorlevel 1 exit /b 1

echo Stopping PlumBrowser if running...
taskkill /IM plumbrowser.exe /F >nul 2>&1

echo cargo build...
cargo build
if errorlevel 1 exit /b 1

echo.
echo Done. Run: scripts\run-as-app.cmd
