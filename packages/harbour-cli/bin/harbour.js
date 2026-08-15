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

const VERSION = '0.1.1';
const REPO = 'Ishannaik/harbour';

const PLATFORMS = {
  'win32-x64': { bin: 'harbour.exe', asset: 'harbour-x86_64-pc-windows-msvc.zip' },
  'darwin-x64': { bin: 'harbour', asset: 'harbour-x86_64-apple-darwin.tar.gz' },
  'darwin-arm64': { bin: 'harbour', asset: 'harbour-aarch64-apple-darwin.tar.gz' },
  'linux-x64': { bin: 'harbour', asset: 'harbour-x86_64-unknown-linux-gnu.tar.gz' },
};

function homeBinDir() {
  const homeDir = process.env.USERPROFILE || process.env.HOME || '.';
  return path.join(homeDir, '.harbour', 'bin');
}

function stampPath() {
  return path.join(homeBinDir(), 'VERSION');
}

function installedStamp() {
  try {
    return fs.readFileSync(stampPath(), 'utf8').trim();
  } catch {
    return '';
  }
}

function writeStamp() {
  fs.mkdirSync(homeBinDir(), { recursive: true });
  fs.writeFileSync(stampPath(), `${VERSION}\n`);
}

function needsRefresh(binPath) {
  return !fs.existsSync(binPath) || installedStamp() !== VERSION;
}

function getBinaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const target = PLATFORMS[key];

  if (!target) {
    console.error(`Unsupported platform or architecture: ${key}`);
    process.exit(1);
  }

  // Dev trees always win so `npx` from the repo uses the binary you just built.
  const localTarget = path.join(__dirname, '..', '..', 'target', 'release', target.bin);
  if (fs.existsSync(localTarget)) return localTarget;

  const wsTarget = path.join(process.cwd(), 'target', 'release', target.bin);
  if (fs.existsSync(wsTarget)) return wsTarget;

  const wsHarbourTarget = path.join(process.cwd(), 'harbour', 'target', 'release', target.bin);
  if (fs.existsSync(wsHarbourTarget)) return wsHarbourTarget;

  return path.join(homeBinDir(), target.bin);
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
      // Indexer not reachable — try to auto-spawn from known locations
      const homeDir = process.env.USERPROFILE || process.env.HOME || '.';
      const candidates = [
        path.join(homeDir, '.harbour', 'bin', 'harbour-indexer.exe'),
        path.join(__dirname, '..', '..', '..', 'harbour-indexer', 'target', 'release', 'harbour-indexer.exe'),
        path.join(process.cwd(), 'harbour-indexer', 'target', 'release', 'harbour-indexer.exe'),
        path.join(process.cwd(), '..', 'harbour-indexer', 'target', 'release', 'harbour-indexer.exe'),
      ];

      for (const candidate of candidates) {
        if (fs.existsSync(candidate)) {
          const indexerProc = spawn(candidate, [], { detached: true, stdio: 'ignore' });
          indexerProc.unref();
          break;
        }
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

async function ensureHomeBinary() {
  const key = `${process.platform}-${process.arch}`;
  const target = PLATFORMS[key];
  const binPath = path.join(homeBinDir(), target.bin);
  if (!needsRefresh(binPath)) {
    return binPath;
  }
  fs.mkdirSync(homeBinDir(), { recursive: true });
  const downloadUrl = `https://github.com/${REPO}/releases/download/v${VERSION}/${target.asset}`;
  console.log(`\x1b[36m[harbour]\x1b[0m Updating native binary to ${VERSION}...`);
  try {
    await downloadFile(downloadUrl, binPath);
    if (process.platform !== 'win32') {
      fs.chmodSync(binPath, 0o755);
    }
    writeStamp();
    console.log(`\x1b[32m[harbour]\x1b[0m Updated.`);
  } catch (err) {
    if (fs.existsSync(binPath)) {
      console.log(`\x1b[33m[harbour]\x1b[0m Update skipped (${err.message}); using existing binary.`);
    } else {
      console.log(`\x1b[33m[harbour]\x1b[0m Could not auto-download (${err.message}).`);
      console.log(`\x1b[36m[harbour]\x1b[0m Use the zip (harbour.exe + harbour-indexer.exe) or a GitHub release.`);
      process.exit(1);
    }
  }
  return binPath;
}

async function run() {
  const local = getBinaryPath();
  const binPath = fs.existsSync(local) ? local : await ensureHomeBinary();
  await ensureIndexer();

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
