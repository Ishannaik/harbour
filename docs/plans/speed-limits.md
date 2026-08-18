# Global + per-torrent speed limits
Ref: #43

## Goal
Write the already-shipped global/alternate rate limits into SPEC, expose a **per-torrent** cap
at the only point librqbit 8.1.1 permits one (add time), and leave a clean seam for the speed
scheduler — without shipping a slider that silently restarts a torrent.

## The finding that shapes this plan

Read on **2026-08-16** from the exact source harbour compiles against,
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/`.

**`src/limits.rs:9-13`** — the config shape, fully public:

```rust
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    pub upload_bps: Option<NonZeroU32>,
    pub download_bps: Option<NonZeroU32>,
}
```

**`src/limits.rs:48-67`** — `Limits` wraps two `ArcSwapOption<RateLimiter>` and exposes
`set_upload_bps` / `set_download_bps`. Swapping is atomic, so a change applies to in-flight
transfers with no restart.

**`src/session.rs:130`** — `pub ratelimits: Limits` on `Session`. **Public.** This is the
global limiter harbour already drives from `RqbitEngine::set_speed_limits`.

**`src/session.rs:265`** — `pub ratelimits: LimitsConfig` on `AddTorrentOptions`. **Public.**
This is the per-torrent limit, supplied once, at add time.

**`src/torrent_state/live/mod.rs:211`** — `ratelimits: Limits` on `TorrentStateLive`. **No
`pub`.** Private field.

**`src/torrent_state/live/mod.rs:253`** — `let ratelimits = Limits::new(paused.shared.options.ratelimits);`
The per-torrent limiter is constructed from the shared options each time the torrent goes live.

**`src/torrent_state/mod.rs:194`** — `pub(crate) options: ManagedTorrentOptions` on
`ManagedTorrentShared`, and `ManagedTorrentOptions` (`src/torrent_state/mod.rs:111-122`) is
itself `pub(crate)`.

**`src/http_api/handlers/configure.rs:9-23`** — the only ratelimit endpoint librqbit ships is
`h_update_session_ratelimits`, and it touches `session().ratelimits` only. There is **no**
per-torrent equivalent, in the HTTP API or in `src/api.rs` (`grep -n "limit" src/api.rs`
returns nothing).

**Therefore, in librqbit 8.1.1:**

1. **Global limits are live and instant.** Already built in harbour, already correct.
2. **A per-torrent limit can be set exactly once — in `AddTorrentOptions` at add time.**
3. **A per-torrent limit cannot be read or changed afterwards from outside the crate.**
   `ManagedTorrent::live()` is public and returns `Arc<TorrentStateLive>`, but the
   `ratelimits` field on it is private, and the `shared.options` path is `pub(crate)`.
4. A pause/unpause cycle *would* rebuild the limiter from `shared.options.ratelimits` — but
   those options are immutable from outside, so the cycle re-applies the same value.

librqbit 8.1.1 is the current release ([crates.io/crates/librqbit](https://crates.io/crates/librqbit),
[docs.rs/crate/librqbit/latest](https://docs.rs/crate/librqbit/latest), both checked
2026-08-16), so this is not a "wait for the next version" situation.

## What already exists in harbour (do not rebuild it)

`src/persist.rs:61-72` already persists all five global knobs:
`download_limit_mib`, `upload_limit_mib`, `alt_download_limit_mib`, `alt_upload_limit_mib`,
`use_alt_rates`. `src/app/settings.rs`'s `apply_rate_limits` already picks the effective pair
and calls `Engine::set_speed_limits` (`src/core/types.rs:694`), implemented in
`src/engine/rqbit.rs:554-563` against `session.ratelimits`. Settings rows 5–9 already edit
them.

**This issue is therefore SPEC-first plus one genuinely new capability (per-torrent), not a
greenfield feature.** Saying otherwise would produce a plan that reimplements working code.

## SPEC / FR reference

**Nothing in SPEC.md covers speed limits at all.** `grep -n "FR-" SPEC.md` tops out at FR-68;
no FR mentions a rate limit, an alternate rate, or a scheduler. The code shipped ahead of the
spec. Per AGENTS rule 2, SPEC is the referee — **add these first, then implement**.

FR numbers **FR-86 … FR-89** (FR-69…FR-85 are claimed by the existing plans in `docs/plans/`;
verified with `grep -oh "FR-[0-9]\+" docs/plans/*.md`). Add to §4.4.

> **FR numbers here are provisional.** Many plans were drafted in parallel on 2026-08-16 and
> their ranges collide — `debrid-support.md` and `plugin-multi-engine-search.md` both also
> claim FR-86+. Plan files are not the allocator: **SPEC.md is**, and every plan's step 1 is a
> SPEC edit, so numbers resolve deterministically in SPEC-PR merge order. Renumber at that
> point if another block lands first; the block *shape* (4 consecutive FRs) is what matters.

- **FR-86 (global limits).** harbour applies a global download and upload cap, each
  independently *unlimited* when unset. Changes apply to in-flight transfers immediately, with
  no restart, and persist in `config.toml`.
- **FR-87 (alternate limits).** A second pair of caps ("alternate rates") can be toggled on and
  off; while on, they replace the normal pair. The toggle is the single seam the speed
  scheduler (separate issue) drives — the scheduler flips this flag and nothing else.
- **FR-88 (per-torrent limits).** A torrent may carry its own download/upload cap, applied when
  it is handed to the engine. It is set **before the torrent starts** and is fixed for that
  run; changing it takes effect the next time the item starts. The UI states this rather than
  implying a live slider.
- **FR-89 (unit and floor).** Limits are expressed in KiB/s. `0`/empty means unlimited. A cap
  below 1 KiB/s is rejected rather than silently rounded to unlimited.

## Workstream

- **Step 1 (settings-row table)** — **Terminal UI (Ishan)**. This is the shared prerequisite;
  see below.
- **Steps 2, 4** — **Engine & Foundation (Sarthak)**: `AddRequest`, `QueueItem`, `LimitsConfig`
  mapping are shared/frozen types.
- **Steps 3, 5** — **Terminal UI (Ishan)**, building against the frozen types.

Shared-type dependencies: `AddRequest` and `QueueItem` both change. Both are Sarthak's. The UI
track must not define its own limit struct.

### Step 1 is a prerequisite for issues #44, #45, #46 and #47

`src/ui/settings.rs:98-124` maps rows by **hardcoded integer index**:

```rust
pub fn row_kind(index: usize) -> Option<RowKind> {
    match index {
        0 | 2 | 4 | 5 | 6 | 7 | 8 | 10 | 11 | 14 | 16 => Some(RowKind::Text),
        1 => Some(RowKind::Theme),
        3 | 9 | 12 | 13 | 15 => Some(RowKind::Toggle),
        ...
```

…and `src/app/settings.rs`'s `settings_toggle_row` dispatches on the same bare integers
(`3 =>`, `9 =>`, `12 =>`, `15 =>`). `APP_ROWS: usize = 17` is a third copy of the same fact.
All five issues in this batch add settings rows. If each renumbers this match independently the
merges will conflict and, worse, will conflict *silently* — a mis-renumber compiles fine and
toggles the wrong setting.

**Step 1 replaces the index match with one declarative table** (a `const ROWS: &[Row]` of
`{ kind, label, field }`, with `row_kind`/`row_label`/`text_field`/`row_count` derived from
it). It is a pure refactor: no behaviour change, no new row, its own PR, and the existing
`row_kind` tests in `src/ui/settings.rs:434-458` are the regression net. **It lands once, here,
and the other four plans depend on it.** Do not repeat this refactor in the other four plans.

## Approach

**Step 1 — settings rows become a table (UI, pure refactor, ~150 lines).**
As above. Ships alone, proves itself against the existing row tests, unblocks four issues.

**Step 2 — SPEC FR-86…FR-89 (docs only).**
Writes down what already ships plus the per-torrent contract. Independently reviewable.

**Step 3 — units move from MiB/s to KiB/s (UI + engine, small).**
Today `Config::download_limit_mib` is a `u64` of **MiB/s** and `mib_to_bps`
(`src/engine/rqbit.rs:569-574`) multiplies by `1024*1024`. Integer MiB/s means **the smallest
expressible cap is 1 MiB/s ≈ 8 Mbps** — useless as an upload cap on a domestic line, which is
the single most common reason to want one. qBittorrent uses KiB/s.
Rename the four fields to `*_limit_kib`, add `#[serde(default)]` and a migration that reads a
legacy `*_limit_mib` key and multiplies by 1024 so existing `config.toml` files keep working
(a silently-dropped key would turn a user's 5 MiB/s cap into unlimited — the exact silent
fallback the project rules forbid). `mib_to_bps` becomes `kib_to_bps`.

**Step 4 — per-torrent limits reach the engine (engine).**
- `AddRequest` and `AddBytesRequest` gain `limits: Option<SpeedLimits>` where
  `SpeedLimits { download_kib: Option<u32>, upload_kib: Option<u32> }` is a new
  `core::types` struct — harbour's own, so `librqbit::LimitsConfig` stays confined to
  `src/engine/rqbit.rs` exactly like every other librqbit type.
- `RqbitEngine::add` / `add_bytes` map it into `AddTorrentOptions { ratelimits, .. }`.
- `QueueItem` gains `#[serde(default, skip_serializing_if = "Option::is_none")] limits:
  Option<SpeedLimits>`, following the `bytes` field precedent at `src/core/types.rs:406`.
- `Queue::add_item_to_engine` (`src/queue.rs:286`) passes `item.limits` through.
- `FakeEngine` records the limits it was handed, so queue tests can assert they arrive.

**Step 5 — the UI sets it, honestly (UI).**
A per-torrent limit row in the downloads view (`L` on the selected item) that edits
`QueueItem.limits`. Because of the finding above, the label reads
`per-torrent limit (applies on next start)` and:
- Item is **queued** → the value is used when it starts. No caveat needed.
- Item is **running** → the value is stored and the UI says *"applies on next start"*.
  It does **not** silently `remove` + `add` the torrent.

**Step 6 — the scheduler seam (no code).**
`use_alt_rates` + `apply_rate_limits` **is** the whole integration surface: a scheduler sets
that bool on a clock and calls `apply_rate_limits`. This plan does not build a scheduler; it
records the seam in FR-87 so the scheduler issue has something to build against.

**The scheduler is planned separately in `docs/plans/speed-scheduler-notifications.md`** (also
written 2026-08-16), and it drives **exactly this seam** — its own words: *"adds no engine code
at all — it decides when to flip `use_alt_rates` and calls the existing `apply_rate_limits`"*.
The two plans agree; no reconciliation is needed. One coupling to honour: **step 3 of this plan
renames the four limit fields from `*_limit_mib` to `*_limit_kib`**, and that plan quotes the
`_mib` names. Whichever lands second updates the field names — a mechanical rename, but it must
not be missed.

**Step 7 — upstream the runtime knob (out-of-repo).**
File an issue on `ikatson/rqbit`: expose per-torrent rate limits at runtime, either
`pub fn ratelimits(&self) -> &Limits` on `TorrentStateLive` or a
`Session::update_torrent_ratelimits(&handle, LimitsConfig)` mirroring the existing
`Session::update_only_files` (`src/session.rs:1406`), which is precisely this shape and already
public. Link the issue number from FR-88. Only once it lands does the live slider exist.

## Files to create / modify

- `SPEC.md` — FR-86…FR-89 in §4.4.
- `src/ui/settings.rs` — **step 1**: the `ROWS` table replacing the three index matches; then
  the new rows' labels/kinds.
- `src/app/settings.rs` — **step 1**: `settings_toggle_row` dispatches on the row's identity,
  not its integer; `apply_rate_limits` unchanged in shape, KiB in step 3.
- `src/persist.rs` — `*_limit_kib` fields + the MiB→KiB migration.
- `src/core/types.rs` — `SpeedLimits`; `limits` on `AddRequest`, `AddBytesRequest`, `QueueItem`.
- `src/engine/rqbit.rs` — `kib_to_bps`; `ratelimits` into both `AddTorrentOptions`; the
  module `//!` block gains the per-torrent-is-add-time-only invariant with the exact upstream
  file:line, so the next person to bump librqbit sees it.
- `src/engine/fake.rs` — record received limits for the queue tests.
- `src/queue.rs` — pass `item.limits` through `add_item_to_engine`.
- `src/ui/downloads.rs`, `src/input.rs`, `src/app/actions.rs` — the `L` row and its dispatch.
- `docs/plans/speed-limits.md` — this file; update when the upstream issue moves.

## Key APIs / libraries

All verified by reading librqbit 8.1.1's vendored source (file:line above), plus
[crates.io/crates/librqbit](https://crates.io/crates/librqbit) and
[docs.rs/crate/librqbit/latest](https://docs.rs/crate/librqbit/latest) to confirm 8.1.1 is
current (2026-08-16). ratatui 0.30.2 is current
([github.com/ratatui/ratatui/releases](https://github.com/ratatui/ratatui/releases), checked
2026-08-16) and matches `Cargo.toml`; no widget work here needs a newer one.

**New crates: none.** The `NonZeroU32` conversion and the KiB migration are both a handful of
lines of arithmetic; a units crate for this would be exactly the dependency AGENTS rule 8
exists to refuse.

## Risks / edge cases

- **Rejected approach: a "live" per-torrent slider implemented as remove + re-add.** It reads
  as instant and is not: `Session::delete` then `add_torrent` re-runs initialization, which
  re-checks every piece on disk (`src/torrent_state/initializing.rs`). A user nudging a cap
  would trigger a full recheck of a 60 GB torrent and see progress appear to stall. It is also
  a silent fallback dressed as a feature. Rejected in writing so it is not re-proposed.
- **Rejected approach: shipping the slider anyway and only applying at add time, unlabelled.**
  A control whose effect is invisible until the next restart, with no label saying so, is a lie
  in the interface. Step 5's caveat text is not optional.
- **The MiB→KiB migration is the one place data can be lost.** A config with
  `download_limit_mib = 5` must become `5120` KiB/s, not `None`. Test both directions; a
  missing key means unlimited, a *legacy* key means multiply.
- **`NonZeroU32` clamps.** `kib_to_bps` must saturate at `u32::MAX` (~4 GiB/s) rather than
  wrap, and must map `Some(0)` to an explicit rejection at the parse site, not to `None`.
  `Some(0)` silently becoming "unlimited" is the opposite of what a user typing `0` into an
  upload cap expects. `parse_opt_number` (`src/app/settings.rs`) already refuses to guess on
  bad input — extend that behaviour, do not weaken it.
- **Global and per-torrent limiters compose multiplicatively, not additively.** Both gates are
  acquired independently (`Limits::prepare_for_download`), so a torrent capped at 2 MiB/s under
  a global 1 MiB/s gets 1 MiB/s. Expected, but worth one comment at the decision site so it is
  not filed as a bug.
- **Alt rates and per-torrent limits are unrelated axes.** Toggling alt rates must not clear a
  per-torrent value. Covered by a queue test.

## Test strategy

- **Unit, `src/ui/settings.rs`** — the step-1 table: `row_count()` matches the table length,
  every index yields a kind, `text_field` agrees with `row_kind` for every row. The existing
  tests at lines 434-458 must pass unchanged through the refactor; that is the point.
- **Unit, `src/persist.rs`** — MiB→KiB migration: a legacy config with `download_limit_mib = 5`
  loads as `5120` KiB; a config with neither key loads as `None`; a new-format config round
  trips.
- **Unit, `src/engine/rqbit.rs`** — `kib_to_bps`: `None → None`, `Some(1) → 1024`,
  `Some(0) → None` *at the boundary only*, saturation above `u32::MAX` does not wrap.
- **Unit, `src/queue.rs`** against `FakeEngine` — an item with `limits` set hands exactly those
  limits to the engine on start; an item without hands `None`; toggling alt rates leaves
  per-item limits untouched.
- **Buffer snapshot, `src/ui/tests.rs`** — the per-torrent row renders `unlimited` when unset,
  and renders the "applies on next start" caveat for a *running* item and not for a queued one.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — add a real magnet with a
  1024 KiB/s cap, poll `snapshot()` for ~20s, assert observed `speed_mib` stays under a
  generous ceiling (say 2 MiB/s). Loose bounds on purpose: this proves the limiter is wired,
  not that governor is accurate.

## Verification

1. `SPEC.md` §4.4 contains FR-86…FR-89, and `src/engine/rqbit.rs`'s module docs cite
   `librqbit-8.1.1/src/torrent_state/live/mod.rs:211` for the private-field invariant.
2. `cargo run` → settings → set global download limit to `512` KiB/s while a torrent is
   running. **The downloads view's speed column drops to ~0.5 MiB/s within a few seconds, with
   no restart.** That is the user-visible proof, and it is only expressible because step 3
   moved the unit to KiB.
3. Toggle alternate rates in settings: the effective cap switches to the alt pair immediately
   and switches back on toggle-off.
4. Add a torrent with a per-torrent cap set at add time, then confirm via the downloads view
   that it never exceeds it while the global cap is unlimited.
5. Edit a *running* torrent's per-torrent cap: the UI shows "applies on next start", the speed
   does **not** change, and `grep -n "remove" src/app/actions.rs` shows no remove/re-add on the
   limit path.
6. An upstream issue exists on `ikatson/rqbit` for runtime per-torrent limits, linked from
   FR-88.
