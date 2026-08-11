# harbour — Design v2

*Formalized design for the harbour TUI (crate `harbour`, binary `harbour`). This
document describes the **intended** implementation; the repo has no code yet.
Normative source of truth: [`context.md`](context.md).*

## 1. Design summary

harbour is a curated torrent finder/downloader that opens straight to a search
bar. Type a query + `Enter` and results stream in from 10 sources as each
answers, tagged with size and seeders. `Enter` on an empty query browses
curated top lists. Downloads run in the background while you keep searching;
the queue is unlimited with a concurrency cap (`HARBOUR_MAX_DOWNLOADS`, 0/unset
= unlimited, oldest-first `promote()` when a slot frees). Everything persists
across restarts; interrupted downloads resume; finished torrents seed by
default with per-item opt-out (`p`).

The UX is a deliberate port of the omp harness's terminal discipline:

- ratatui + crossterm + tokio, **differential rendering** (only changed cells
  rewritten via ratatui's diff).
- **30fps base cadence** with coalesced render requests and adaptive
  backpressure from the previous frame's cost.
- **DEC 2026 synchronized output** (BSU/ESU) bracketing every frame — zero
  flicker, zero tearing.
- Loader spinners advance every **80ms** (~12.5fps for status-line spinners,
  ~30fps for activity); animated colorizers run on the status line.
- Progress bars **ease toward target, never jump**; speed/ETA tick at 30fps.
- Rounded borders `╭╮╰╯` with tee junctions throughout.

Flow: splash (logo draw-in + gradient sweep) → search (sidebar groups +
source-health dots, gradient search bar with shimmer while results stream) →
downloads (active animated bars + speed/peers/ETA, recently downloaded,
Seeding tab) → now-playing (phase 6, libmpv).

**Keybinds** (normative): `Enter` search · `d` download to default folder ·
`Shift+d` download to chosen folder · `o` change output folder · `p`
pause/stop seeding · `?` help · `q` quit · `w` watch (phase 6).

Terminal lifecycle: alt-screen, hardware cursor hidden (the TUI draws its
own), synchronous exit (never wait on engine sockets — OS reclaims them),
terminal restored unconditionally on panic and on exit. Crash logging to a
file.

Phases 1–7 and deferred spikes are owned by `docs/roadmap.md`; the theme
schema table lives in `docs/theming.md`; per-source mechanics live in
`docs/sources.md`.

---

## 2. Views & layouts

Views are ratatui widgets over a single UI state struct fed by engine events
through an mpsc channel; each view owns a buffer-snapshot test (see §9).

### 2.1 Splash

Shown at boot while the engine session initializes. Animated logo draw-in
(logo glyphs revealed column-by-column over ~600ms) plus a horizontal
gradient sweep (per-char color from `(x + phase) % ramp` mapped through the
theme's accent ramp). Renders version, tagline, and — if config/theme
validation fell back — a loud warning line (see §8).

```
╭ harbour ───────────────────────────────────────────╮
│                                                    │
│   ███╗   ██╗ █████╗ ██████╗ ██████╗  ██████╗ ██████╗ │
│   ████╗  ██║██╔══██╗██╔══██╗██╔══██╗██╔═══██╗██╔══██╗│
│   ██╔██╗ ██║███████║██████╔╝██████╔╝██║   ██║██████╔╝│
│   ██║╚██╗██║██╔══██║██╔══██╗██╔══██╗██║   ██║██╔══██╗│
│   ██║ ╚████║██║  ██║██║  ██║██████╔╝╚██████╔╝██║  ██║│
│   ╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚═╝  ╚═╝│
│                                                    │
│   curated torrents straight from your terminal      │
│                    v0.1.0                           │
│                                                    │
╰────────────────────────────────────────────────────╯
```

### 2.2 Search

Three regions: sidebar (groups + source-health dots), gradient search bar,
result list. Results stream in as sources answer; each row shows name, size
and seeders colored by theme (size in `syntaxNumber`, seeders green above a
threshold / red near zero). Source tags are **staggered**: a row's tag set
grows as more sources report the same torrent, each new tag animating in
with a ~40ms stagger. `?` help overlays as a centered modal.

```
╭ harbour — search ─────────────────────────────────────────╮
│ ▸ query here…                                       [⏎] │  ← gradient bar, shimmer while streaming
│ ┌ Games ─────┐ ┌────────────────────────────────────────┐ │
│ │ fitgirl  ● │ │ 1 Elden Ring: Shadow of the Erdtree    │ │
│ └────────────┘ │    48.2 GB   ⬆ 12403  ⬇ 82   [fitgirl] │ │
│ ┌ Movies ────┐ │ 2 Interstellar (2014) 1080p REMUX      │ │
│ │ yts      ● │ │    22.1 GB   ⬆ 8912   ⬇ 41 [yts] [tpb] │ │
│ │ tpb      ○ │ │ 3 Dune Part Two 4K DV                  │ │
│ │ 1337x    ● │ │    65.8 GB   ⬆ 5204   ⬇ 63  [x1337-m]  │ │
│ └────────────┘ │ 4 …                                    │ │
│ ┌ TV ────────┐ │                                        │ │
│ │ eztv     ● │ │                                        │ │
│ │ tpb      ○ │ │                                        │ │
│ │ 1337x    ● │ │                                        │ │
│ └────────────┘ │                                        │ │
│ ┌ Anime ─────┐ │                                        │ │
│ │ nyaa     ● │ │                                        │ │
│ │ subsplease●│ │                                        │ │
│ └────────────┘ └────────────────────────────────────────┘ │
│ 10/10 sources answered  ▓▓▓▓▓▓▓▓░░  ⏳ streaming…        │ ← status line: spinner + colorizer + eased bar
╰───────────────────────────────────────────────────────────╯
```

Sidebar semantics: a group's sources are listed under it; the dot is
`success` when healthy, `error`/`dim` when offline. Selecting a group
filters the result list to that group; selecting a source filters to that
source; selecting "all" clears the filter. Offline notifications accumulate
here while a search runs (source down → `offline` tag, search continues).

### 2.3 Downloads

Tabs: **Active** and **Seeding**. Active shows the queue: status per item
(`queued`, `downloading`, `failed`, `missing`), eased animated progress bar,
speed, peers, ETA. Finished items drop into a "Recently downloaded" section
below the queue. Seeding tab shows per-item upload speed, peers, and the `p`
pause/stop action. Failed items show their error message inline.

```
╭ harbour — downloads ──────────────────────────────────────╮
│ ┌ Active ▓┐┌ Seeding ┐                                    │
│ │ Elden Ring …   48.2 GB                                  │
│ │ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░  62%  3.1 MB/s  04:12  ⬆12 │
│ │ Interstellar   22.1 GB                                  │
│ │ ▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░  38%  1.8 MB/s  08:55  ⬆41 │
│ │ … queued: 2 (HARBOUR_MAX_DOWNLOADS=2)                    │
│ │ recently downloaded ──────────────────────────────      │
│ │  Dune Part Two      ✓ done  12:04  today 12:31          │
│ └─────────────────────────────────────────────────────────┘
│ 2 active · 1 seeding · 1 failed   ⬆ 3.1 MB/s total         │ ← status line
╰───────────────────────────────────────────────────────────╯
```

### 2.4 Now playing (phase 6)

Watch mode (`w`, CLI `harbour <magnet>` + watch). libmpv is the renderer — the
TUI shows transport state and exits the alt-screen for the external player;
no custom render engine. Layout: title, playback position/seek bar, volume,
seeding speed. Marked phase 6; details may shift.

```
╭ harbour — now playing ────────────────────────────────────╮
│  ▶ Interstellar (2014) 1080p — watch: dune.p2.mp4         │
│  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░  01:12:44 / 02:48:31  43%      │
│  volume ▓▓▓▓▓▓░░ 62   seed ⬆ 4.2 MB/s   peers 118         │
│  [p] pause   [←→] seek   [+/-] volume   [q] back          │
╰───────────────────────────────────────────────────────────╯
```

---

## 3. Animation spec

One animation loop drives every view; no per-widget timer threads. The loop
owns a `TickSource` abstraction so tests can inject fixed ticks (§9).

**Cadence.** Base tick is 30fps (33.3ms). A `tokio::time::interval` emits
ticks; render requests (input events, engine events, spinner advances) are
**coalesced** — they set a dirty flag / wake the loop, and the loop renders
at most once per tick, always from the latest state. A burst of events within
one frame produces exactly one draw.

**Adaptive backpressure.** The loop measures previous frame cost (time to
build + diff + write). If the cost exceeds the 33.3ms budget, the effective
cadence drops (interval lengthens proportionally) so the UI degrades to a
slower but stable frame rate instead of falling behind and queueing frames.
Frames are never queued — a slow frame simply delays the next tick.

**Sync output.** Every frame write is bracketed by DEC 2026
`ESC[?2026h` (Begin Synchronized Update) / `ESC[?2026l` (End) so the
terminal commits the frame atomically — no partial paints, no flicker. The
BSU/ESU pairs wrap only frame writes; input/stderr output is never
interleaved inside them, and each write is followed by a flush.

**Spinners.** Frame advances every **80ms**. Status-line spinners (the
"streaming…" loader, per-item status glyphs) therefore animate at ~12.5fps;
activity spinners (progress, speed/ETA) animate at the 30fps frame rate —
their value is recomputed every frame from the latest stats. Spinner frame
sets come from the theme's `spinnerFrames` (default `unicode` preset).

**Animated colorizers.** Status-line elements interpolate color over a theme
ramp (e.g. the streaming indicator sweeps through `syntaxKeyword` →
`accent` → `success`). The colorizer phase advances on the 80ms spinner tick,
keeping it calm while the underlying bar/counter tick at 30fps.

**Eased bars.** Progress values never jump: each bar tracks a smoothed
`display` value that eases toward the target:

```
display += (target - display) * (1 - exp(-dt / TAU))   // TAU = 200ms
```

`dt` comes from the tick source (deterministic in fixed-tick tests), clamped
to `[0, 1]`; `display` converges without overshoot. Speed shown is the
smoothed rate; ETA = `remaining_bytes / speed`, recomputed per frame, shown
as `HH:MM` (or `mm:ss` under an hour).

**Splash.** Draw-in reveals logo columns over ~600ms using the same tick
source; the gradient sweep's phase advances per frame (`(x + phase) % ramp`
mapped into the theme accent ramp), so the logo cycles once per ~3s.

---

## 4. Theme spec

The omp theme JSON schema is ported verbatim to Rust (`theme::Schema`):
`name`, `colors` (required tokens), `vars` (recursive refs), `symbols`
(preset `unicode | nerd | ascii`, per-key overrides, `spinnerFrames`), and
optional `export`. Validation errors are loud and fall back to the default
theme with a warning (see §8). The **full schema table with "used by harbour"
annotation per token** is in `docs/theming.md`; this section is the summary.

Default theme: **titanium** (Tokyo Night palette), loaded from an embedded
copy when `~/.harbour/themes/` has no `titanium.json`:

| token | value | token | value |
| --- | --- | --- | --- |
| accent | `#7aa2f7` | syntaxComment | `#565f89` |
| success | `#9ece6a` | syntaxKeyword | `#bb9af7` |
| error | `#f7768e` | syntaxFunction | `#7aa2f7` |
| warning | `#e0af68` | syntaxString | `#9ece6a` |
| muted | `#565f89` | syntaxNumber | `#ff9e64` |
| dim | `240` | syntaxType | `#2ac3de` |
| text | `#c0caf5` | syntaxOperator | `#89ddff` |
| selectedBg | `#2a2f45` | syntaxPunctuation | `#9aa5ce` |
| bg / statusLineBg | `#16161e` | border | `#4c566a` |

**Curated token subset.** harbour's views consume a subset of the omp token
list: core text/borders (`accent`, `border`, `borderAccent`, `borderMuted`,
`success`, `error`, `warning`, `muted`, `dim`, `text`), background blocks
(`selectedBg`, `statusLineBg`), markdown tokens for the `?` help view
(`mdHeading`, `mdCode`, `mdCodeBlock`, `mdQuote`, `mdListBullet`,
`mdLink`/`mdLinkUrl`), and diff+syntax tokens for result rows and status
text (`syntaxComment` … `syntaxPunctuation`, `toolDiffAdded`,
`toolDiffRemoved` for queue-state coloring). omp-app-specific tokens
(thinking states, tool/custom-message backgrounds, message text) are parsed
and validated but not consumed by harbour.

**Custom themes.** `~/.harbour/themes/<name>.json`; the active theme is set in
`config.toml` (`theme = "titanium"`). Theme files are **live-reloaded** on
file change (watched via `notify`); a reload failure falls back to the
previous valid theme with a banner. Color-mode detection: `COLORTERM=truecolor`
or `WT_SESSION` set → truecolor; otherwise 256-color mode with theme colors
quantized through the terminal palette.

---

## 5. Engine integration (librqbit)

Embedded librqbit (Apache-2.0) handles magnet / `.torrent` / infohash input,
metadata, DHT + trackers, stats, seeding, and session-state resume. The engine
runs on the tokio runtime; all engine events cross to the UI over an
**mpsc channel** (`EngineEvent` → UI state → render). UI input becomes
`EngineCommand`s back. Per-source and engine isolation is the rule (§8).

**Session lifecycle.**

1. *Boot*: create `Session` with config (listen ports, DHT, save paths).
   Under bootguard safe mode (§7) the session is created but no torrents
   start — everything is added paused.
2. *Add*: `AddTorrentOptions` from the queue item (mapping below); the item
   enters `queued` and is `promote()`d oldest-first when a slot frees
   (`HARBOUR_MAX_DOWNLOADS`, 0/unset = unlimited).
3. *Run*: events (`MetadataReceived`, `Progress`, `Completed`, `Failed`) →
   mpsc → UI state; stats polled every **500ms**.
4. *Exit*: synchronous — no waiting on engine sockets (OS reclaims them);
   librqbit session state persists for piece-level resume, best-effort.

**`AddTorrentOptions` mapping.**

| option | source |
| --- | --- |
| `paused` | `true` when added at concurrency cap, or in bootguard safe mode; also the seed-pause state on `p` |
| `output_folder` | configured default folder (`o` changes it) or the per-item folder from `Shift+d` |
| `trackers` | config `trackers` override appended to the torrent's own tracker list |

**Stats mapping** (from the 500ms poll): librqbit stats → `TorrentStats`:
`progress` (fraction, from piece stats) drives the eased bar; `speed` (bytes/s,
smoothed) and `peers` (connected/total) drive the active rows; `eta =
remaining_bytes / speed`. Seeding rows show upload speed + peers from the
same poll.

**Seeding.** Completed torrents stay in the session and seed by default;
`p` pauses a seeding item (status `seeding` → idle) or stops seeding for an
active download. The Seeding tab is a filtered view of the same queue state.

**State restore.** On boot: load the `downloads.json` ledger, re-add items to
librqbit, restore session state (piece-level resume). If the previous run
died mid-restore (bootguard, §7), every item restores **paused** — no engines
start until the user resumes.

**Metadata capture.** When `MetadataReceived` fires, the `.torrent` bytes are
saved to `cache/torrents/<info_hash>.torrent`; later re-add/re-seed verifies
on-disk files locally without re-fetching from the swarm.
**Stray-download detector.** A seeding item reporting `speed > 0 && progress
< 1` for 2 consecutive polls after a 10s grace period has missing files →
status `missing`.

---

## 6. Sources & cache design

Full per-source mechanics (selectors, fallback hosts, rate limits,
fixtures) live in `docs/sources.md`. Summary:

- **10 sources** in 4 groups — Games: `fitgirl` (HTML, the only code-running
  category → trusted repacker alone); Movies: `yts` (JSON, multi-host
  fallback), `tpb-movies` (JSON, apibay), `x1337-movies` (HTML), `bittorrented`
  (HTML); TV: `eztv` (RSS), `tpb-tv` (JSON), `x1337-tv` (HTML); Anime: `nyaa`
  (RSS), `subsplease` (RSS).
- `Source` trait: `{ id, label, groups, homepage, reports_health, search(query) -> Vec<TorrentResult> }`.
- Multi-host fallback per source; resilient fetch (retries, per-source
  timeout, abort signal); a dead source reports `offline` in the sidebar and
  never blocks other sources.
- Magnet builder: `magnet:?xt=urn:btih:<lowercase infohash>&dn=<name>`.

**`TorrentResult`** (canonical struct):

```rust
pub struct TorrentResult {
    pub info_hash: String,   // lowercase hex infohash
    pub name: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    pub num_files: Option<u32>,
    pub source: SourceId,    // e.g. SourceId::FitGirl
    pub magnet: String,
    pub added: Option<DateTime<Utc>>,
}
```

Search results dedupe by `info_hash` across sources; a row's source tags are
the deduped source set, rendered staggered (§2.2).

**Cache.** Per `(source, query)` search results are cached with a **5-minute
TTL**, stored as `cache/search/<source>/<query>.json` (query normalized to
lowercase, safe-filename encoded). Expired-but-present entries may be served
with a staleness flag only when the source is offline (fallback path — never
when the source is healthy). Cache writes are atomic (temp file + rename).

---

## 7. Persistence design

Config dir: `~/.harbour/` (Windows: `%USERPROFILE%\.harbour`).

```
~/.harbour/
├── config.toml            # theme, default output folder, trackers, HARBOUR_MAX_DOWNLOADS
├── downloads.json         # queue ledger (normative for restart)
├── history.json           # search history, capped at 500
├── cache/
│   ├── search/<source>/<query>.json   # 5-min TTL result cache
│   ├── torrents/<info_hash>.torrent   # captured metadata (§5)
│   └── covers/            # phase 7 (spike)
└── themes/<name>.json     # custom themes (§4)
```

librqbit's session state (handled by the engine) covers piece-level resume;
`downloads.json` is the app-level ledger: per item `info_hash`, `name`,
`magnet`, output folder, status (`queued | downloading | failed | seeding |
missing`), timestamps, and error messages. `history.json` records queries,
capped at 500 entries (FIFO eviction).

**Bootguard.** A crash marker (`cache/bootguard` — written at boot, removed
on clean exit) guards restores:

1. Marker present at boot → the previous run died mid-restore → **safe mode**:
   load the ledger, re-add every item **paused**; no engines start until the
   user resumes. Marker remains until a clean exit.
2. Marker absent → normal restore: re-add, promote per the concurrency cap,
   resume active items.
3. Clean exit removes the marker *after* ledger + session-state writes
   complete (write → remove marker).

Safe mode is surfaced in the UI (splash banner + a `missing`/paused queue)
rather than being silent.

---

## 8. Error handling

Layered, and failures are never silent:

- **Per-source isolation.** Every source runs independently with its own
  timeout, retry budget, and abort signal. A source that fails mid-search
  reports `offline` in the sidebar; the search continues and the status line
  shows the answered count (e.g. `9/10 sources answered`). One source can
  never block or poison another.
- **Engine errors.** librqbit failures (add, metadata, disk) surface as an
  **error banner** (omp `errorBanner` style, `error` color on the status
  line) plus the item's status flips to `failed` with the error message
  stored in the ledger and shown inline in the Downloads view.
- **Config/theme validation.** Invalid `config.toml` or theme JSON fails
  loudly: the offending file and error are printed, defaults load, and a
  warning banner shows on splash. Live theme reload failures fall back to the
  last valid theme with a banner (§4). No silent defaulting.
- **Panic safety.** A panic hook writes a crash log (`cache/crash.log`),
  restores the terminal unconditionally (alt-screen exit, cursor shown), and
  re-raises. Terminal restoration is wired into `Drop` of the terminal guard,
  so normal exits, `q`, Ctrl-C, and panics all restore the terminal. Exits
  are synchronous: engine sockets are left to the OS, never awaited.

---

## 9. Testing strategy

Tests are part of the acceptance criteria per phase (see `docs/roadmap.md`).

- **Scraper fixture tests.** Each source has fixture HTML/JSON/RSS in
  `tests/fixtures/`; `search()` output is asserted against hand-verified
  `TorrentResult` sets (info_hash, size_bytes, seeders, magnet format).
- **Theme-schema validation tests.** `Schema::from_json` accepts the full omp
  token surface and rejects missing required colors, bad `vars` refs, unknown
  symbol presets; fallback behavior is asserted.
- **Cache TTL tests.** Expiration at exactly 5 minutes, atomic-write
  behavior, offline-stale fallback.
- **ratatui buffer-snapshot tests per view.** Each view renders a fixture
  state into a buffer, compared against a checked-in snapshot (insta-style);
  diffs on any layout/color change.
- **Animation determinism (fixed-tick tests).** The loop takes a `TickSource`;
  tests inject fixed 33.3ms ticks and assert exact eased bar values, spinner
  frame indices at 80ms boundaries, and coalescing (N events in one tick →
  one render).
- **Gated integration.** One real tiny magnet through the whole pipeline
  (engine → mpsc → UI state → resume) gated behind `HARBOUR_TEST_NET=1` so CI
  stays hermetic by default.
- **Live smoke.** Manual smoke against live sources before release: all 10
  sources answer, downloads complete and seed, restart resumes.

---

## Open questions

- `o` (change output folder) semantics for items already in the queue: new
  items only, or re-path queued items too? Defaults to new items; revisit in
  phase 5.
- Whether group/source filtering in the sidebar is cumulative (group +
  source intersect) or exclusive. Defaults to cumulative.
- Now-playing layout (phase 6) may be replaced by a minimal status line if
  libmpv's embedded mode needs the whole screen.
