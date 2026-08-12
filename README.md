<h1 align="center">harbour</h1>

<p align="center">
  <b>Curated torrents straight from your terminal.</b><br>
  A Rust TUI. torlink-parity, omp-grade polish.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/Ishannaik/harbour"></a>
  <img alt="Language: Rust" src="https://img.shields.io/badge/language-Rust-orange">
</p>

## What it is

harbour is a terminal client for curated torrents. Browse a hand-picked
catalogue, search, and pull torrents without leaving the terminal.

> Status: UI MVP. Animated splash, keyboard-driven search, downloads queue
> with Seeding tab, and the `?` help overlay run against deterministic fake
> data; the engine and live sources land in later phases.

## Build

```bash
cargo build --release
```

## Usage

```bash
cargo run
```

## Stack

- Rust
- TUI (ratatui)
