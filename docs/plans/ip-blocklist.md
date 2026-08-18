# IP blocklist + auto-update
Ref: #61

## Goal
Let harbour load a peer IP blocklist from a local file or a URL, prove how many ranges it
actually loaded, and keep librqbit's existing blocklist enforcement — without inheriting the two
failure modes in librqbit 8.1.1 that would otherwise make this feature either crash the app or
silently protect nothing.

## The findings that shape this whole plan

Read on **2026-08-16** from the exact source harbour compiles against,
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/`.

**`src/session.rs:426`** — the public knob exists:

```rust
pub blocklist_url: Option<String>,
```

**`src/session.rs:623-630`** — how it is loaded. This is the critical finding:

```rust
let blocklist: blocklist::Blocklist = if let Some(blocklist_url) = opts.blocklist_url {
    blocklist::Blocklist::load_from_url(&blocklist_url)
        .await
        .inspect_err(|e| warn!("failed to read blocklist: {e}"))
        .unwrap()          // <-- panics
} else {
    blocklist::Blocklist::empty()
};
```

**A blocklist that fails to load panics `Session::new_with_opts`.** harbour's
`RqbitEngine::new` (`src/engine/rqbit.rs:254-256`) maps the `Err` to
`EngineError::Unavailable` — but a **panic** is not an `Err`, so the graceful "downloads are
unavailable" path in `src/app/mod.rs:371-383` never runs. A typo'd blocklist path would take the
whole app down. This single line is why harbour must validate the blocklist itself before
librqbit ever sees it.

**`src/blocklist.rs:32-52`** — `load_from_url` accepts `file://` URLs and converts them to a
local read, so a local file is reachable through the same one-string knob.

**`src/blocklist.rs:60-84`** — gzip is auto-detected from the `1F 8B` magic bytes. Also:
`fill_buf()` on a body shorter than 2 bytes hits
`anyhow::bail!("Content too short…")` → `Err` → the `.unwrap()` above → **panic**. An empty
blocklist file crashes harbour.

**`src/blocklist.rs:124-153`** — `parse_ip_range`, the entire accepted grammar:

```rust
// comments (#) and blank lines skipped
// split at ':'  →  then split_once('-')  →  both sides must parse as IpAddr
```

That is the **PeerGuardian `.p2p` format** (`name:1.2.3.4-1.2.3.255`) and nothing else.
An eMule `.dat` line (`1.0.0.0 - 1.255.255.255 , 000 , description`) contains no `:`, so
`rfind(':')` returns `None`, `split_point` falls back to `0`, and `split_at(1)` chops the first
character off the start address — the parse always fails. **`.dat` is silently unsupported:
every line is dropped and the user gets an empty blocklist with no error.**

**`src/session.rs:749-751`** (incoming) and **`src/torrent_state/live/mod.rs:576-581`**
(outgoing) — enforcement covers **both** directions. Good news: harbour does not need to build
any filtering, only to load a list correctly.

**`src/lib.rs:45`** — `mod blocklist;` is **private**. `Blocklist`, `load_from_file`, and
`is_blocked` are not public API. harbour cannot reuse librqbit's parser and cannot query the
loaded list; the only surface is the `blocklist_url` string.

**There is no session-level reload.** The list is read once during `Session::new_with_opts` and
stored in an immutable field. **Any blocklist change — including auto-update — takes effect at
the next launch.** This is the same class of setting as `listen_port`, which harbour's settings
already label boot-time (`src/persist.rs:76-77`).

## SPEC / FR reference

**Exists today.** NFR-10 (Security/Privacy: "the only outbound traffic is the user's own
transfers"), NFR-11 (path safety), FR-53/NFR-07 (crash recovery), NFR-15 ("no failure in config,
ledger, theme, engine construction, or persistence aborts startup"). **NFR-15 is the one this
feature would violate by default**, given the panic above.

**Nothing in SPEC mentions peer filtering, blocklists, or any periodic outbound fetch that is
not a search.** Both need specifying.

> **FR numbers here are proposed, not reserved.** Several plans in `docs/plans/` were drafted
> in parallel against the same free block (FR-86+), so numbers collide across files. Allocate
> final numbers when the SPEC edit lands (first merged wins, renumber the rest). The
> requirement *text* is the deliverable; the number is bookkeeping.

**Missing from SPEC — add first, then implement.** Proposed **FR-103 … FR-106** in §4.4, with a
cross-reference from NFR-10:

- **FR-103 (blocklist source).** harbour accepts a blocklist as a local file path or an
  `http(s)` URL, in PeerGuardian `.p2p` text or eMule `.dat` text, plain or gzipped. Off by
  default; an unset blocklist means no filtering and no fetch.
- **FR-104 (validated before use, never silently empty).** harbour parses the blocklist itself,
  reports the number of ranges loaded and the number of lines skipped, and refuses to arm a list
  that yields **zero** ranges. A blocklist that protects nothing must say so — arming one is the
  worst outcome of this feature, because the user believes they are protected.
- **FR-105 (never fatal).** A missing, unreachable, empty, or unparseable blocklist degrades to
  "no blocklist" **with a banner** and the app starts normally (NFR-15). It never panics and
  never blocks startup.
- **FR-106 (auto-update is boot-time and explicit).** When an update URL is configured, harbour
  refreshes the cached list at most once per configured interval, at startup only, with a hard
  deadline and a size cap. A failed refresh keeps the last good cached list and banners the
  failure. The settings row is labelled **Boot-time**, because the engine cannot reload a
  blocklist in a running session.

## Workstream

**Engine & Foundation (Sarthak)** owns effectively all of it: the parser, the cache, the
validation, and the `SessionOptions` wiring all sit in `src/engine/` and `src/core/`.
**Terminal UI (Ishan)** owns two settings rows and the banner copy. **Sources (Dhruv)** — none.

**Shared-type dependency:** none. `Engine`, `EngineEvent`, and `QueueItem` are untouched; this
is entirely inside `EngineLaunchOptions` and `Config`, both of which are already engine-owned.

## Approach

**Step 1 — SPEC (docs only).** FR-103…FR-106 into §4.4 plus the NFR-10 cross-reference. ~30
lines.

**Step 2 — the parser (engine track, pure, no I/O).** `src/engine/blocklist.rs`:

```rust
pub struct BlocklistStats { pub ranges: usize, pub skipped: usize }
/// Parses .p2p and .dat text into normalized `name:start-end` lines.
pub fn normalize(text: &str) -> (String, BlocklistStats)
```

Two accepted input grammars, one normalized output:

- **`.p2p`** — `description:START-END`. Pass through after validating both addresses.
- **`.dat`** — `START - END , level , description`. Split on `-`, trim, validate both addresses,
  re-emit as `description:START-END`. **This is the conversion that makes `.dat` work at all**,
  since librqbit's parser cannot read it.
- Comments (`#`) and blank lines skipped; any line whose two sides do not both parse as `IpAddr`
  is skipped and counted, never guessed at.

Uses only `std::net::IpAddr::from_str`. ~150 lines, pure, table-tested. **This step is the whole
feature's correctness and is reviewable with zero moving parts.**

**Step 3 — load + cache (engine track).** `load_blocklist(config, state_dir) ->
Result<(PathBuf, BlocklistStats), BlocklistError>`:
1. Read the source — local file, or `http(s)` GET with a hard deadline and a size cap
   (reject bodies over, say, 64 MiB before buffering them).
2. Gunzip if the first two bytes are `1F 8B`. **`flate2` is not in the tree; see Key APIs — the
   supported v1 path is to let `reqwest`'s already-enabled `gzip` feature handle
   Content-Encoding, and to require plain text for local files, banner-ing a gzipped local
   file rather than half-supporting it.**
3. `normalize()`.
4. **Refuse if `stats.ranges == 0`** (FR-104) or if the normalized text is under 2 bytes — the
   latter is the exact input that makes librqbit's `create_from_stream` bail and therefore
   panic (`blocklist.rs:68-73`).
5. Write the normalized text to `<state>/blocklist.p2p` via the existing atomic-write helper
   (FR-55), so a killed harbour never leaves a half-written list that the next launch arms.

**Step 4 — wire it (engine track, small).** `EngineLaunchOptions` gains
`blocklist_url: Option<String>`; `RqbitEngine::new` sets `SessionOptions.blocklist_url`.
**Only ever pass the `file://` URL of the validated cache from step 3 — never the user's raw
path or URL.** That is what neutralizes the `.unwrap()` panic: by the time librqbit reads the
file, harbour has already proven it parses, is non-empty, and is over 2 bytes. Build the URL
with `url::Url::from_file_path` (`url` is already in the tree via librqbit) rather than string
concatenation — Windows drive letters and spaces both break naive `format!("file://{path}")`.

**Step 5 — auto-update (engine track).** `Config.blocklist_update_url`,
`blocklist_update_hours: u64` (0 = never), and `blocklist_updated_at: i64` (unix seconds).
At startup, if the interval has elapsed, fetch → validate → replace the cache; on any failure
keep the previous cache and banner. Runs **before** engine construction so the fresh list is the
one armed this session. ~120 lines.

**Step 6 — settings + banner (UI track).** Three rows via the shared
`row_kind`/`row_label`/`text_field` layout in `src/ui/settings.rs`:
"IP Blocklist File/URL (boot-time)", "Blocklist Auto-Update URL (boot-time)",
"Blocklist Update Interval (hours, 0 = off)". Bump `APP_ROWS` and update the index tables in
`row_kind`, `text_field`, `row_label`, plus the tests at `src/ui/settings.rs:434-459`. The
startup banner reports the outcome — `"blocklist: 289,412 ranges loaded (17 lines skipped)"` or
the reason it is off — folded into the existing `startup_warnings` join at
`src/app/mod.rs:459-465`.

## Files to create / modify

**Create**
- `src/engine/blocklist.rs` — `normalize`, `BlocklistStats`, `BlocklistError`, `load_blocklist`.

**Modify**
- `SPEC.md` — FR-103…FR-106 in §4.4; NFR-10 cross-reference.
- `src/engine/mod.rs` — `pub mod blocklist;`.
- `src/core/paths.rs` — `blocklist_cache_file(root)` next to `ledger_file`.
- `src/persist.rs` — `Config.blocklist_path`, `blocklist_update_url`, `blocklist_update_hours`,
  `blocklist_updated_at`; all `#[serde(default)]`.
- `src/engine/rqbit.rs` — `EngineLaunchOptions.blocklist_url`;
  `SessionOptions.blocklist_url`; a `//!` note recording the `session.rs:623` `.unwrap()` and
  the `.p2p`-only grammar, with file and line, so the next librqbit bump sees why the
  pre-validation exists. This is precisely the "invariants get a comment at the decision site"
  rule.
- `src/app/mod.rs` — call `load_blocklist` before `RqbitEngine::new`; fold the outcome into
  `startup_warnings`.
- `src/ui/settings.rs`, `src/app/settings.rs` — the three rows and their commit arms.

## Key APIs / libraries

- **`librqbit::SessionOptions.blocklist_url: Option<String>`** — verified present in the
  vendored 8.1.1 source at `src/session.rs:426` (read 2026-08-16). Accepts `file://`
  (`blocklist.rs:35-41`). Enforced on incoming (`session.rs:749`) and outgoing
  (`torrent_state/live/mod.rs:576-581`) connections.
- **`librqbit::blocklist::Blocklist` is NOT public** (`src/lib.rs:45` is `mod`, not `pub mod`),
  so harbour cannot reuse the parser or query `is_blocked`. Hence step 2's own parser. Worth an
  upstream issue asking for `pub mod blocklist` plus a non-panicking load — link it from FR-105
  the way `docs/plans/sequential-download.md` links its upstream ask.
- **`std::net::IpAddr::from_str`** — the entire validation surface. No CIDR support is needed:
  both blocklist formats are start–end ranges.
- **`reqwest`** — already a dependency, already built with the `gzip` feature
  (`Cargo.toml:15`), which transparently handles a `Content-Encoding: gzip` response.
- **Blocklist formats**, checked 2026-08-16:
  [PeerGuardian `.dat` format notes](https://sourceforge.net/p/peerguardian/wiki/dev-blocklist-format-dat/)
  and [qBittorrent issue #5281 on .dat vs .p2p](https://github.com/qbittorrent/qBittorrent/issues/5281),
  which is the same "the two formats differ by `:` vs `-`" distinction found in librqbit's
  parser. **The `.dat` access-level field (the `, 000 ,` column) is treated by eMule-family
  clients as a permissiveness level; harbour v1 blocks every listed range and ignores the
  level.** If review wants the level honored, confirm the exact threshold against
  qBittorrent's source first and put it in FR-103 — do not guess a number.

**New crates: none.** A gzipped *local* `.dat`/`.p2p` file would need `flate2`; v1 rejects it
with a clear banner ("gunzip the file, or use a URL") instead of adding a crate for a case the
URL path already covers via reqwest. If the banner proves annoying in practice, `flate2` is a
small, well-audited crate and adding it is a one-line follow-up — but it must be *justified by
use*, not added speculatively.

## Risks / edge cases

- **The panic is the headline risk and step 3/4 is the entire mitigation.** Never hand librqbit
  a path the user typed. Never hand it a file harbour has not just parsed. Never hand it a file
  under 2 bytes or with zero ranges. A regression test that constructs an engine with a
  deliberately broken blocklist config and asserts the app still starts is mandatory.
- **A silently empty blocklist is worse than no blocklist.** A `.dat` file passed straight
  through parses to zero ranges and the user believes they are filtered. FR-104's refusal +
  the loaded-count banner is the fix, and the count is the only thing that makes it *verifiable*.
- **Expect `flate2` to be needed, not merely possible.** The de facto provider (I-BlockList)
  serves gzipped *file bodies* (`…list.gz`, no `Content-Encoding: gzip` header), so reqwest's
  transparent decompression does not apply and the body arrives as gzip bytes. Those parse to
  zero ranges and hit FR-104's refusal — the plan degrades loudly rather than silently, which is
  correct, but the first real auto-update URL a user pastes will land there. Treat the `flate2`
  follow-up as expected work, and either ship it with step 5 or make the banner say
  "this looks gzipped — gunzip it, or wait for gzip support" rather than a generic parse error.
- **Large lists.** A full I-BlockList run is millions of ranges. Cap the download size, and note
  that librqbit builds an `IntervalTree` over the whole list at session start — memory and
  startup cost scale with it, which touches NFR-03 (≤100ms p95 to interactive) and NFR-13
  (footprint). Measure with a real list before claiming the feature is free; if the startup cost
  is material, load the blocklist *after* the splash rather than before it.
- **No live reload — say so in the UI.** Anything else is a control that appears to do something
  it cannot. The `listen_port` "Boot-time" label is the existing precedent to copy.
- **Auto-update is outbound traffic to a third party.** NFR-10 says the only outbound traffic is
  the user's own transfers; a blocklist fetch is a new exception and belongs in the FR-106 text
  explicitly, not as an unstated side effect.
- **A blocklist can block your own peers.** Overly broad lists kill swarm health. Not harbour's
  problem to solve, but the loaded-range count in the banner is what lets a user correlate "I
  turned this on and everything stalled" with the cause.
- **`file://` URL construction on Windows.** `format!("file://{}", path.display())` produces
  `file://C:\Users\…`, which `Url::parse` will not resolve back to a path. Use
  `Url::from_file_path`, and unit-test it on a path with a space in it.

## Test strategy

- **Unit, `src/engine/blocklist.rs`** — table tests over `normalize`:
  a `.p2p` line passes through; a `.dat` line converts; a `.dat` line with no level converts;
  comment and blank lines are skipped without counting as errors; a line with a malformed
  address is skipped and **counted**; an IPv6 `.p2p` line survives; a mixed-version range
  (`1.2.3.4-::1`) is skipped; an all-garbage input yields `ranges == 0` (which the caller must
  refuse).
- **Unit, `load_blocklist`** — a temp file with 3 ranges yields `ranges == 3` and writes the
  cache; an empty file returns `Err` and writes **no** cache; a missing file returns `Err`; a
  file of zero valid lines returns `Err`. Every one of these is a case that would have panicked
  librqbit.
- **Unit, file URL** — `Url::from_file_path` on a path containing a space round-trips through
  `to_file_path` on all three platforms (`cfg`-gated assertions).
- **Integration, `tests/engine_net.rs` (no network needed, but it is where engine construction
  tests live)** — construct `RqbitEngine` with `blocklist_path` pointing at a nonexistent file
  and assert the engine still constructs and the app-level warning is produced. **This is the
  regression test for the panic** and is the single most important test in this plan.
- **Buffer snapshot, `src/ui/tests.rs`** — the three settings rows render with the "boot-time"
  marker; the startup banner shows the loaded-range count.

## Verification

1. `SPEC.md` §4.4 contains FR-103…FR-106, and `src/engine/rqbit.rs`'s module docs cite
   `librqbit-8.1.1/src/session.rs:623` for the panic and `src/blocklist.rs:124` for the grammar.
2. Download a real PeerGuardian list (e.g. an I-BlockList `.p2p`), set it in settings, restart.
   **The startup banner reads `blocklist: N ranges loaded`, with N in the hundreds of
   thousands.** That number is the proof — an armed-but-empty list is the failure this feature
   exists to prevent.
3. Convert the same list to eMule `.dat` and repeat. The loaded count is comparable. Confirm
   `<state>/blocklist.p2p` now contains `name:start-end` lines — the conversion that librqbit
   could not do for itself.
4. Point the setting at `/definitely/not/here.p2p` and restart. **harbour starts normally** with
   `blocklist: could not load … — no blocklist is active`. It does not panic, and downloads
   still work. Before this plan, that input crashed the app.
5. Point it at an empty file. Same graceful outcome, different message — never an armed empty
   list.
6. Set an auto-update URL and a 24-hour interval, restart twice within the hour. The second
   start does **not** re-fetch (the interval is honored) and the cached list is still armed.
7. Add a torrent and confirm downloads still connect to peers — a blocklist that blocks
   everything is a possible outcome and the loaded-range count plus a live peer count is how a
   user tells the two apart.
