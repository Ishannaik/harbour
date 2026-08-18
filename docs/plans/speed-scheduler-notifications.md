# Speed scheduler + in-terminal notifications
Ref: #66

## Goal
Bind harbour's existing alternate rate limits to a time-of-day window, and give the TUI a
transient toast channel for completions and other passing news — no OS tray, no OS notifications.

## The findings that shape this plan

Read on **2026-08-16** from the harbour source and `Cargo.lock`.

**1. The rate-limit plumbing already exists end to end. The scheduler is a `bool` writer.**

```rust
// src/app/settings.rs:62 — the one place limits are chosen and applied
fn apply_rate_limits(app: &mut App) {
    let (down, up) = if app.config.use_alt_rates {
        (app.config.alt_download_limit_mib, app.config.alt_upload_limit_mib)
    } else {
        (app.config.download_limit_mib, app.config.upload_limit_mib)
    };
    app.queue.engine().set_speed_limits(down, up);
}
```

`Config` already carries `download_limit_mib`, `upload_limit_mib`, `alt_download_limit_mib`,
`alt_upload_limit_mib`, `use_alt_rates` (`src/persist.rs:58-72`), the settings overlay already
edits all five (rows 5–9), and `run()` applies them at boot (`app/mod.rs:406`). **The scheduler
adds no engine code at all** — it decides when to flip `use_alt_rates` and calls the existing
`apply_rate_limits`. That is the single most important scoping fact in this issue.

**2. Local time-of-day costs zero new crates — but only via chrono.**

`Cargo.lock` on 2026-08-16:

- **`chrono 0.4.45`** (line 434), pulled by `librqbit-dht`, with dependencies
  `iana-time-zone`, `js-sys`, `num-traits`, `serde`, `wasm-bindgen`, `windows-link`. The
  presence of `iana-time-zone` and `windows-link` means the **`clock` feature is already enabled**
  in the unified build — i.e. `Local::now()` is already compiled into the binary today.
- **`time 0.3.55`** (line 3791) is also in-tree — and is the wrong choice. Since 0.3.5 the crate
  **refuses to determine the local offset in a multithreaded process**, returning
  `IndeterminateOffset`; the alternative is `local_offset::set_soundness(Unsound)`, which is
  documented as enabling undefined behaviour (time-rs `UtcOffset` docs and
  [time-rs/time#380](https://github.com/time-rs/time/issues/380); the history is
  [RUSTSEC-2020-0071](https://osv.dev/vulnerability/RUSTSEC-2020-0071) — all checked 2026-08-16).
  harbour is emphatically multithreaded: `tokio` with `features = ["full"]`, a dedicated input
  thread (`app/mod.rs:308`), and the theme-watcher thread. `time` would fail at runtime on Linux.
- **`jiff 0.2.35`** (line 1624, via `serde_with`) is the modern correct-by-construction option
  and would also work, but it is a *transitive* of a transitive; depending on it directly ties
  harbour to `serde_with`'s resolution. chrono is one hop from `librqbit`, which harbour already
  pins deliberately.

**Decision: `chrono`, declared explicitly.** Feature unification is a build-graph coincidence,
not a contract — if `librqbit-dht` ever drops chrono the build must still be correct. So:

```toml
chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }
```

Zero new crates *today*, and correct if the graph changes tomorrow. Record the lockfile evidence
in the PR description per the lean-dependency rule.

**3. Toasts collide with UR-13 head-on.**

> **UR-13** Error banner … is the **single channel** for engine/config errors; banners are
> dismissible and never overlap the active input.

The issue asks for toasts covering "completion, errors". Building a second error channel
contradicts the referee document. This is resolved by SPEC amendment, not by ignoring it.

**4. Completion already arrives as an event.** `EngineEvent::Done { id }`
(`src/core/types.rs:739`) flows into `apply_event` in `src/app/actions.rs` — the hook exists;
no polling for "is it finished yet" is needed.

## SPEC / FR reference

**Exists today.** `FR-51` (config), `UR-13` (the error banner), `NFR-04` (idle CPU ≤ 2% of one
core — the scheduler tick must respect it), `UR-11` (every async op shows state).

**Sibling plan — read it first.** `docs/plans/speed-limits.md` (#43) covers the global and
alternate rate limits themselves and **explicitly reserves the seam this issue plugs into**
(its "Step 6 — the scheduler seam (no code)": *"a scheduler sets that bool on a clock and calls
`apply_rate_limits`"*). #43 should land first; #66 then adds only the clock. If #43 has already
written the alternate-limit FR into SPEC, #66 references it instead of restating it.

**Missing from SPEC — add first, then implement.** Proposed **FR-82 / FR-83** in §4.4 and
**UR-20** in §5, plus an amendment to UR-13.

> **FR numbers here are provisional.** Several plans in `docs/plans/` independently propose
> FR-8x/FR-9x; allocate the real numbers against `SPEC.md` at the moment the SPEC commit is
> written, in merge order. The **UR** numbers (UR-14…UR-20, across #63–#66) are internally
> consistent and non-colliding — keep them as written.

- **FR-82 (alternate speed schedule).** harbour can bind the alternate rate limits to a
  daily time window in the machine's local timezone. The window is inclusive of its start minute
  and exclusive of its end minute, and may wrap past midnight (`22:00`–`07:00`). A window whose
  start equals its end is never active. The schedule only ever writes the existing
  `use_alt_rates` switch; there is no second limit path.
- **FR-83 (manual override).** Toggling "Use Alternative Rates" by hand while a schedule is
  enabled overrides the schedule until the next window edge, then the schedule resumes. The
  override is in-memory and is not persisted — a restart is governed by the schedule alone.
- **UR-20 (toasts).** Transient, self-expiring notices stack above the status bar, newest at
  the bottom, at most three, each ~4s. They never take keyboard focus, never block, and are
  never the only place an error is reported.
- **UR-13 (amended).** …is the single channel for errors that need acknowledgement. Toasts
  (UR-20) carry transient success and informational notices only; anything a user must act on
  goes to the banner.

FR-83 exists because the ambiguity ships as a bug otherwise: without it, the scheduler re-flips
the user's manual toggle on the next tick and the setting looks broken.

## Workstream

Split, and the split matters:

- **Engine & Foundation (Sarthak)** — the `Config` fields (`persist.rs` is foundation code with
  the atomic-write and quarantine invariants), the pure schedule evaluator, and its wiring into
  the app loop's tick. Config is a shared contract; a field added carelessly breaks forward
  compatibility with older builds.
- **Terminal UI (Ishan)** — the three settings rows, the toast widget and its state, and the
  toast emission sites.

Shared-type dependencies: **`EngineEvent::Done`** (read-only, already exists). No shared type
changes. `Config` gains fields under the existing `#[serde(default)]` so older config files keep
loading — that is the additive rule already used for `disabled_sources` (`persist.rs:52`).

**Ordering:** independent of #63/#64/#65 for the scheduler; the three settings rows should land
**after #63** so they are authored as `SettingDef` table entries in the `Speed` category rather
than as three more hardcoded indices that #63 then has to migrate.

## Approach

**Step 1 — SPEC (docs only).** FR-82, FR-83, UR-20, and the UR-13 amendment.

**Step 2 — config + the pure evaluator (Engine, ~120 lines).**

`Config` gains three fields, all `#[serde(default)]`-covered:

```rust
/// Bind the alternate rate limits to a daily window (FR-82). Off by default:
/// a scheduler that starts throttling an existing install after an upgrade
/// would be an unrequested behaviour change.
pub alt_schedule_enabled: bool,
/// Window start / end as "HH:MM", local time. Strings rather than minutes
/// because config.toml is a file people edit by hand, and 1320 is not a time.
pub alt_schedule_from: String,   // default "22:00"
pub alt_schedule_to:   String,   // default "07:00"
```

New `src/core/schedule.rs`, entirely pure and entirely testable without a clock:

```rust
/// Minutes since local midnight, or None if `text` is not "HH:MM".
/// Returns None rather than defaulting — a mistyped window that silently
/// becomes 00:00 is exactly the silent fallback the project forbids.
pub fn parse_hhmm(text: &str) -> Option<u16>;

/// Is `now` inside [from, to)? Wraps past midnight when `from > to`.
/// `from == to` is never active (an empty window, not a 24h one) — the
/// ambiguity is resolved here, once, with a test, instead of in the caller.
pub fn window_active(now: u16, from: u16, to: u16) -> bool {
    if from == to { false }
    else if from < to { now >= from && now < to }
    else { now >= from || now < to }
}
```

**Step 3 — the tick (Engine, ~90 lines).**

`App` gains two non-persisted fields:

```rust
/// Last schedule verdict, so the loop acts on *edges* only. Acting every
/// tick would re-assert the schedule over a manual toggle every 30 seconds
/// and make the settings row look broken (FR-83).
sched_last: Option<bool>,
/// A manual toggle that outranks the schedule until the next edge (FR-83).
sched_override: Option<bool>,
```

In `run()`'s existing cadence block (`app/mod.rs:511-524`, alongside `last_poll`), a
`last_schedule_check` at **30s**:

1. Skip unless `config.alt_schedule_enabled`.
2. `now = chrono::Local::now()`; `now_min = hour * 60 + minute`.
3. `active = window_active(now_min, from, to)` — a `None` from `parse_hhmm` warns loudly once
   and disables the check for the session; it never guesses.
4. If `Some(active) == sched_last`, return (no edge).
5. Edge: clear `sched_override`, set `config.use_alt_rates = active`, call the existing
   `apply_rate_limits(app)`, persist, emit a toast (*"alt speed limits on until 07:00"*),
   set `sched_last = Some(active)`.

30s is chosen against `NFR-04`: two wall-clock reads and an integer compare per minute is
unmeasurable, while a per-frame check at 30fps would be 108 000 `Local::now()` calls an hour for
a value that changes twice a day.

`settings_toggle_row`'s `UseAltRates` arm sets `sched_override = Some(new_value)` when a schedule
is enabled, which the edge logic then clears — FR-83 in three lines.

**Step 4 — the settings rows (UI, ~70 lines).**

Three `SettingDef` entries in `Category::Speed` (#63's table): one `ToggleField::AltSchedule` and
two `TextField::AltScheduleFrom` / `AltScheduleTo`. Commit validates through `parse_hhmm` and
**keeps the edit open with a loud banner** on a bad value — the exact pattern
`settings_edit_text` already uses for the numeric rows (`app/settings.rs:148`). The row values
render as `22:00 → 07:00 (active)` so the current verdict is visible without arithmetic.

**Step 5 — toasts (UI, ~180 lines).**

`src/ui/toast.rs`:

```rust
pub enum ToastKind { Success, Info, Warn }
pub struct Toast { pub kind: ToastKind, pub text: String, pub expires: Instant }
/// At most three. A burst of ten completions must not become a wall of
/// toasts covering the list the user is reading.
pub const MAX_TOASTS: usize = 3;
pub const TTL: Duration = Duration::from_secs(4);
```

`AppState.toasts: Vec<Toast>`. `App::toast(kind, text)` pushes and truncates from the front.
Expiry is swept in the loop next to the spinner advance, which already receives `now` — no timer,
no task, no channel. Rendered right-aligned in the rows directly above the status bar, each a
one-line `Clear` + styled span using the existing `success`/`accent`/`warning` theme tokens.

Height accounting goes through `app/mod.rs::status_height`, whose doc comment
(`app/mod.rs:675`) already records that under-allocating one row here made the safe-mode banner
invisible. `status_height` becomes `banner_height(..) + toast_height(..) + 1`, and
`mouse_view_area` follows automatically because it already calls `status_height`.

Emission sites, deliberately few:

| Event | Toast |
| --- | --- |
| `EngineEvent::Done` in `apply_event` | `Success`: "<name> finished" |
| schedule edge (step 3) | `Info`: "alt speed limits on until 07:00" |
| settings saved | `Info`: "settings saved" |
| `EngineEvent::Failed` | **banner, not toast** (UR-13 as amended) |

## Files to create / modify

- `SPEC.md` — FR-82/FR-83 in §4.4, UR-20 + the UR-13 amendment in §5. **First commit.**
- `Cargo.toml` — `chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }`.
- `src/core/schedule.rs` — **new**: `parse_hhmm`, `window_active`. Pure, no clock.
- `src/core/mod.rs` — `pub mod schedule;`.
- `src/persist.rs` — the three `Config` fields + defaults.
- `src/app/mod.rs` — `sched_last` / `sched_override`; the 30s check; toast expiry sweep;
  `status_height` including `toast_height`; `App::toast`.
- `src/app/settings.rs` — `AltSchedule*` rows; `parse_hhmm` validation; the FR-83 override.
- `src/app/actions.rs` — the `Done` toast in `apply_event`.
- `src/ui/settings.rs` — three `SettingDef` entries (needs #63).
- `src/ui/toast.rs` — **new**: `Toast`, `ToastKind`, `draw`, `toast_height`.
- `src/ui/mod.rs` — `pub mod toast;`, `AppState.toasts`.
- `src/ui/help.rs` — no new keybind (toasts are not interactive); no change expected.

## Key APIs / libraries

- **chrono 0.4.45** — `chrono::Local::now()` → `chrono::Timelike::{hour, minute}`. Already in
  `Cargo.lock` line 434 via `librqbit-dht`, with `iana-time-zone` present (clock feature on).
  Declared explicitly anyway; **zero new crates**.
- **Rejected: `time`** — `UtcOffset::current_local_offset()` errors in multithreaded processes
  (time-rs docs / issue #380, checked 2026-08-16) and harbour is multithreaded by design. The
  escape hatch is `set_soundness(Unsound)`, which is UB-by-opt-in. Not acceptable.
- **Rejected: `jiff`** — technically excellent, in-tree at 0.2.35, but only via `serde_with`; a
  direct dependency there is a longer leash to a crate harbour does not otherwise pin.
- **Rejected: `cron` / `croner`** — a full cron parser for one daily window. Two `u16`s and a
  comparison is the whole feature; a cron expression is a worse UI *and* a new crate.
- **Rejected: `notify-rust` / OS notifications / a tray icon** — explicitly out of scope per the
  issue, and it would breach `NFR-10`'s "no telemetry, files stay on disk" spirit by adding a
  D-Bus/WinRT surface for a TUI. Named here so it is rejected once, in writing.
- **librqbit** — nothing new. `engine.set_speed_limits(down, up)` already exists and is already
  called from `apply_rate_limits`; the scheduler never touches librqbit directly.
- **ratatui 0.30.2** — toasts are `Clear` + `Paragraph`, the same construction as the error
  banner. No new widget.

## Risks / edge cases

- **Overnight wrap is the classic bug.** `22:00`–`07:00` means `from > to`. `window_active`
  handles it in one branch with a test per case; nothing else in the codebase may re-derive it.
- **DST transitions.** `Local::now()` handles them; `now_min` is a wall-clock minute, so a
  "spring forward" simply skips minutes and a "fall back" repeats them. The 30s edge check makes
  a repeated hour harmless: the verdict is unchanged, so no edge fires. Worth a why-comment.
- **Timezone changes mid-session** (laptop crossing a boundary). The next tick reads the new
  local time; nothing is cached. This is a reason to call `Local::now()` per tick rather than
  caching an offset at boot.
- **The scheduler must not fight the user.** FR-83 exists for this. Without the override, a user
  who turns alt rates off at 23:00 sees them turn back on 30 seconds later, with no explanation.
  Test it explicitly.
- **A bad `HH:MM` in a hand-edited config.** `parse_hhmm` returns `None`, the app warns once and
  runs the session unscheduled. It never guesses `00:00`, and it never silently rewrites the
  user's file.
- **`00:00`–`00:00`.** Defined as *never active* (FR-82). Someone will argue for "always"; the
  answer is that "always on alt rates" is already expressible by just turning alt rates on.
- **A toast wall.** Clearing a large finished queue can emit many `Done` events at once.
  `MAX_TOASTS = 3` plus front-truncation bounds it; a test with ten rapid pushes asserts three.
- **Toasts must not eat the banner's rows.** `status_height` is the one place both are summed,
  and its existing doc comment is the warning. A test asserts a banner and three toasts coexist
  at 80×24 with the search list still rendering.
- **`NFR-04` idle CPU.** The 30s cadence is the mitigation; do not move the check into the 30fps
  draw path "because it is cheap".
- **Rejected: per-day-of-week schedules.** qBittorrent has them; the issue does not ask for them,
  and they multiply the config surface and the test matrix. If wanted, they are additive later
  (`alt_schedule_days: Vec<u8>` with `#[serde(default)]` meaning every day).

## Test strategy

- **Unit, `src/core/schedule.rs`** (the highest-value tests in this issue, and clock-free):
  `parse_hhmm` accepts `"00:00"`, `"22:00"`, `"23:59"`; rejects `"24:00"`, `"22:60"`, `"22"`,
  `"10:00 PM"`, `""`. `window_active` — normal window `09:00–17:00` true at 09:00, true at
  16:59, false at 17:00, false at 08:59; wrapping `22:00–07:00` true at 22:00, true at 23:59,
  true at 00:00, true at 06:59, false at 07:00, false at 12:00; degenerate `from == to` false at
  every minute of the day (a 1440-iteration loop).
- **Unit, `src/app/mod.rs`** — the edge logic against an injected `now_min` (the check takes the
  minute as a parameter so the test needs no clock): no action when the verdict is unchanged;
  action exactly once on transition; `sched_override` suppresses one edge and is cleared by it.
- **Unit, `src/persist.rs`** — a `config.toml` written by a pre-#66 build round-trips with the
  three new fields at their defaults (the `#[serde(default)]` additive guarantee).
- **Unit, `src/ui/toast.rs`** — expiry drops a toast at `expires`, not before; ten pushes leave
  three, and the three are the newest.
- **Buffer snapshot, `src/ui/tests.rs`** — a `Success` toast renders its text above the status
  bar at 80×24; a banner plus three toasts coexist without clipping the list; zero toasts costs
  zero rows.
- **Buffer snapshot, settings** — the schedule rows render `22:00 → 07:00` and the active marker.
- **No engine integration test.** Nothing in this issue touches the swarm;
  `engine.set_speed_limits` is already exercised by the existing settings path.

## Verification

1. `cargo run` → `shift+S` → `Speed`. Set **Alt Download Limit** to `1`, **Alt Schedule** on,
   window `from` = one minute from now, `to` = ten minutes from now. Wait.
   At the edge: a toast appears (*"alt speed limits on until …"*), **Use Alternative Rates**
   flips to `[● ON]` on screen, and an active download's speed drops to ~1 MiB/s.
   **That speed drop is the verification** — the config value and the toast are not enough.
2. While the window is active, press Enter on **Use Alternative Rates** to turn it off. Speed
   recovers, and it **stays** off for the rest of the window (FR-83). At the window's end edge it
   resumes following the schedule.
3. `~/.harbour/config.toml` contains `alt_schedule_enabled`, `alt_schedule_from`,
   `alt_schedule_to`; a config file from before this change still loads with no banner.
4. Set `alt_schedule_from = "25:00"` by hand and relaunch → one loud banner naming the bad value,
   the app runs unscheduled, and the file is **not** rewritten.
5. Finish a download → a green *"<name> finished"* toast appears above the status bar and
   disappears on its own after ~4s, without stealing keys: hold `↓` throughout and the selection
   keeps moving.
6. Force an engine failure → it appears in the **error banner**, not as a toast (UR-13 as
   amended).
