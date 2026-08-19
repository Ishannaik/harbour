# start-harbour.ps1 — 1-click launcher for Harbour & Indexer

$ErrorActionPreference = "SilentlyContinue"

# Resolve paths whether run from Projects root or harbour folder
$baseDir = if (Test-Path "$PSScriptRoot\target") { "$PSScriptRoot\.." } else { $PSScriptRoot }
$indexerBin = "$baseDir\harbour-indexer\target\release\harbour-indexer.exe"
$clientBin  = "$baseDir\harbour\target\release\harbour.exe"
$portFile   = "$env:USERPROFILE\.harbour\indexer.port"

# Determine active port
$port = 8765
if (Test-Path $portFile) {
    $readPort = Get-Content $portFile -ErrorAction SilentlyContinue | Out-String
    $parsedPort = 0
    if ([int]::TryParse($readPort.Trim(), [ref]$parsedPort) -and $parsedPort -gt 0) {
        $port = $parsedPort
    }
}

# 1. Check if indexer is already active
$indexerRunning = (Test-NetConnection -ComputerName 127.0.0.1 -Port $port -InformationLevel Quiet)

if (-not $indexerRunning) {
    if (Test-Path $indexerBin) {
        Write-Host "[harbour] Starting background indexer service..." -ForegroundColor Cyan
        Start-Process -FilePath $indexerBin -WindowStyle Hidden
        Start-Sleep -Milliseconds 500
        if (Test-Path $portFile) {
            $port = (Get-Content $portFile -ErrorAction SilentlyContinue).Trim()
        }
        Write-Host "[harbour] Indexer active on 127.0.0.1:$port" -ForegroundColor Green
    } else {
        Write-Host "[harbour] Indexer binary not found at $indexerBin. Build with 'cargo build --release' in harbour-indexer." -ForegroundColor Yellow
    }
} else {
    Write-Host "[harbour] Indexer service already active on 127.0.0.1:$port." -ForegroundColor Green
}

# 2. Launch the client TUI
if (Test-Path $clientBin) {
    & $clientBin $args
} else {
    Write-Host "[harbour] Client binary not found at $clientBin. Running with cargo..." -ForegroundColor Yellow
    Set-Location "$baseDir\harbour"
    cargo run --release -- $args
}
