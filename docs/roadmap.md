# harbour — roadmap

Status: planning. Repo has no code yet; phases describe the intended build order.
Scope: v1 = feature parity with torlink's interactive app, then watch mode, then
deferred spikes. Phase list is normative (see `harbour-context.md`); every task
below traces to a named requirement in `SPEC.md`.

## 1. Milestone overview

Three milestones:

| Milestone | Phases | Deliverable |
| --- | --- | --- |
| M0 — v1: torlink interactive parity | 1–5 | Search 10 real sources, download/seed via librqbit, persistence + resume, polished ratatui TUI |
| M1 — Watch mode | 6 | Stream seeding torrents to libmpv via librqbit HTTP range endpoint; now-playing view |
| M2 — Deferred spikes | 7 | Feasibility spikes only: cs.rin.ru, online-fix.me, cover art, headless daemons |

M0 is the shipped product. M1 is deliberately *not* in v1: live streaming watch
is listed as an explicit v1 non-goal in the context, but it is the first thing
built after v1 because it reuses M0's engine and ledger directly. M2 items are
unproven or out of parity scope; none is committed until its spike passes.

## 2. Phases

### Phase 1 — Skeleton

Goal: a compiling crate whose terminal lifecycle, theme loading, and animation
loop are production-grade before any product code lands on top.

Tasks:

- Scaffold crate `harbour-tui` (binary `harbour`, edition 2024, tokio runtime, clap CLI with `--help`/`--version` and a positional `magnet|infohash|.torrent` argument).
- Port the omp theme JSON schema to Rust structs (`name`, `colors`, `vars`, `symbols`, optional `export`) with serde, validating required tokens.
- Theme loader: `~/.harbour/themes/<name>.json` (Windows `%USERPROFILE%\.harbour`), live reload on file change, loud validation errors with fallback to defaults.
- Ship the default theme **titanium** (Tokyo Night palette) embedded; truecolor detection via `COLORTERM=truecolor` or `WT_SESSION`, else 256-color.
- Animation subsystem: 30fps base cadence, coalesced render requests, adaptive backpressure from previous frame cost.
- DEC 2026 synchronized output (BSU/ESU) around every frame write — zero flicker.
- Terminal lifecycle: alt-screen, hidden hardware cursor, differential rendering (ratatui diff), unconditional restore on exit and panic, crash log to file.
- Loader primitives: 80ms spinner advance (~12.5fps status cadence), eased progress bars (value eases toward target, never jumps), 30fps speed/ETA tick.

Definition of done:

- `harbour --version` and `harbour --help` work; unknown theme JSON produces a loud warning and falls back to titanium.
- A blank ratatui frame renders at 30fps with BSU/ESU with no flicker in Windows Terminal; frame cost feedback loop is measurable (adaptive backpressure active).
- Killing the process mid-frame restores the terminal unconditionally; panic path writes a crash log and restores the terminal (verified in tests).
- Theme-schema validation tests and fixed-tick animation determinism tests pass.

Dependencies: none.

### Phase 2 — Splash + search UI with fake data

Goal: the complete interactive search flow, end to end, driven by a fake-data
engine — the UI contract is final before any real scraping or downloading exists.

Tasks:

- Define the shared core types once, in one module: `TorrentResult { info_hash, name, size_bytes, seeders, leechers, num_files?, source, magnet, added? }`, source metadata, `QueueItem`, and the engine event enum (search results, status transitions) — the mpsc contract phase 4 implements against.
- Splash view: animated logo draw-in + gradient sweep.
- Search view: sidebar with the 4 groups (Games/Movies/TV/Anime) and 10 source-health dots, gradient search bar with shimmer while results stream.
- Results list: size and seeders colored, staggered source tags, arrow-key navigation, rounded borders `╭╮╰╯` + tee junctions.
- Fake-data engine: a deterministic seeded generator per source id that streams results with per-source latency and per-source offline simulation.
- Downloads view scaffold: active downloads with animated bars + speed/peers/ETA, recently-downloaded list, Seeding tab.
- Keybinds wired to view actions: Enter search, empty Enter = curated top lists, `d` download default folder, `shift+d` download to folder, `o` change output, `p` pause/stop seed, `?` help, `q` quit (downloads are no-op stubs until phase 4).
- Error banner plumbing (omp errorBanner style) for engine/source failures.
- ratatui buffer-snapshot tests per view; keybind tests.

Definition of done:

- Keyboard-driven search, streaming results, offline simulation, and all keybinds work end to end against fake data; `?` help lists the correct keybinds.
- Buffer-snapshot tests pass for splash, search, downloads, and help views; shared types are stable (phase 4 can depend on them unmodified).

Dependencies: Phase 1.

### Phase 3 — Real scrapers + cache

Goal: all 10 sources answer real searches with real metadata; one dead source
never blocks the others.

Tasks:

- Implement the `Source` trait `{ id, label, groups, homepage, reports_health, search(query) -> Vec<TorrentResult> }` and the 10-source registry.
- Resilient fetch layer: reqwest, per-source timeout, retries, abort signal, multi-host fallback per source.
- GamesHub HTML scraper (Games).
- CineVault JSON API scraper with cinevault.mx / .am / .rs fallback hosts (Movies).
- VaultIndex JSON API scraper on mirror-api.org for both `vault-movies` and `vault-tv` (Movies, TV).
- ReelIndex HTML scrapers for `reel-movies` and `reel-tv` (Movies, TV).
- ShowPort RSS via quick-xml (TV).
- TsukiBase RSS and FanSubs RSS via quick-xml (Anime).
- TorrentHub HTML scraper (Movies).
- Magnet builder: `magnet:?xt=urn:btih:<lowercase info_hash>&dn=<name>`.
- Search-result cache: `cache/search/<source>/<query>.json`, 5-minute TTL per (source, query).
- Per-source isolation: any source failing times out → `offline` dot in sidebar, search continues without it.
- Scraper unit tests on fixture HTML/JSON/RSS per source; cache TTL tests; `HARBOUR_TEST_NET=1`-gated integration tests; manual smoke against live sources before release.

Definition of done:

- Every source returns real, parsed `TorrentResult`s in fixture tests; live smoke against all 10 sources yields results (or a correctly reported `offline`).
- Cache hits within TTL, expiry after 5 minutes; a dead source is tagged offline without delaying other sources' results.

Dependencies: Phase 2 (consumes the shared `TorrentResult`/`Source` types; fake engine is replaced, not extended).

### Phase 4 — librqbit integration: download/progress/seed

Goal: real downloads with live progress, seeding by default, and a sane queue.

Tasks:

- Embed librqbit; construct the engine from magnet / infohash / `.torrent` input; wire engine events → mpsc → UI state at the 500ms stats poll.
- CLI path: `harbour [magnet|infohash|.torrent]` launches straight into that download.
- Queue with statuses `queued → downloading → failed`, and `seeding → missing` on completion/stray detection; `HARBOUR_MAX_DOWNLOADS` concurrency cap (0/unset = unlimited); oldest-first `promote()` when a slot frees.
- Downloads view: live eased progress, speed, ETA, peers; recently-downloaded list fills.
- Seeding tab: upload speed, peers, per-item pause/stop (`p`); seed-by-default with trackers-override support.
- Wire `d` (default folder), `shift+d` (folder picker per download), `o` (change output folder) to the engine.
- Metadata capture: when torrent metadata arrives, save the `.torrent` bytes to `cache/torrents/<id>.torrent` so re-add/re-seed verifies locally without re-fetching from the swarm.
- Stray-download detector: a seed reporting `speed > 0 && progress < 1` for 2 consecutive polls after a 10s grace period → flag `missing`.
- Error path: engine error → error banner + item status `failed` with the message.

Definition of done:

- A real tiny magnet (gated behind `HARBOUR_TEST_NET=1`) downloads to disk, seeds afterward, and pause/stop works; progress/speed/ETA update live at 30fps.
- Concurrent downloads capped by `HARBOUR_MAX_DOWNLOADS`; queued items auto-promote oldest-first; engine failure surfaces as banner + `failed` status.

Dependencies: Phase 1 (runtime/lifecycle), Phase 2 (shared types). Can run in parallel with Phase 3 — no scraper dependency.

### Phase 5 — Persistence + bootguard + resume

Goal: harbour survives restarts; interrupted downloads resume; a crash never
auto-starts engines without the user's say-so.

Tasks:

- Config dir resolution (`~/.harbour/`, Windows `%USERPROFILE%\.harbour`) and `config.toml` load with validation and loud fallback.
- `downloads.json` ledger: queue, statuses, output paths, torrent ids; atomic writes.
- `history.json`: search history, hard cap 500 entries.
- librqbit session-state save/load for piece-level resume of interrupted downloads.
- Bootguard: crash marker written at boot; if the previous run died mid-restore, restore every item paused (safe mode) — no engines start until the user resumes.
- Resume interrupted downloads on startup; seed-by-default resumes too.
- Recently-downloaded list persisted and restored.
- Tests: ledger round-trip, corrupt-file fallback, bootguard safe-mode behavior.

Definition of done:

- Kill the app mid-download and relaunch → the item resumes from saved piece state; queue and recently-downloaded restore correctly.
- Simulated crash → safe mode: everything paused, zero engines start, a banner explains; corrupt `downloads.json`/`config.toml` fall back loudly instead of panicking.

Dependencies: Phase 4 (queue semantics, librqbit session state).

### Phase 6 — Watch mode

Goal: stream a seeding torrent's file to libmpv and show a now-playing view —
"tori stack": librqbit serves, libmpv renders. No custom render engine.

Tasks:

- librqbit HTTP streaming endpoint serving file ranges (Range requests) for seeding torrents.
- axum server hosting the stream endpoint, bound to localhost with a token guard.
- `w` keybind: launch libmpv (external player) on the focused torrent's primary file.
- Now-playing view: title, playback progress, pause/play, clean return to the TUI on exit.
- Coordinate player and engine lifecycle: player exit returns focus without disturbing seeding state.
- End-to-end smoke: watch a real downloaded torrent file.

Definition of done:

- Pressing `w` on a seeding torrent opens it in libmpv within a few seconds and plays; now-playing view tracks state; closing the player restores the TUI cleanly.

Open question: whether watching should pause the torrent's seeding while the
player has the file open — decide during spike, default is to leave seeding
untouched.

Dependencies: Phase 4 (engine + streaming), Phase 5 (ledger locates files).

### Phase 7 — Deferred spikes

Goal: prove or disprove feasibility of the deferred items. No production
commitment; each spike ends with a recommendation.

Tasks:

- cs.rin.ru scraper spike: Cloudflare challenge handling, forum/catalog structure, page stability.
- online-fix.me scraper spike: same feasibility questions.
- Cover art spike: fetch + store in `cache/covers/`, render via sixel or halfblocks in the results list.
- Headless daemon modes spike: `watch`/`serve`/`files`/`attach` + `--daemon` (engine without TUI).

Definition of done:

- Each spike produces a written verdict (feasible / infeasible / needs-X) with fixture evidence; no spike code is merged to main unless the verdict is "feasible" and the follow-up is scheduled.

Dependencies: Phase 3 (scraper patterns), Phase 4 (headless engine reuse).

## 3. Dependency graph

```
Phase 1 (skeleton)
 ├── Phase 2 (UI + fake data)
 │    └── Phase 3 (real scrapers + cache)
 ├── Phase 4 (librqbit engine) ──────────┐   ← 2 and 4 run in parallel
 │    └── Phase 5 (persistence + bootguard)
 │         └── Phase 6 (watch mode) ◄────┘   ← needs 4 (stream) + 5 (ledger)
 └── (scraper patterns from 3) + (engine from 4)
      └── Phase 7 (deferred spikes)
```

Hard edges:

- 1 → 2: the animation loop, theme loader, and terminal lifecycle are prerequisites for any view.
- 1 → 4: the tokio runtime and terminal lifecycle are prerequisites for the engine.
- 2 → 3: scrapers return the shared `TorrentResult` type and replace the fake engine's output path.
- 4 → 5: bootguard's safe-mode semantics and resume both depend on queue statuses and librqbit session state.
- 5 → 6: watch mode needs the ledger to locate files and the engine to serve ranges.
- 3/4 → 7: spikes reuse scraper patterns and the headless engine.

Everything else is parallelizable (see §6).

## 4. Definition of done — v1 (M0)

Summarized from SPEC acceptance criteria:

- **Search**: opens straight to the search bar; query + Enter streams results from all 10 sources as each answers, tagged with size and seeders; empty query + Enter browses curated top lists; dead sources are tagged `offline` and never block others.
- **Navigation**: arrow keys move the cursor; `d` downloads to the default folder; `shift+d` picks a folder for that download; `o` changes the output folder; `p` pauses/stops seeding; `?` shows keybinds; `q` quits.
- **Downloads**: run in the background while searching continues; unlimited queue with concurrency cap (`HARBOUR_MAX_DOWNLOADS`); live progress, speed, time left; finished items appear in recently-downloaded.
- **Seeding**: on by default after completion; opt out per item with `p`; Seeding tab shows upload speed and peers; trackers override supported.
- **Persistence**: queue, history, and recently-downloaded survive restarts; interrupted downloads resume piece-level; crash → bootguard safe mode.
- **CLI**: `harbour [magnet|infohash|.torrent]` launches straight into that download; `--help` and `--version` work.
- **Polish**: 30fps, zero flicker (DEC 2026 sync output), truecolor, no flicker on resize; terminal always restored even on panic.
- **Testing gates**: scraper fixture tests, theme-schema validation, cache TTL, buffer snapshots, fixed-tick animation determinism, `HARBOUR_TEST_NET=1` integration, live smoke before release.

M0 ships when every item above holds in a live session — not when the code compiles.

## 5. Deferred work register

| Item | Rationale | Status |
| --- | --- | --- |
| cs.rin.ru source | Scraping feasibility unproven — Cloudflare challenge, forum/catalog structure; untrusted repackers violate the "GamesHub alone" trust rule until proven | Spike in phase 7 |
| online-fix.me source | Same feasibility + trust questions as cs.rin.ru | Spike in phase 7 |
| Live streaming watch | Out of torlink-parity scope; needs libmpv integration + streaming endpoint; external player is the renderer by design | Phase 6 (first post-v1 milestone) |
| Cover art / inline images | Terminal support for sixel/halfblocks is uneven; polish, not parity; adds a fetch/cache pipeline | Spike in phase 7 |
| Headless daemon modes (`watch`/`serve`/`files`/`attach`, `--daemon`) | Power-user surface; TUI is the product; requires a stable engine API that only exists after M0 | Spike in phase 7 |
| Built-in updater | Distribution story unresolved (cargo install vs. packaged binaries); updater is a supply-chain surface, not a parity feature | No date |

## 6. Suggested parallelization

- **Phase 1 is serial** — everything hangs off it. One workstream, do not split.
- After phase 1, three parallel workstreams, each as its own subagent(s):
  - **UI track**: Phase 2 (one subagent), then Phase 3 merged in (scrapers can be one subagent per source family — HTML: gameshub/ReelIndex/torrent-hub; JSON: cinevault/vault-index; RSS: showport/tsukibase/fansubs — fan out up to 3, with the `Source` trait + registry as the shared contract).
  - **Engine track**: Phase 4 (one subagent), then Phase 5 (one subagent). Depends only on phases 1–2 shared types.
  - **Integration/verification**: joins after both tracks land; owns buffer snapshots, live smoke, and M0 acceptance run.
- Phases 2 and 4 are the biggest true parallelism win: UI (fake data) and engine (headless-capable) touch no shared code beyond the event enum and `QueueItem`, which phase 2 pins down first.
- Phase 6 starts only after 4 + 5 (streaming + ledger). Phase 7 spikes are independent of each other and can fan out in parallel once phases 3/4 patterns exist.
- Contract note: the shared types module (phase 2) is the single coordination point between tracks — freeze it before fan-out, treat changes as breaking.
