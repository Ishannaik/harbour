# Friend launcher. No silent failures. The TUI starts the indexer itself.
$ErrorActionPreference = "Stop"

$here = $PSScriptRoot
$homeBin = Join-Path $env:USERPROFILE ".harbour\bin\harbour.exe"
$candidates = @(
    (Join-Path $here "target\release\harbour.exe"),
    $homeBin
)

$dev = $false
$forward = @()
foreach ($a in $args) {
    if ("$a" -ieq "-Dev") {
        $dev = $true
    } else {
        $forward += $a
    }
}

$exe = $null
foreach ($p in $candidates) {
    if (Test-Path -LiteralPath $p) {
        $exe = (Resolve-Path -LiteralPath $p).Path
        break
    }
}

if (-not $exe) {
    if ($dev) {
        Write-Host "harbour.exe not found; -Dev: cargo run --release"
        Set-Location $here
        cargo run --release -- @forward
        exit $LASTEXITCODE
    }
    Write-Host "harbour.exe not found. Friend path:" -ForegroundColor Red
    Write-Host "  powershell -ExecutionPolicy Bypass -File install-harbour.ps1"
    Write-Host "or build: cargo build --release"
    exit 1
}

Write-Host "harbour: $exe"
& $exe @forward
exit $LASTEXITCODE
