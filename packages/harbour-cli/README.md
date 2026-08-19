# harbour-cli

> Curated torrents straight from your terminal. A high-performance, asynchronous BitTorrent TUI client built in Rust with Ratatui & librqbit.

```bash
npx harbour-cli
```

## Overview

**Harbour** is a modern terminal BitTorrent client engineered for speed, aesthetics, and reliability.
It provides an interactive, keyboard-driven terminal experience with an animated splash sequence,
real-time search, concurrency-managed download queues with dedicated seeding views, and watch-now streaming playback.

## Quickstart

Run directly without installing:

```bash
npx harbour-cli
```

Or pass direct torrent arguments:

```bash
npx harbour-cli "magnet:?xt=urn:btih:..."
npx harbour-cli path/to/file.torrent
```

## Features

- **30fps DEC 2026 Synchronized Rendering:** Zero-flicker graphics and smooth easing progress bars.
- **Embedded librqbit Engine:** Async BitTorrent core, metadata caching, DHT, and fast session resume.
- **Bootguard Crash Recovery:** Safe-mode automatic recovery on interrupted runs.
- **Watch-Now Streaming:** HTTP Range-served streaming to external players (mpv/VLC).
- **Stremio-Style Architecture:** 100% legal protocol-neutral client communicating with self-hosted search indexers.

## Controls

| Key | Action |
|---|---|
| <kbd>Enter</kbd> | Search (empty query browses curated top lists) |
| <kbd>d</kbd> | Download to default directory |
| <kbd>Shift</kbd> + <kbd>D</kbd> | Download to custom directory |
| <kbd>←</kbd> / <kbd>→</kbd> | Switch between Downloads & Seeding tabs |
| <kbd>p</kbd> | Pause / resume download or seeding |
| <kbd>?</kbd> | Show keybindings help |
| <kbd>q</kbd> | Quit |

## Repository

- GitHub: [https://github.com/Ishannaik/harbour](https://github.com/Ishannaik/harbour)
- License: MIT
