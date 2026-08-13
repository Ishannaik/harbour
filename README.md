<h1 align="center">harbour</h1>

<p align="center">
  <b>Curated torrents straight from your terminal.</b><br>
  A Rust TUI. torlink-parity, omp-grade polish.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <img alt="Language: Rust" src="https://img.shields.io/badge/language-Rust-orange">
</p>

## What it is

harbour is a terminal torrent client. Browse, search, and pull torrents
without leaving the terminal — animated splash, keyboard-driven search,
downloads queue with a Seeding tab, watch-now playback, and a qBittorrent-level
settings screen.

## Architecture (Stremio-shaped)

harbour is split into two pieces, exactly the way Stremio splits its core from
its add-ons:

- **`harbour` — the client (this repo).** The terminal UI, the download engine
  (librqbit), the queue, persistence, watch-now, and settings. It ships **zero
  scrapers** and implements only the neutral `Source` interface. It's a
  BitTorrent client, period — legal everywhere.
- **`harbour-indexer` — the search service (separate, self-hosted).** Owns the
  torrent-index scrapers, the resilient fetch layer, and the search cache. It
  exposes a tiny JSON API. The client talks to it over HTTP; **you** run and
  point the client at whatever indexer you host (`indexer_url` in the config).
  The indexer is deliberately **not** hosted on GitHub — it's the piece you
  self-host, the same model as Stremio addons, Jackett, and Prowlarr.

The client never scrapes anything itself. You bring your own indexer — the
same model as Stremio addons, Jackett, and Prowlarr.

## Build

```bash
# client
cargo build --release

# indexer (separate, self-hosted — not on GitHub)
cargo build --release
```

## Run

```bash
# 1. start your indexer (it binds 127.0.0.1:8765 by default)
cargo run --release   # in the harbour-indexer repo

# 2. run the client; search now hits your indexer
cargo run
```

Set `indexer_url` in `~/.harbour/config.toml` (Windows: `%USERPROFILE%\.harbour\config.toml`)
if your indexer lives elsewhere.

## Stack

- Rust, edition 2024
- TUI (ratatui), input (crossterm)
- Downloads (librqbit)
