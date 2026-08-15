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
const http = require('http');
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

function getActiveIndexerPort() {
  const homeDir = process.env.USERPROFILE || process.env.HOME || '.';
  const portFile = path.join(homeDir, '.harbour', 'indexer.port');
  if (fs.existsSync(portFile)) {
    const raw = fs.readFileSync(portFile, 'utf8').trim();
    const port = parseInt(raw, 10);
    if (!isNaN(port) && port > 0) return port;
  }
  return 8765;
}

async function ensureIndexer() {
  const port = getActiveIndexerPort();
  return new Promise((resolve) => {
    const req = http.get(`http://127.0.0.1:${port}/health`, (res) => {
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

function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { 'User-Agent': 'harbour-cli' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return downloadFile(res.headers.location, dest).then(resolve).catch(reject);
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`Failed to download: HTTP ${res.statusCode}`));
      }
      const file = fs.createWriteStream(dest);
      res.pipe(file);
      file.on('finish', () => {
        file.close(() => resolve());
      });
    }).on('error', reject);
  });
}

async function run() {
  await ensureIndexer();
  const binPath = getBinaryPath();

  if (!fs.existsSync(binPath)) {
    const homeDir = process.env.USERPROFILE || process.env.HOME || '.';
    const binDir = path.join(homeDir, '.harbour', 'bin');
    fs.mkdirSync(binDir, { recursive: true });

    const key = `${process.platform}-${process.arch}`;
    const target = PLATFORMS[key];
    const downloadUrl = `https://github.com/${REPO}/releases/download/v${VERSION}/${target.bin}`;

    console.log(`\x1b[36m[harbour]\x1b[0m First run detected. Downloading native binary...`);
    try {
      await downloadFile(downloadUrl, binPath);
      if (process.platform !== 'win32') {
        fs.chmodSync(binPath, 0o755);
      }
      console.log(`\x1b[32m[harbour]\x1b[0m Ready!`);
    } catch (err) {
      console.log(`\x1b[33m[harbour]\x1b[0m Could not auto-download (${err.message}).`);
      console.log(`\x1b[36m[harbour]\x1b[0m Run 'cargo build --release' or download from https://github.com/${REPO}/releases`);
      process.exit(1);
    }
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
