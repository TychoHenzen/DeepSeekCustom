# Copy every hook in scripts/hooks/ into .git/hooks/.
#
# Git hooks are not checked into a repository, so a fresh clone starts with
# none. Run this once after cloning, and again after editing a hook.
#
# Run it with:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install-hooks.ps1

$repoRoot = Split-Path -Parent $PSScriptRoot
$source = Join-Path $PSScriptRoot 'hooks'
$target = Join-Path $repoRoot '.git\hooks'

if (-not (Test-Path $target)) {
    Write-Error "No .git/hooks directory at $target. Is this a git repository?"
    exit 1
}

Get-ChildItem -Path $source -File | ForEach-Object {
    $dest = Join-Path $target $_.Name
    Copy-Item -Path $_.FullName -Destination $dest -Force
    Write-Host "installed $($_.Name)"
}
