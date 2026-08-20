<div align="center">

```text
    __                 __                     
   / /_  ____ ______  / /_  ____  __  _______ 
  / __ \/ __ `/ ___/ / __ \/ __ \/ / / / ___/ 
 / / / / /_/ / /    / /_/ / /_/ / /_/ / /     
/_/ /_/\__,_/_/    /_.___/\____/\__,_/_/      
```

### BitTorrent from your terminal.

**Install** — Linux, macOS (Intel + Apple Silicon), and Windows binaries:

[**GitHub Releases**](https://github.com/Ishannaik/harbour/releases) · tag `v*` ships a versioned release · Actions → **release** → Run workflow publishes **nightly**

```bash
# Linux
tar -xzf harbour-linux-x86_64.tar.gz && ./harbour

# macOS Apple Silicon
tar -xzf harbour-macos-aarch64.tar.gz && ./harbour

# macOS Intel
tar -xzf harbour-macos-x86_64.tar.gz && ./harbour
```

Windows: unzip `harbour-windows-x86_64.zip` and run `harbour.exe`.

This archive is the client. Search talks to an indexer you run locally ([LEGAL.md](LEGAL.md)). Install `mpv` to watch.

Shots below use the Creative Commons demo catalog (Blender Foundation: Sintel, Big Buck Bunny, Tears of Steel). See [LEGAL.md](LEGAL.md).

<p align="center">
  <img src="docs/images/search.png" alt="Search: Sintel and other CC-BY Blender titles" width="900">
</p>
<p align="center">
  <img src="docs/images/downloads.png" alt="Downloads: Sintel in progress, Big Buck Bunny finished" width="900">
</p>
<p align="center">
  <img src="docs/images/settings.png" alt="Settings: mpv, titanium theme, local indexer, demo catalog" width="900">
</p>


A high-performance, asynchronous BitTorrent TUI client built in Rust with Ratatui & librqbit.

[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Ratatui](https://img.shields.io/badge/TUI-Ratatui-7aa2f7?logo=rust&logoColor=white)](https://github.com/ratatui/ratatui)
[![librqbit](https://img.shields.io/badge/Engine-librqbit-9ece6a)](https://github.com/ikatson/rqbit)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS%20%7C%20Windows-blue?logo=linux&logoColor=white)](#)
[![CI](https://img.shields.io/badge/CI-passing-success?logo=githubactions&logoColor=white)](#)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Protocol Neutral](https://img.shields.io/badge/Architecture-Protocol_Neutral-informational)](#stremio-style-addon-architecture)

</div>

---

## Overview

**Harbour** is a modern terminal BitTorrent client engineered for speed, aesthetics, and reliability.
It provides an interactive, keyboard-driven terminal experience with an animated splash sequence,
real-time multi-source search, concurrency-managed download queues with dedicated seeding views,
instant watch-now streaming playback, and full qBittorrent-grade session controls.

Harbour follows the **Stremio / Jackett addon paradigm**: the client is a 100% legal, protocol-neutral
BitTorrent client containing **zero scrapers or index databases**. It interacts with any search provider
implementing the open HTTP `Source` interface (see the [Custom Indexer Guide](docs/indexer-guide.md)).

---

## Features

### ⚡ 30fps DEC 2026 Synchronized Rendering
- **Zero-Flicker Terminal Graphics:** Emits terminal DEC 2026 synchronized update sequences (`\x1b[?2026h` / `\x1b[?2026l`) to ensure complete frame coherence without tearing.
- **Differential Screen Updates:** Only modified cells are redrawn via Ratatui diffing, minimizing CPU and terminal bandwidth.
- **Adaptive Cadence & Smooth Easing:** 30fps animation loop with adaptive frame backpressure and bounded easing interpolation for progress bars and download rates.

### 🧩 Stremio-Style Addon Architecture
- **Complete Protocol Neutrality:** Harbour core contains zero scrapers, trackers, or catalog parsers.
- **Decoupled Indexing:** Connects over a lightweight JSON/HTTP REST contract (`GET /search`, `GET /magnet`, `GET /health`) to a user-hosted indexer.
- **Fault-Isolated Parallel Search:** Search requests fan out across multiple upstream sources asynchronously. If a single source degrades or times out, healthy sources stream results uninterrupted.

### 🚀 Embedded `librqbit` Engine
- **Native Tokio Async Core:** Pure Rust BitTorrent implementation with metadata capture, DHT routing, and public tracker announcements.
- **Local Metadata Caching:** Captured `.torrent` binaries are atomically cached on disk (`~/.harbour/cache/torrents/<hash>.torrent`), enabling instant re-checks and re-seeding without swarm overhead.
- **Precise Queueing & Seed Ratios:** Configurable active download concurrency limits (`HARBOUR_MAX_DOWNLOADS`), oldest-first auto-promotion, and automated seeding ratio cutoffs.

### 🛡️ Bootguard Crash Recovery
- **Crash-Resilient State Machine:** Atomic ledger persistence (`downloads.json`) with staged writes and automated rollback.
- **Safe Mode Startup:** A bootguard canary marker detects unexpected process terminations; on recovery, Harbour enters Safe Mode, loading all previous items in a paused state to protect on-disk data.
- **Quarantine Safeguard:** Malformed state files are isolated to `.corrupt` archives with zero data loss or silent overwrites.

### 🎬 Interactive Search, Curated Top Lists & Watch-Now
- **Two-Pane Modal Search:** Fast search bar with animated shimmer feedback during active queries; empty queries instantly load curated top-tier lists.
- **Watch-Now Streaming:** Embedded HTTP Range-served streaming daemon paired with external media playback (`mpv` / `VLC`) for immediate video streaming while downloading.
- **Interactive Settings & Player Picker:** Real-time in-TUI configuration editor for bandwidth limits, listening ports, SOCKS5 proxies, and media players.

---

## Architecture

```mermaid
flowchart TD
    subgraph UI ["Terminal Interface (Ratatui TUI)"]
        A["Animated Splash & Views"] --> B["Two-Pane Search & Shimmer"]
        B --> C["Downloads & Seeding Tab"]
        C --> D["Watch-Now / Settings Modal"]
    end

    subgraph Core ["Harbour Client Core"]
        E["Action Dispatcher & State"]
        F["Queue Manager & Ledger (downloads.json)"]
        G["Bootguard Recovery Safeguard"]
        H["Theme Engine (67-token OMP schema)"]
    end

    subgraph Engine ["librqbit BitTorrent Engine"]
        I["Torrent Swarm / DHT / Trackers"]
        J["Disk I/O & Metadata Cache (.torrent)"]
        K["HTTP Range Stream Server (127.0.0.1)"]
    end

    subgraph External ["External Services & Players"]
        L["User Indexer (harbour-indexer) HTTP :8765"]
        M["External Player (mpv / VLC)"]
    end

    UI <-->|"30fps Sync / Key Events"| Core
    Core <-->|"tokio async channels"| Engine
    Core -->|"GET /search, GET /magnet"| L
    Engine -->|"HTTP Range Video Stream"| M
```

---

## Quickstart & Installation

### Prerequisites
- **Rust Toolchain:** Rust 2024 Edition (MSRV 1.85+)
- **Terminal Emulator:** Any modern terminal with Truecolor support (Windows Terminal, Alacritty, Kitty, WezTerm, Ghostty, iTerm2).
- **Optional Media Player:** `mpv` or `VLC` installed on `PATH` for Watch-Now streaming.

### Windows
Copies `harbour` onto your PATH (`%USERPROFILE%\.harbour\bin`):

```powershell
powershell -ExecutionPolicy Bypass -File install-harbour.ps1
```

Open a new terminal and type `harbour`.

### 1. Build from Source
```bash
# Clone the repository
git clone https://github.com/Ishannaik/harbour.git
cd harbour

# Build an optimized release binary
cargo build --release
```

### 2. Launch Harbour
```bash
# Launch interactive search and queue TUI
cargo run --release

# Or invoke the compiled binary directly
./target/release/harbour
```

### 3. Direct CLI Invocations
Harbour can launch directly into a download from magnet links, infohashes, or local `.torrent` files:
```bash
# Start directly from a magnet URI (always quote magnets to protect shell & characters)
harbour "magnet:?xt=urn:btih:3b245504fb5f3c40d7be923e5900de2957e84d72&dn=Ubuntu"

# Start directly from a 40-character hex infohash
harbour 3b245504fb5f3c40d7be923e5900de2957e84d72

# Start directly from a local .torrent file
harbour /path/to/distro.torrent
```

### 4. Running the Companion Indexer (Optional)
To enable multi-source searching across curated torrent indexes, run a companion indexer service such as [`harbour-indexer`](https://github.com/Ishannaik/harbour-indexer):
```bash
# Run the indexer (binds to http://127.0.0.1:8765 by default)
harbour-indexer
```

---

## Keybindings & Controls

Harbour is designed for rapid, fluid keyboard navigation.

### Global Navigation
| Shortcut | Action | Description |
| :--- | :--- | :--- |
| <kbd>Tab</kbd> | Switch Screen | Toggle between Search and Downloads screens |
| <kbd>?</kbd> | Help Overlay | Display interactive keybinding cheatsheet |
| <kbd>q</kbd> | Quit | Perform a clean, atomic shutdown and exit |
| <kbd>Ctrl</kbd> + <kbd>c</kbd> | Force Quit | Unconditional exit with terminal recovery |

### Search Screen
| Shortcut | Action | Description |
| :--- | :--- | :--- |
| <kbd>Enter</kbd> | Execute Search | Search active query (or browse curated Top Lists if empty) |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Navigate Results | Move selection up / down through search results |
| <kbd>d</kbd> | Default Download | Enqueue selected item to default download folder |
| <kbd>Shift</kbd> + <kbd>d</kbd> | Custom Download | Prompt for a custom destination directory and enqueue |
| <kbd>o</kbd> | Set Output Dir | Change and persist the global default download folder |
| <kbd>w</kbd> | Watch-Now | Stream selected media directly into an external player |
| <kbd>s</kbd> | Open Settings | Open the in-TUI configuration and theme editor |
| <kbd>Esc</kbd> / <kbd>Backspace</kbd> | Return to Input | Refocus the search input field for instant query refinement |

### Downloads & Seeding Screen
| Shortcut | Action | Description |
| :--- | :--- | :--- |
| <kbd>←</kbd> / <kbd>→</kbd> or <kbd>s</kbd> | Toggle Tab | Switch between Active Downloads and Seeding tabs |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Navigate Items | Move selection through the queue ledger |
| <kbd>p</kbd> | Pause / Resume | Toggle transfer state for selected download or seed |
| <kbd>r</kbd> | Retry | Restart a failed transfer from the last verified block |
| <kbd>x</kbd> | Remove | Delete item from ledger (preserves files on disk) |
| <kbd>w</kbd> | Watch-Now | Stream completed / playable item to external player |
| <kbd>Shift</kbd> + <kbd>p</kbd> | Player Picker | Select active streaming player (`mpv`, `vlc`, custom) |
| <kbd>Shift</kbd> + <kbd>s</kbd> | Open Settings | Access global bandwidth and network configuration |

---

## Configuration & Theming

Harbour creates its configuration workspace under `~/.harbour/` (Windows: `%USERPROFILE%\.harbour\`).

### Configuration (`config.toml`)
```toml
# Default directory for completed torrent payloads
download_dir = "~/Downloads"

# Active color theme (titanium, or any custom theme in ~/.harbour/themes/)
theme = "titanium"

# Automatically seed completed torrents
seed_by_default = true

# Upstream search indexer HTTP endpoint
indexer_url = "http://127.0.0.1:8765"

# External media player path (None = auto-detect mpv then vlc)
player = "mpv"

# Bandwidth Limits (in MiB/s, unset = unlimited)
download_limit_mib = 0
upload_limit_mib = 0

# Alternative Rate Limits (toggleable in settings)
use_alt_rates = false
alt_download_limit_mib = 2
alt_upload_limit_mib = 1

# Maximum concurrent active downloads (0 = unlimited)
max_active_downloads = 3

# Automated seeding ratio target (stops seeding when ratio reached)
stop_seed_at_ratio = false
seed_ratio = 1.5

# Network & Swarm discovery
enable_dht = true
enable_upnp = true
listen_port = 6881

# Custom trackers automatically appended to all torrents
trackers = [
  "udp://tracker.opentrackr.org:1337/announce",
  "udp://open.tracker.cl:1337/announce"
]
```

### Theming System
Harbour implements the complete 67-token **omp theme schema**.
- **Titanium (Tokyo Night):** Ships embedded as the default dark theme, utilizing deep midnight canvas (`#16161e`), electric cyan/blue accents (`#7aa2f7`), forest green indicators (`#9ece6a`), and coral warning highlights (`#f7768e`).
- **Custom Themes:** Drop custom JSON themes into `~/.harbour/themes/<name>.json`. Themes support `$var` expansion, ANSI 256 indexing, custom spinner glyphs, and hot-reload live on save.
- **Palette Fallback:** Automatically detects terminal color depth (`Truecolor` vs `256-color`), applying distance-weighted 6x6x6 color cube quantization for legacy terminals.

---

## Environment Variables

| Variable | Default | Description |
| :--- | :--- | :--- |
| `HARBOUR_MAX_DOWNLOADS` | `0` (unlimited) | Concurrency limit for simultaneous active downloads |
| `HARBOUR_SOURCE_TIMEOUT` | `10` | Per-source HTTP timeout in seconds during searches |
| `HARBOUR_STATE_DIR` | `~/.harbour` | Relocate configuration, state ledger, and torrent caches |

---

## Disclaimer

**Harbour is a peer-to-peer file transfer utility and terminal interface implementing the BitTorrent protocol.**
It does not host, index, promote, or distribute any torrent files or copyrighted materials.
The application operates strictly as a neutral network client connecting to user-specified swarms and endpoints.
Users are solely responsible for ensuring that all downloaded and shared content complies with the intellectual property laws and regulations of their jurisdiction.

---

<div align="center">
  <sub>Built with ❤️ in Rust. Released under the <a href="LICENSE">MIT License</a>.</sub>
</div>
