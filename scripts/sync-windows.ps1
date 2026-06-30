# На Windows: забрать последние изменения с GitHub и пересобрать.
# Запуск из корня репо: .\scripts\sync-windows.ps1

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

Write-Host "→ git pull..."
git pull origin main

Write-Host "→ cargo build..."
cargo build

Write-Host "Готово. Запуск: cargo run  или  .\scripts\run-as-app.ps1"
