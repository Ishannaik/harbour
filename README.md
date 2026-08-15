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

## Architecture

harbour is built on the **Stremio / Jackett model** to maintain full protocol neutrality:

- **`harbour` (this client):** A pure terminal BitTorrent client (`librqbit` engine, Ratatui TUI, queue manager, persistence, watch-now playback). It contains **zero scrapers** and connects to any search service implementing the open `Source` HTTP interface.
- **Indexer service:** A standalone, user-hosted service (such as `harbour-indexer`) that handles index lookups and caching over HTTP.

## Quickstart

### 1. Build the client
```bash
cargo build --release
```

### 2. Run
```bash
# Launch the search & downloads TUI
cargo run --release

# Or launch directly into a download
cargo run --release -- "magnet:?xt=urn:btih:..."
```

By default, search requests route to `http://127.0.0.1:8765`. Configure a custom indexer endpoint in `~/.harbour/config.toml` (Windows: `%USERPROFILE%\.harbour\config.toml`):
```toml
indexer_url = "http://127.0.0.1:8765"
```

## Stack

- **Language:** Rust (2024 edition)
- **TUI & Terminal:** [Ratatui](https://github.com/ratatui/ratatui), [Crossterm](https://github.com/crossterm-rs/crossterm)
- **BitTorrent Engine:** [librqbit](https://github.com/ikatson/rqbit)

## Disclaimer

harbour is a peer-to-peer file transfer utility and terminal frontend. It does not host, index, or distribute copyrighted materials. Users are solely responsible for ensuring that their network activity and downloads comply with applicable local laws and regulations.
