# harbour

**Curated torrents straight from your terminal.**

harbour is a terminal torrent client and search aggregator in the spirit of
[torlink](https://github.com/baairon/torlink) — type a query, hit Enter, and
watch results stream in from ten sources at once, then download with a single
keypress while the search keeps going underneath. No accounts, no API keys, no
browser tabs. Built in Rust on tokio with an omp-grade animated TUI: truecolor,
zero flicker, eased progress bars, and a 30fps render loop that feels like a
desktop app, because a download manager has no business looking like a log
file.

harbour is under active development. The feature list below describes what the
project is building toward; anything not yet shipped is called out explicitly.

## Features

- **Ten sources, one query** — searches GamesHub, CineVault, VaultIndex, ReelIndex, ShowPort, TsukiBase,
  FanSubs, and TorrentHub simultaneously, grouped into Games, Movies, TV,
  and Anime. A source that is down is tagged offline in the sidebar and search
  continues without it.
- **Streaming results** — matches appear as each source answers, tagged with
  size, seeders, and source, instead of waiting for the slowest scraper.
- **Animated downloads** — live progress bars with speed, peers, and ETA that
  tick at 30fps; eased bar animation instead of jarring jumps.
- **Auto-seed** — finished torrents keep seeding by default; opt out per item
  with `p`. A Seeding tab shows upload speed and peers.
- **Resume** — interrupted downloads pick up where they left off, and the whole
  queue persists across restarts.
- **omp-grade animated TUI** — ratatui + crossterm with differential rendering,
  DEC synchronized output, a splash screen with an animated logo draw-in, and
  theme support in omp's JSON schema (Tokyo Night "titanium" by default).
- **Zero setup** — no accounts, no API keys, no configuration required to get a
  working download. Point it at a magnet link and it just works.

## What it looks like

harbour opens with an animated splash (logo draw-in, gradient sweep), then drops
you into the search view: a sidebar of source groups with live health dots on
the left, a shimmering gradient search bar on top, and streaming results below.
The downloads view shows active torrents as animated bars with speed, peers,
and ETA; finished items land in "recently downloaded" with a Seeding tab for
upload stats.

## Under the hood

Rust 2024 edition on tokio. The UI is ratatui + crossterm with differential
rendering at a 30fps cadence and DEC synchronized output for zero flicker;
scraping is reqwest + scraper/quick-xml with per-source timeouts, retries, and
multi-host fallbacks; the download engine is embedded librqbit with DHT,
trackers, piece-level resume, and session-state persistence.

## Install

```sh
cargo install harbour-tui
```

Requires a Rust toolchain (edition 2024, stable). On Windows, **Windows
Terminal is recommended** — the TUI targets truecolor and modern terminal
protocols.

## Quick start

```sh
# Search
harbour

# Jump straight into a download
harbour <magnet>
harbour <infohash>
harbour <file.torrent>
```

Run `harbour` with no arguments and it opens straight to a search bar. Type a
query and press Enter — results stream in from all ten sources as each one
answers. An empty query browses curated top lists instead. Arrow keys move
through results; `d` downloads the selected item to the default folder.
Downloads run in the background while you keep searching.

Passing a magnet link, raw infohash, or `.torrent` file on the command line
skips search entirely and launches that download immediately.

Every item in the queue is in one of five states: `queued`, `downloading`,
`failed`, `seeding`, or `missing` (a seed whose files vanished from disk).
Engine errors surface as a banner and flip the item to `failed` with a message
instead of silently stalling.

## Keybinds

| Key | Action |
| --- | --- |
| `Enter` | Search (or browse top lists on an empty query) |
| `d` | Download to the default folder |
| `Shift+d` | Download to a folder you pick |
| `o` | Change the output folder |
| `p` | Pause / stop seeding |
| `?` | Keybind help |
| `q` | Quit |
| `w` | Watch the selected torrent (planned — watch mode is not built yet) |

## Sources

| ID | Label | Groups |
| --- | --- | --- |
| `gameshub` | GamesHub | Games |
| `cinevault` | CineVault | Movies |
| `vault-movies` | VaultIndex | Movies |
| `reel-movies` | ReelIndex | Movies |
| `showport` | ShowPort | TV |
| `vault-tv` | VaultIndex | TV |
| `reel-tv` | ReelIndex | TV |
| `tsukibase` | TsukiBase | Anime |
| `fansubs` | FanSubs | Anime |
| `torrent-hub` | TorrentHub | Movies |

Games is the only category that runs code, so it stays with GamesHub — a
trusted repacker. Search results are cached per source and query for five
minutes.

## Config

Configuration lives in `~/.harbour/` (`%USERPROFILE%\.harbour` on Windows):

```
config.toml            # settings
downloads.json         # download ledger
history.json           # recent searches / downloads (capped at 500)
cache/search/          # per-source search result cache
cache/torrents/        # saved .torrent metadata
cache/covers/          # reserved for cover art
```

### Environment

- `HARBOUR_MAX_DOWNLOADS` — maximum concurrent downloads. `0` or unset means
  unlimited; the queue promotes the oldest waiting item whenever a slot frees.

### Themes

Drop a theme file into `~/.harbour/themes/<name>.json` using omp's theme schema
and pick it from the config. Themes hot-reload on file change; a theme that
fails validation is rejected loudly and harbour falls back to the titanium
default. See [docs/theming.md](docs/theming.md) for the full schema and token
reference.

## Roadmap

The build is phased: TUI skeleton and animation loop, splash + search UI,
real scrapers, the librqbit download engine, persistence and crash-safe
resume, then watch mode. Details in [docs/roadmap.md](docs/roadmap.md).

Explicitly deferred (not planned for v1):

- `cs.rin.ru` and `online-fix.me` as sources — scraping feasibility unproven.
- Live streaming watch mode — planned for phase 6 via libmpv, not built yet.
- Cover art and inline images.
- Headless daemon modes and a built-in updater.

## License

MIT © 2026 Ishan Naik
