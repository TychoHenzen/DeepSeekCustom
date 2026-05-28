# Launch DeepSeekCustom harness in a new PowerShell window.
# Builds first, then opens the binary in a fresh terminal.

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Host "Building DeepSeekCustom..." -ForegroundColor Cyan
cargo build --profile dev --manifest-path "$projectRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed." -ForegroundColor Red
    Read-Host "Press Enter to exit"
    exit 1
}

$exe = "$projectRoot\target\debug\DeepSeekCustom.exe"
if (-not (Test-Path $exe)) {
    Write-Host "Binary not found: $exe" -ForegroundColor Red
    Read-Host "Press Enter to exit"
    exit 1
}

Write-Host "Launching in new window..." -ForegroundColor Green
Start-Process powershell -ArgumentList "-NoExit", "-Command", "& '$exe'"
