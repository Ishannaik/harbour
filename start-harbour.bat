@echo off
setlocal

set "BASE_DIR=%~dp0"
if exist "%BASE_DIR%target" (
    set "INDEXER_BIN=%BASE_DIR%..\harbour-indexer\target\release\harbour-indexer.exe"
    set "CLIENT_BIN=%BASE_DIR%target\release\harbour.exe"
) else (
    set "INDEXER_BIN=%BASE_DIR%harbour-indexer\target\release\harbour-indexer.exe"
    set "CLIENT_BIN=%BASE_DIR%harbour\target\release\harbour.exe"
)

if exist "%INDEXER_BIN%" (
    start /b "" "%INDEXER_BIN%"
)

if exist "%CLIENT_BIN%" (
    "%CLIENT_BIN%" %*
) else (
    echo [harbour] Client binary not found at %CLIENT_BIN%. Building and running...
    cd /d "%BASE_DIR%harbour"
    cargo run --release -- %*
)
