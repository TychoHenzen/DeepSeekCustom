# Build and run the loopback web application.

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Host "Building DeepSeekCustom..." -ForegroundColor Cyan
cargo build --profile dev --manifest-path "$projectRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed." -ForegroundColor Red
    Read-Host "Press Enter to exit"
    exit 1
}

$exe = "$projectRoot\target\debug\deepseek-custom.exe"
if (-not (Test-Path $exe)) {
    Write-Host "Binary not found: $exe" -ForegroundColor Red
    Read-Host "Press Enter to exit"
    exit 1
}

Write-Host "Starting the local web application..." -ForegroundColor Green
Write-Host "The server reports its final URL and opens it in your browser." -ForegroundColor Cyan
& $exe
exit $LASTEXITCODE
