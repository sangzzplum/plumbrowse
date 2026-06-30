# Собирает PlumBrowser и запускает exe отдельно от PowerShell.
# Если политика блокирует .ps1, используй: scripts\run-as-app.cmd
# Или: powershell -ExecutionPolicy Bypass -File .\scripts\run-as-app.ps1
param(
    [switch]$Release
)

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

if ($Release) {
    Write-Host "→ cargo build --release..."
    cargo build --release
    $Bin = Join-Path $Root "target\release\plumbrowser.exe"
} else {
    Write-Host "→ cargo build (dev)..."
    cargo build
    $Bin = Join-Path $Root "target\debug\plumbrowser.exe"
}

Write-Host "→ start $Bin"
Start-Process -FilePath $Bin
