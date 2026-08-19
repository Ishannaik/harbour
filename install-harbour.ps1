# One-shot install: copies harbour + harbour-indexer into ~/.harbour/bin
# and puts that folder on the user PATH so the friend just types `harbour`.
$ErrorActionPreference = "Stop"

$binDir = Join-Path $env:USERPROFILE ".harbour\bin"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

$here = $PSScriptRoot
$candidatesHarbour = @(
    (Join-Path $here "target\release\harbour.exe"),
    (Join-Path $here "harbour.exe")
)
$candidatesIndexer = @(
    (Join-Path $here "harbour-indexer.exe"),
    (Join-Path $here "..\harbour-indexer\target\release\harbour-indexer.exe"),
    (Join-Path $here "target\release\harbour-indexer.exe")
)

function Find-First($list) {
    foreach ($p in $list) {
        if (Test-Path $p) { return (Resolve-Path $p).Path }
    }
    return $null
}

$harbourSrc = Find-First $candidatesHarbour
$indexerSrc = Find-First $candidatesIndexer

if (-not $harbourSrc) {
    Write-Host "harbour.exe not found. Build first: cargo build --release" -ForegroundColor Red
    exit 1
}
if (-not $indexerSrc) {
    Write-Host "harbour-indexer.exe not found. Build harbour-indexer, then re-run." -ForegroundColor Red
    exit 1
}

Copy-Item -Force $harbourSrc (Join-Path $binDir "harbour.exe")
Copy-Item -Force $indexerSrc (Join-Path $binDir "harbour-indexer.exe")

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $binDir } else { "$userPath;$binDir" }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    $env:Path = "$env:Path;$binDir"
    Write-Host "Added $binDir to your user PATH. Open a new terminal." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Installed:" -ForegroundColor Green
Write-Host "  $binDir\harbour.exe"
Write-Host "  $binDir\harbour-indexer.exe"
Write-Host ""
Write-Host "Friend runs:  harbour"
Write-Host "The TUI starts the indexer by itself. No second window."
