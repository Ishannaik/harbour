#!/usr/bin/env node

/**
 * harbour-cli — npm wrapper for the native Harbour TUI binary.
 *
 * Runs the pre-compiled native Rust binary for the current platform
 * or downloads it directly from GitHub Releases.
 */

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const https = require('https');

const VERSION = '0.1.0';
const REPO = 'Ishannaik/harbour';

const PLATFORMS = {
  'win32-x64': { bin: 'harbour.exe', asset: 'harbour-x86_64-pc-windows-msvc.zip' },
  'darwin-x64': { bin: 'harbour', asset: 'harbour-x86_64-apple-darwin.tar.gz' },
  'darwin-arm64': { bin: 'harbour', asset: 'harbour-aarch64-apple-darwin.tar.gz' },
  'linux-x64': { bin: 'harbour', asset: 'harbour-x86_64-unknown-linux-gnu.tar.gz' },
};

function getBinaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const target = PLATFORMS[key];

  if (!target) {
    console.error(`Unsupported platform or architecture: ${key}`);
    process.exit(1);
  }

  // Check local release build if running in repository development
  const localTarget = path.join(__dirname, '..', '..', 'target', 'release', target.bin);
  if (fs.existsSync(localTarget)) {
    return localTarget;
  }

  // Check cache dir in user home directory
  const homeDir = process.env.USERPROFILE || process.env.HOME || '.';
  const binDir = path.join(homeDir, '.harbour', 'bin');
  return path.join(binDir, target.bin);
}

async function ensureIndexer() {
  return new Promise((resolve) => {
    const req = https.get('http://127.0.0.1:8765/health', (res) => {
      resolve(res.statusCode === 200);
    });
    req.on('error', () => {
      // Indexer not reachable — try to auto-spawn if present locally
      const localIndexer = path.join(__dirname, '..', '..', '..', 'harbour-indexer', 'target', 'release', 'harbour-indexer.exe');
      if (fs.existsSync(localIndexer)) {
        console.log(`\x1b[36m[harbour]\x1b[0m Starting local indexer in background...`);
        const indexerProc = spawn(localIndexer, [], { detached: true, stdio: 'ignore' });
        indexerProc.unref();
      }
      resolve(false);
    });
    req.setTimeout(500, () => {
      req.destroy();
      resolve(false);
    });
  });
}

async function run() {
  await ensureIndexer();
  const binPath = getBinaryPath();

  if (!fs.existsSync(binPath)) {
    console.log(`\x1b[36m[harbour]\x1b[0m Native binary not found at ${binPath}`);
    console.log(`\x1b[36m[harbour]\x1b[0m Please build with 'cargo build --release' or download from GitHub Releases.`);
    process.exit(1);
  }

  const child = spawn(binPath, process.argv.slice(2), {
    stdio: 'inherit',
  });

  child.on('exit', (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 0);
    }
  });
}

run();
