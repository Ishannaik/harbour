# harbour — SPEC

Crate `harbour`, binary `harbour`. Tagline: **"curated torrents straight from your terminal"**.
MIT (2026, Ishan Naik). Rust edition 2024, tokio async. Reference product: torlink
(github.com/baairon/torlink, TypeScript/Ink, MIT).

This document is the normative spec. Every requirement is testable; acceptance for v1 is
section 8. Naming, statuses, keybinds, sources, and phases follow the shared design context
verbatim — no alternatives.

---

## 1. Vision

harbour is a terminal-native torrent client that opens straight into a search bar: type a
query, press Enter, and curated results stream in from 10 hand-picked sources (FitGirl, YTS,
TPB, 1337x, EZTV, Nyaa, SubsPlease, BitTorrented) as each one answers, tagged with size,
seeders, and leechers. Downloads run in the background while you keep searching — queued,
throttled by a concurrency cap, resumable across restarts, and seeding by default on
completion — all rendered with omp-grade terminal polish: 30fps synchronized output, zero
flicker, eased progress bars, and a truecolor theme system. v1 targets feature parity with
torlink's interactive app; downloads and identity are fully local, with no central server.

## 2. Goals / Non-goals

### Goals

- **G-1** Feature parity with torlink's interactive app (search across 10 curated sources,
  background downloads with resume, seed-by-default, persistence, `harbour [magnet|infohash|.torrent]` CLI).
- **G-2** omp-level terminal polish: animated, truecolor, zero flicker.
- **G-3** Fully local: files stay on disk, no central server, no telemetry.

### Non-goals (deferred, verbatim from context)

- cs.rin.ru / online-fix.me sources (scraping feasibility unproven — Cloudflare,
  forum/catalog structure; spike in phase 7).
- Live streaming watch (phase 6, via libmpv + librqbit Range-served stream; no custom
  render engine — external player/libmpv is the renderer).
- Cover art / inline images.
- Headless daemon modes (`watch`/`serve`/`files`/`attach` + `--daemon`).
- Built-in updater.

## 3. Scope statement

**In scope (v1):** interactive TUI (splash, search, downloads, seeding views); the 10
sources in section 4.2; background downloading via librqbit with resume and seeding;
persistence (`downloads.json`, `history.json`, config, cache); bootguard safe mode;
omp-schema theming with custom themes; watch mode (phase 6) via libmpv.

**Out of scope (v1):** the section 2 non-goals; non-listed sources (no user-added custom
sources); a web UI; account/login systems; paid/private trackers; UDP-tracker-only swarms
requiring private announces; GPU/ASCII-image rendering.

**Defined later:** anything marked as an open question in section 9 is not a v1 commitment.

## 4. Functional requirements

### 4.1 CLI & startup (FR-01 … FR-10)

- **FR-01** `harbour` with no arguments starts the interactive TUI: splash view → search
  view with focus in the search input. Verified: launch, observe focus and splash.
- **FR-02** `harbour <magnet|infohash|.torrent>` validates the argument; on success it skips
  the search view and immediately enqueues that item as a download. Invalid input prints a
  usage error to stderr and exits non-zero without starting the TUI.
- **FR-03** `--help` prints usage (flags, positional arg, keybind summary) and exits 0.
- **FR-04** `--version` prints `harbour <semver>` and exits 0.
- **FR-05** An infohash argument must be 40 lowercase hex characters (or accepted as
  uppercase and normalized to lowercase); the magnet builder emits
  `magnet:?xt=urn:btih:<lowercase infohash>&dn=<name>`.
- **FR-06** On startup the config dir is resolved as `~/.harbour/` (Windows:
  `%USERPROFILE%\.harbour`) and created if missing; missing `config.toml` falls back to
  defaults without error.
- **FR-07** `HARBOUR_MAX_DOWNLOADS` env is read at startup: `0`/unset = unlimited; a
  positive integer = hard concurrency cap; invalid values fall back to unlimited with a
  warning.
- **FR-08** Crash marker is written at boot and cleared on clean exit (bootguard).
  Clean exit happens in exactly this order: **flush the ledger, then clear the marker, then
  restore the terminal, then exit**. A crash between flush and clear must leave the marker
  armed — clearing first would stand the breaker down over stale state. If the
  marker is present at startup (previous run died), every restored item starts paused and
  no engine starts until the user resumes — verified by killing the process mid-run and
  restarting.
- **FR-09** Terminal is restored unconditionally on exit: alternate screen leaves, hardware
  cursor returns, colors reset — including on panic (panic hook restores, then logs crash
  to a file).
- **FR-10** All network and engine work runs on tokio; the TUI never blocks on network I/O
  (verified by starting a download with a blackholed network and confirming the UI stays
  responsive).

### 4.2 Search & browse (FR-11 … FR-22)

- **FR-11** Enter with a non-empty query searches all 10 sources in parallel:
  fitgirl (Games, HTML scrape), yts (Movies, JSON API, yts.mx/.am/.rs fallback hosts),
  tpb-movies (Movies, JSON API apibay.org), x1337-movies (Movies, HTML scrape),
  eztv (TV, RSS), tpb-tv (TV, JSON API apibay.org), x1337-tv (TV, HTML scrape),
  nyaa (Anime, RSS), subsplease (Anime, RSS), bittorrented (Movies, HTML scrape).
  The fan-out happens **inside the user-run indexer** (`harbour-indexer`): the
  client sends one `GET /search` to `indexer_url` and the indexer runs the
  enabled scrapers concurrently, returning the concatenated results. The client
  never scrapes; user-disabled sources are sent as `exclude` so they are never
  queried.
- **FR-12** Enter with an empty query triggers curated top-list browsing (per-source
  curated items, same result display as search).
- **FR-13** Results stream into the list as each source answers — the UI renders partial
  results before all sources finish; a source's results appear no later than 1 render frame
  after its response is received.
- **FR-14** Every `TorrentResult` carries `{ info_hash, name, size_bytes, seeders,
  leechers, num_files?, source, magnet?, added? }`; a result missing `info_hash` or `name` is
  dropped, not rendered. `magnet` is **optional**: sources that hide it behind a detail page
  return none and the engine resolves it on demand when the user presses `d`. A displayable
  row never requires the magnet — fetching one detail page per row at search time is the
  single largest avoidable latency cost in the product.
- **FR-15** Per-source isolation: a source that errors, times out, or is unreachable
  shows an `offline` tag in the sidebar and is skipped; the other 9 sources continue and
  the search completes.
- **FR-16** Each source has a per-request timeout and retry policy; a source that fails
  its retries is marked `offline` for the remainder of that search.
- **FR-17** Search results are cached per (source, query) with a 5-minute TTL; a repeat
  search within TTL renders from cache without network activity (verified by a
  `HARBOUR_TEST_NET=0`-style offline run — cache hits still return rows; TTL expiry is unit-
  tested with a fake clock).
- **FR-18** Sidebar shows the 4 groups (Games/Movies/TV/Anime), each listing its sources with
  a live health dot that updates as the search progresses. States are `unknown` (never
  probed), `checking` (search in flight, no answer yet), `online`, `empty` (answered, no
  matches) and `offline` (failed or out of budget). `checking` and `unknown` are distinct
  from `offline`: a source that has not answered *yet* must never render as dead.
- **FR-19** Sources are polled only while a search is active; idle searches leave no
  in-flight network work (verified: no connections remain 5s after last result).
- **FR-20** Search cancellation: a new query cancels in-flight requests for the previous
  query; stale responses for the old query are discarded, never appended.
- **FR-21** `Source` trait: `{ id, label, groups, homepage, reports_health,
  search(query) -> Vec<TorrentResult> }`; all 10 sources implement it; multi-host fallback
  is per-source.
- **FR-22** Scraper robustness is covered by fixture tests: each HTML-scrape and
  JSON/RSS source parses committed fixture files (fixture → expected rows), and a fixture
  change breaks the test (no silent parse fallback).

### 4.3 Result display (FR-23 … FR-28)

- **FR-23** Each result row shows: name, source label, size, seeders, leechers.
  `size_bytes` renders as human units (B/KiB/MiB/GiB/TiB, one decimal for ≥1 KiB).
- **FR-24** Seeders and leechers are colored: seeders ≥ 100 green (success), 1–99 yellow
  (warning), 0 dim; leechers follow the same scale with the muted token. Colors come from
  the active theme, not hardcoded.
- **FR-25** Results merge into a **single list, deduplicated by `info_hash`**, keeping the
  copy reporting more seeders, sorted by seeders descending and then by date. Rows appear as
  their source answers and the list re-sorts as late results land. Deduplication happens in
  the search-orchestration layer, never in a scraper — a source cannot know what the others
  returned.
- **FR-26** Staggered source tags: each source's block header tag appears with a slight
  stagger on arrival (e.g. 80ms apart) — pure presentation, never reorders rows.
- **FR-27** Arrow keys navigate the list (Up/Down move one row, wrap at ends); the
  selected row is highlighted with `selectedBg`; Enter on a row is not required (Enter in
  the input re-searches).
- **FR-28** Search bar shows a shimmer while results are streaming and stops shimmering
  when all sources have answered (verified by buffer snapshot before/after last source).

### 4.4 Downloads (FR-29 … FR-41)

- **FR-29** `d` on a selected result enqueues it to the default output folder (from
  `config.toml`); `shift+d` prompts for a folder first, then enqueues; `o` changes the
  default output folder (persisted).
- **FR-30** Any number of items can be queued (unlimited queue); statuses progress
  `queued → downloading → failed`, and on completion move to `seeding`. `p` moves a
  downloading or seeding item to `paused` and back.
- **FR-31** Concurrency cap: at most `HARBOUR_MAX_DOWNLOADS` items download at once;
  when a slot frees, the oldest `queued` item is promoted (`promote()`) automatically.
- **FR-32** Active download rows show: animated progress bar, speed, peers, and ETA.
  Peers and ETA are **absent, not zero**, whenever the engine cannot report them (paused,
  initializing, or errored): the UI renders an em dash, never `0`, so a paused item is
  never mistaken for one nobody is connected to. Otherwise:
  values refresh at the 30fps render cadence with a 500ms stats poll from the engine.
- **FR-33** Progress bars ease toward the target value — the rendered value never jumps
  more than a bounded per-frame step (fixed-tick determinism test).
- **FR-34** Downloads continue while the user searches/browses; starting a new search
  never pauses or slows active transfers (verified with an active transfer + concurrent
  search).
- **FR-35** Interrupted downloads resume: cancel + relaunch (or crash + bootguard
  recovery) resumes from saved piece state via librqbit session state — verified with a
  real tiny magnet under `HARBOUR_TEST_NET=1`.
- **FR-36** Engine errors render as an error banner (omp errorBanner style) and set the
  item status `failed` with the error message; the item remains visible in the downloads
  list with a retry affordance.
- **FR-37** When torrent metadata arrives, the `.torrent` bytes are saved to
  `cache/torrents/<id>.torrent`; re-adding/re-seeding later verifies on-disk files locally
  from that file without re-fetching from the swarm.
- **FR-38** Completed items drop into "recently downloaded" with a completion marker and
  start seeding by default.
- **FR-39** `harbour <magnet|infohash|.torrent>` (FR-02) downloads into the default output
  folder and behaves identically to an interactive `d` enqueue.
- **FR-40** The user can change the output folder per item at enqueue time only
  (`shift+d`); the per-item folder is recorded in the ledger and used on resume.
- **FR-41** All download mutations (enqueue, promote, pause, resume, remove) are applied
  through the engine via the input→action channel; UI never mutates engine state directly
  (unit-tested action layer).

### 4.5 Seeding (FR-42 … FR-47)

- **FR-42** Finished torrents seed by default; seeding is per-item and trackers override
  are supported when the tracker advertises a non-seed role.
- **FR-43** `p` on a seeding item pauses seeding; pressing it again resumes. `p` never
  removes an item or deletes data — removal is a separate, explicitly confirmed action.
- **FR-44** Seeding tab shows per-item upload speed and peer count (em dash when unknown,
  per FR-32), refreshed at the same
  500ms poll cadence.
- **FR-45** Missing-file detection: a completed item the engine reports as live and
  *downloading again* has lost its files on disk — a real seed never pulls data, because
  verification reads the disk. It is flagged `missing`, the torrent is stopped, and nothing
  is re-downloaded. The detector requires consecutive observations past a grace window (a
  fresh re-seed legitimately looks identical while it verifies); the thresholds are derived
  from observed engine behaviour in the E1 spike rather than copied from the reference
  product, whose constants describe a different engine. **An engine error is never
  `missing`** — that is `failed` (FR-36). `missing` items render with a distinct tag and do
  not count as active seeds. Acceptance is behavioural: delete a seed's files, the item goes
  `missing`, and no re-download starts.
- **FR-46** A seed that becomes `missing` does not block downloads or other seeds; the
  user can re-check or remove it.
- **FR-47** Seeding state persists across restarts (see 4.6); on bootguard recovery all
  seeds start paused until resumed.

### 4.6 Persistence (FR-48 … FR-56)

- **FR-48** `downloads.json` is the ledger: one entry per known item with info_hash,
  name, source, magnet, output folder, and status
  (`queued|downloading|paused|failed|seeding|missing`). It is written atomically
  (write-temp + rename) on every status change.
  **Live statistics are never persisted**: progress, speed, peers and ETA come from the
  engine at runtime, and piece-level resume state is librqbit's per FR-50, so a persisted
  copy would be either stale between writes or a whole-file rewrite twice a second per
  item. Ledger writes are debounced and flushed synchronously on exit.
- **FR-49** `history.json` records **search queries** with a hard cap of 500 entries, oldest
  evicted first, de-duplicated; the cap is enforced on write and verified by a unit test. The
  recently-downloaded list is a different thing: it is derived from the ledger (items with
  `finished == true`) rather than kept in a second file, so there is one source of truth for
  what has completed.
- **FR-50** On startup the ledger is loaded and reconciled against librqbit session
  state; piece-level resume state comes from librqbit's session, not from `downloads.json`.
- **FR-51** Config (`config.toml`) persists: default output folder, theme name, and
  seed-by-default toggle; invalid config values fall back with a loud warning banner.
- **FR-52** Cache layout is `cache/search/<source>/<query>.json`,
  `cache/torrents/<id>.torrent`, `cache/covers/` (covers unused in v1, directory reserved);
  search cache respects the 5-minute TTL from FR-17.
- **FR-53** Bootguard (FR-08) on recovery: every restored item is paused, engines start
  only on explicit user resume, and a banner explains the safe-mode reason.
- **FR-54** Corrupt ledger/history JSON never crashes startup: the file is quarantined
  (renamed with `.corrupt` suffix), defaults load, and a warning banner shows.
- **FR-55** All persistence writes are crash-safe (temp file + atomic rename on the same
  volume); a crash mid-write leaves either the old or the new file, never a partial one.
- **FR-56** Duplicate detection: enqueuing an info_hash already in the ledger focuses the
  existing item instead of creating a duplicate.

### 4.7 Watch mode — phase 6 (FR-57 … FR-61)

- **FR-57** `w` on a playable (seeding/complete) item opens the now-playing view and
  streams the file through a librqbit HTTP Range-served stream endpoint to libmpv — the
  external player is the renderer; harbour ships no render engine.
- **FR-58** The stream endpoint serves HTTP Range requests so libmpv can seek; seeking
  works on complete and partially-downloaded-but-watchable files.
- **FR-59** Playback progress and player state are reflected in the now-playing view;
  `q`/esc returns to the previous view and stops the stream cleanly.
- **FR-60** Watch mode only activates while the swarm has the requested piece ranges;
  insufficient data shows an error banner instead of a broken stream.
- **FR-61** The stream endpoint binds to loopback only (no external network exposure);
  verified by connecting from a non-loopback address and getting a refusal.

### 4.8 Code quality (FR-62 … FR-68)

- **FR-62** Shipped (non-test) code forbids `unsafe` and denies `unwrap_used`, `expect_used`,
  `panic`, `dbg_macro`, and `todo` — enforced via crate-root
  `#![cfg_attr(not(test), …)]` in `src/main.rs` (test code keeps its unwraps); clippy runs
  `--all-targets -- -D warnings` in CI. Documented invariants may use
  `#[expect(…, reason = "…")]` or `unreachable!` with the invariant stated.
- **FR-63** Maximum line length is 100 chars, pinned in `rustfmt.toml`; enforced by
  `cargo fmt --check` and a CI awk check over all tracked `*.rs` files. No exemptions:
  fixture raw strings are reflowed at whitespace-insensitive boundaries or moved to
  `src/sources/fixtures/` (HTML attribute values and JSON tokens cannot be split).
- **FR-64** CI (`.github/workflows/ci.yml`) runs on every push and PR: fmt check, clippy
  `-D warnings`, the offline test suite, the line-length check, `cargo-audit` (RustSec
  advisories), and `cargo-deny` (license policy per `deny.toml`, OSI-permissive only).
  Network-gated tests never run in CI; the job is cached and fail-fast.
- **FR-65** Build joy: `just check` / `just lines` / `just audit` mirror the CI pipeline
  locally; `.cargo/config.toml` caps build jobs at 8 so heavy builds never peg a dev
  machine; heavy verification runs in CI, not on dev machines.
- **FR-66** `main` is branch-protected: PR required, ≥1 approval, the `quality` CI check
  green, linear history, no force pushes or deletions. (Enabled via GitHub settings/API —
  an owner action; this FR records that it must stay on.)
- **FR-67** Size and complexity norms: functions ≤30 LOC excellent / 31–50 acceptable /
  51–80 review / >80 refactor; cyclomatic ≤10 target, 11–15 warn, >15 refactor;
  cognitive ≤10; nesting ≤3; params ≤4 (5+ review); files ≤500 preferred, 500–700
  review, 700–1000 strong refactor, >1000 justify. Exceptions: big data/contract tables
  (e.g. `core/types.rs`, the frozen shared contract), dense protocol/state machines, and
  co-located test modules (scraper parser + fixtures + tests). Splitting to satisfy a
  number is forbidden; the per-file signal starts at 700 and ratchets down as refactors
  land. **Mechanical backstop (live)**: `clippy.toml` sets `excessive-nesting-threshold
  = 4`, `cognitive-complexity-threshold = 15`, `too-many-lines-threshold = 120`; those
  config keys enable their lints, so violations fail CI under `-D warnings`. Per-file
  LOC remains review pressure only.
- **FR-68** `src/app.rs` (1420 LOC — the one file FR-67's exceptions do not save) is
  decomposed by responsibility (splash / loop / watch / dispatch), boundaries drawn per
  concept, not to a line count; the FR-67 signal tightens after it lands.

## 5. UI/UX requirements (UR)

- **UR-01** Views, in order: splash (animated logo draw-in + gradient sweep) → search
  (sidebar: groups + source-health dots; gradient search bar with shimmer while results
  stream; results with size/seeders colored; staggered source tags) → downloads (active
  animated bars + speed/peers/ETA; recently downloaded; Seeding tab) → now-playing
  (phase 6). Navigation: `?` toggles the keybind help overlay; `q` quits.
- **UR-02** Render cadence: 30fps base with coalesced render requests and adaptive
  backpressure from the previous frame's cost — a frame only renders when the previous one
  finished (verified by frame-timing test: no render starts while a previous render runs).
- **UR-03** Synchronized output: every frame is wrapped in DEC 2026 begin/end synchronized
  update sequences (BSU/ESU); no frame renders outside a sync pair (verified by escape-
  sequence unit test). Zero flicker is the observable acceptance.
- **UR-04** Differential rendering: only changed cells are redrawn (ratatui diff); a
  static frame with no state change issues no cell writes (buffer-diff test).
- **UR-05** Loader: 80ms spinner advance (~12.5fps status / ~30fps activity); animated
  colorizers on the status line.
- **UR-06** Progress bars are eased (values ease toward target, never jump — bounded
  step, see FR-33); speed/ETA tick at 30fps.
- **UR-07** Rounded borders `╭╮╰╯` with tee junctions on all panels; ASCII fallback when
  the terminal reports no unicode support.
- **UR-08** Alt-screen lifecycle: alternate screen entered on start, left on exit;
  hardware cursor hidden while the TUI draws its own; exit is synchronous — the process
  never waits on engine sockets (OS reclaims them).
- **UR-09** Terminal restore is unconditional (UR-08 + FR-09): on clean quit, panic, or
  crash, the terminal is restored to its prior state.
- **UR-10** Keybinds (normative): Enter=search, `d`=download to default folder,
  `shift+d`=download to folder, `o`=change output folder, `p`=pause/stop seed,
  `?`=help, `q`=quit, `w`=watch (phase 6). Screen navigation: `Tab` cycles
  search ⇄ downloads; `←`/`→` switch the downloads tabs (Downloads/Seeding);
  `esc` closes the help overlay. `?` shows exactly these.
- **UR-11** Every async operation shows state: streaming search shows shimmer + per-source
  dots; downloads show bars; empty results show an empty state ("no results — try another
  query"), never a blank pane.
- **UR-12** Layout is responsive: resizing the terminal re-flows panels without panics or
  clipped rendering; minimum supported size is 80×24 (below that, a resize hint banner).
- **UR-13** Error banner (omp errorBanner style) is the single channel for engine/config
  errors; banners are dismissible and never overlap the active input.

## 6. Theme requirements (TR)

- **TR-01** The omp theme JSON schema is ported verbatim: `name`, `colors` (required
  tokens), `vars` (recursive refs), `symbols` (preset `unicode|nerd|ascii`, per-key
  overrides, `spinnerFrames`), optional `export`.
- **TR-02** Default dark theme is **titanium** (Tokyo Night palette): accent `#7aa2f7`,
  success `#9ece6a`, error `#f7768e`, warning `#e0af68`, muted `#565f89`, dim `240`,
  text `#c0caf5`, selectedBg `#2a2f45`, bg/statusLineBg `#16161e`, border `#4c566a`,
  syntaxComment `#565f89`, syntaxKeyword `#bb9af7`, syntaxFunction `#7aa2f7`,
  syntaxString `#9ece6a`, syntaxNumber `#ff9e64`, syntaxType `#2ac3de`,
  syntaxOperator `#89ddff`, syntaxPunctuation `#9aa5ce`. The full token set is
  documented in `docs/theming.md` with per-token "used by harbour" annotation.
- **TR-03** harbour uses a curated subset of the omp token set for its views; tokens
  marked omp-app-specific are accepted but ignored (schema-valid, never rendered).
- **TR-04** Custom themes live in `~/.harbour/themes/<name>.json` and are selectable via
  `config.toml` `theme` key; a theme file missing required `colors` tokens fails loudly
  and falls back to titanium with a warning banner.
- **TR-05** Theme live reload: editing the active custom theme file re-renders with the
  new colors without restart; invalid intermediate states fall back to defaults until the
  file is valid again (debounced, verified by file-watch test).
- **TR-06** Theme files validate against the ported schema before load; validation errors
  are loud (warning banner naming the file and error), never silent.
- **TR-07** Color-mode detection: `COLORTERM=truecolor` or `WT_SESSION` present →
  truecolor; otherwise 256-color palette mapping. Detection runs once at startup; theme
  tokens map through the active mode.
- **TR-08** Every UI color is a theme token reference — no hardcoded colors in view code
  (verified by code review + a theme where all tokens are garish high-contrast, which
  must visibly change every themed element).

## 7. Non-functional requirements (NFR)

- **NFR-01 (Performance)** Render loop holds 30fps on a 1080p/120Hz terminal with 200+
  rows of results: frame time budget ≤ 33ms p95; a frame that overruns coalesces the next
  render instead of dropping state (backpressure, UR-02).
- **NFR-02 (Performance)** Input latency: keypress → visible state change ≤ 50ms p95
  (measured with synthetic input + frame timestamps).
- **NFR-03 (Performance)** Fast startup: TUI interactive (splash visible) **≤ 100ms p95**
  after process start on the reference machine, excluding first-run state-directory
  creation; network work never delays first paint. The budget sits far below what the
  reference product achieves on purpose: lighter and faster has to be a number we work for,
  not one we inherit.
- **NFR-04 (Performance)** Low idle CPU: with no active search/download, render loop idles
  at ≤ 2% of one core (frames suppressed when state is unchanged — differential render +
  coalescing, UR-04).
- **NFR-05 (Reliability)** Per-source isolation: any single source hanging or failing
  never blocks another source's results or the UI (FR-15); source timeouts are enforced
  by abort signals.
- **NFR-06 (Reliability)** Panic safety: any panic logs a crash file and restores the
  terminal (FR-09); the TUI never exits with the terminal left in alt-screen.
- **NFR-07 (Reliability)** Crash recovery: an unclean kill mid-download resumes piece
  state on next launch; bootguard pauses engines until resumed (FR-08/FR-53).
- **NFR-08 (Portability)** Primary target Windows Terminal (Windows 11); macOS and Linux
  terminals are supported for the same feature set with no conditional compilation
  beyond paths/colors. Path resolution honors `%USERPROFILE%` vs `$HOME`.
- **NFR-09 (Portability)** Truecolor where available, 256-color fallback otherwise
  (TR-07); no feature requires a specific terminal emulator beyond DEC sync support
  (skipped gracefully when unsupported — banner-free, frames still render).
- **NFR-10 (Security/Privacy)** Files stay on disk: no uploads, no central server, no
  telemetry, no network calls beyond source fetching, tracker/DHT traffic for the user's
  own transfers, and the loopback stream endpoint (FR-61).
- **NFR-11 (Security)** Path safety: all cache/ledger paths are derived from known ids
  (info_hash) and never from untrusted result names (no path traversal via a crafted
  torrent name) — unit-tested.
- **NFR-12 (Maintainability)** Every public item is rustdoc'd; non-obvious invariants get
  a why-comment at the decision site (code-comment convention from context).
- **NFR-13 (Footprint)** Idle resident memory while seeding 20 torrents with no active
  download stays within the budget recorded in `docs/engine-spike-librqbit.md` §5.
- **NFR-14 (Footprint)** The release binary stays within the size budget recorded in the
  same place. Both numbers come from E1 measurements rather than guesses.
- **NFR-15 (Reliability)** No failure in config, ledger, theme, engine construction, or
  restore may stop the app reaching a usable screen, and no subsystem failure may escalate
  to another. Every failure mode has a defined fallback (`docs/plan-engine.md` §4.2).

## 8. Acceptance criteria — v1 checklist (all must pass)

1. `harbour` boots to splash → search with focus in the input; `--help`/`--version` work.
2. A live query returns streaming results from all 10 sources (or the source marked
   `offline` in the sidebar with the other 9 present) — manual smoke against live sources.
3. Results show name/source/size/seeders/leechers with the FR-24 colors; navigation and
   `?` keybind overlay work.
4. `d` downloads to the default folder; `shift+d` prompts for a folder; `o` changes and
   persists the default folder; `HARBOUR_MAX_DOWNLOADS` caps concurrency with oldest-first
   `promote()`.
5. Downloads show progress/speed/peers/ETA; an interrupted download resumes after
   restart (real tiny magnet, `HARBOUR_TEST_NET=1`).
6. Completed items seed by default and appear in recently downloaded; `p` pauses/seeds;
   Seeding tab shows upload speed/peers; the stray-download detector flags `missing`.
7. Engine error → error banner + `failed` status with message; source down → `offline`
   tag, search continues.
8. Everything persists: `downloads.json`/`history.json` (cap 500) survive restart;
   corrupt files are quarantined with a warning; bootguard safe mode pauses all engines
   after a crash.
9. Search cache honors the 5-minute TTL (cache hit renders offline, TTL expiry unit-
   tested).
10. Terminal restore is verified by scripted quit and by forced panic — both restore the
    terminal.
11. Titanium theme renders all themed elements; a custom theme in
    `~/.harbour/themes/<name>.json` loads, live-reloads, and fails loudly on invalid
    schema.
12. Full test suite green: scraper fixtures, theme-schema validation, cache TTL, ratatui
    buffer snapshots per view, fixed-tick animation determinism.
13. (Phase 6) `w` streams a complete item to libmpv over loopback Range requests with
    seek; non-loopback connections refused.
14. No flicker: DEC 2026 sync output wraps every frame (observable on Windows Terminal).

## 9. Open questions

- ~~**OQ-1**~~ *Resolved:* `p` is pause-only; deletion sits behind an explicit confirm.
  librqbit offers both `pause`/`unpause` and `delete(delete_files)`, so this was a product
  decision rather than a technical one.
- **OQ-2** Result pagination: source responses can exceed one screen; whether blocks
  virtualize (scroll within a source's block) or paginate per source is undecided —
  ratatui buffer tests depend on this.
- **OQ-3** History cap 500 eviction is oldest-first, but whether history feeds a search
  suggestion UI (and its keybind) is undecided — not a v1 commitment.
- **OQ-4** Non-goals carry feasibility spikes in phase 7 (cs.rin.ru/online-fix.me
  scraping, cover art via sixel/halfblocks, headless daemons); their specs are deferred
  until the spikes conclude.
- **OQ-5** EZTV/Nyaa/SubsPlease RSS mirrors can rotate hosts like YTS's; the canonical
  host list per source lives in `docs/sources.md` and may gain fallbacks without a spec
  change.
- **OQ-6** `bittorrented` is an HTML scrape with no defined mirror policy; whether it gets
  multi-host fallback like YTS is decided when its first scrape fixture lands.
- **OQ-7** Minimum supported terminal size (UR-12 says 80×24) may be raised if watch-mode
  playback controls require more room — revisit in phase 6.
