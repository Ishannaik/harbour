# Share ratio / seed-time limits
Ref: #45

## Goal
Turn harbour's existing ratio-stop into qBittorrent's full share-limit model — ratio **and**
seeding time, a configurable action when either is reached, and a per-category override — with
a seed clock that survives a restart.

## What already exists in harbour (do not rebuild it)

- `Queue::stop_ratio: Option<f64>` (`src/queue.rs:97-100`), live-settable via `set_stop_ratio`
  (`src/queue.rs:134`).
- The ratio check inside `Queue::tick` (`src/queue.rs:487-501`):

  ```rust
  if item.finished
      && item.status == QueueStatus::Seeding
      && self.stop_ratio.is_some_and(|target| {
          let d = snap.stats.downloaded_bytes;
          d > 0 && snap.stats.uploaded_bytes as f64 / d as f64 >= target
      })
  {
      item.status = QueueStatus::Paused;
      ratio_paused.push(snap.id.clone());
  }
  ```

  Note the shape: the pause is collected into `ratio_paused` and executed **after** the loop, so
  no borrow crosses an `await`. Any new action must preserve that structure.
- `Config::stop_seed_at_ratio` + `Config::seed_ratio` (`src/persist.rs:84-88`), settings rows
  15 (toggle) and 16 (text).
- `EngineStats::uploaded_bytes` and `downloaded_bytes` (`src/core/types.rs:463-466`), already
  mapped from librqbit at `src/engine/rqbit.rs:396`.

**So the ratio half is built.** This issue adds the time half, the action, the per-category
override, and — the part that actually needs care — a **durable** seed clock.

## The finding that shapes this plan

**The seed clock is not durable today.** `Runtime::seed_started_at: Option<Instant>`
(`src/queue.rs:53`) lives in the `Runtime` struct, which `src/queue.rs:48` documents as *"One
entry's runtime bookkeeping — **never persisted**"*, and `Instant` is a monotonic clock with no
meaning across processes — it cannot be serialised even if we wanted to. It exists solely to
anchor the file-gone detector's `SEED_GRACE`.

A seeding-time limit therefore **cannot** be built on it: every restart would reset the clock,
and a user with a 24-hour limit who restarts daily would seed forever. This needs a new durable
field on `QueueItem`, which is a frozen shared type — Sarthak's, following the `bytes` field
precedent at `src/core/types.rs:406-407`.

**Reference semantics**, from qBittorrent's Options → BitTorrent → Share Ratio Limiting
(checked 2026-08-16 via [qBittorrent's "How to disable auto seed"
wiki](https://github.com/qbittorrent/qBittorrent/wiki/How-to-disable-auto-seed) and the
[share-limit-priority issue](https://github.com/qbittorrent/qBittorrent/issues/23875)):
qBittorrent evaluates **three concurrent conditions** — share ratio, total seeding time, and
inactive seeding time — and takes the configured action when **any one** is met. The actions
offered are pause (stop) or remove. This issue asks for ratio + seeding time + action, i.e.
qBittorrent's model minus the inactive-time condition; note that omission in SPEC rather than
implying full parity.

## SPEC / FR reference

**Nothing in SPEC.md covers share limits.** §4.5 Seeding (FR-42…FR-47) covers seeding by
default, pause/resume, the seeding tab, and the `missing` detector — no ratio, no time, no
action. `Config::stop_seed_at_ratio` shipped ahead of the spec, exactly like the speed limits.
Per AGENTS rule 2, **add to SPEC first**.

FR numbers **FR-94 … FR-97** (FR-69…FR-93 are claimed: FR-69…FR-85 by the existing plans,
FR-86…FR-89 by `speed-limits.md`, FR-90…FR-93 by `queue-management.md`). Add to §4.5.

- **FR-94 (share ratio limit).** A finished seed whose share ratio (uploaded ÷ downloaded)
  reaches its effective limit triggers the configured action. Unset = seed indefinitely. Ratio
  is undefined for a torrent that downloaded nothing (a re-seed of local files), and an
  undefined ratio **never** triggers the action.
- **FR-95 (seeding time limit).** A finished seed that has been seeding for longer than its
  effective limit triggers the same action. Seeding time accumulates across restarts and counts
  only time actually spent in `seeding` — not time paused, queued, or with the app closed.
- **FR-96 (action on reached).** The action is **stop** (the seed pauses, files kept — a paused
  seed per AGENTS vocabulary) or **remove** (the item leaves the queue, **files always kept**).
  Stop is the default. Whichever limit is reached first wins; they are not additive.
- **FR-97 (per-category override).** A category (see #46) may carry its own ratio limit, time
  limit and action. An item's effective limits are its category's when the category sets them,
  otherwise the global ones. Uncategorised items always use the global limits.

## Workstream

**Engine & Foundation (Sarthak)** owns steps 1–4: `QueueItem`, `Queue::tick`, and the effective-
limit resolution are all frozen-contract territory.

**Terminal UI (Ishan)** owns step 5 (settings rows, the seeding-tab columns).

**Shared-type dependencies — two, both real:**

- `QueueItem` gains a durable seed-seconds counter. Sarthak's.
- **FR-97 depends on `Category` from #46.** `categories-tags.md` defines the type; this plan
  builds the *seam* (an `effective_limits(item) -> ShareLimits` resolver with a single
  call site) and wires the global path. **Ship steps 1–5 before #46 lands**; step 6 fills in
  the category branch once `Category` exists. Do not define a competing category type here.

**Depends on:** `speed-limits.md` **step 1** (the settings-row table) — this plan adds three
settings rows.

## Approach

**Step 1 — SPEC FR-94…FR-97 (docs only).**

**Step 2 — a durable seed clock (engine).**
`QueueItem` gains `#[serde(default)] seeded_secs: u64` — total accumulated seeding time. In
`Queue::tick`, an item observed in `Seeding` this tick adds the elapsed wall time since the
previous tick. `tick` already takes `now: Instant` as a parameter *"so the grace period and the
consecutive-observation counter are testable at fixed ticks"* (`src/queue.rs:398-401`) — reuse
that exact mechanism, so the clock is testable without sleeping. Accumulating per tick rather
than storing a start timestamp is what makes "counts only time actually spent seeding" true
across pauses and restarts for free.

Persist on change like any other durable field. Guard the write rate: `seeded_secs` changes
every tick, and the ledger is written by `src/persist.rs`'s atomic temp+rename. Round to whole
seconds and only mark the ledger dirty when the value actually changes, so a 30fps loop does
not rewrite the ledger 30 times a second. **This is the one place this feature can hurt
performance** (NFR-04's idle-CPU budget) and it gets a comment at the decision site.

**Step 3 — the limits become a struct with one resolver (engine).**

```rust
pub struct ShareLimits {
    pub max_ratio: Option<f64>,
    pub max_seed_secs: Option<u64>,
    pub action: LimitAction, // Stop | Remove
}
```

in `core::types`. `Queue` holds the global one; `Queue::effective_limits(&QueueItem)` returns
it today and consults the item's category in step 6. **One resolver, one call site** — the same
reasoning as `project_status`: re-deriving the effective limit at each condition is how the
ratio and time branches drift apart.

**Step 4 — both conditions, both actions (engine).**
Replace the inline ratio predicate at `src/queue.rs:487-501` with a
`limit_reached(&ShareLimits, &EngineStats, seeded_secs) -> bool` free function (pure, trivially
testable) and extend the deferred-action list from `ratio_paused: Vec<InfoHash>` to
`Vec<(InfoHash, LimitAction)>`, executing after the loop exactly as today. `Remove` calls
`self.remove(&id, false)` — **`delete_files: false`, always.** `Queue::remove`'s own docs
(`src/queue.rs:385`) say destructive removal *"is never a default anywhere above this layer"*;
an automatic policy is the last place it should be one.

**Step 5 — the UI (UI track).**
Three settings rows in the step-1 table: seeding time limit (text, minutes), action (a toggle
cycling Stop/Remove — reuse `RowKind::Toggle`'s two-state dispatch rather than inventing a
third kind), and keep the existing ratio rows. The seeding tab
(`src/ui/downloads.rs:301 draw_seeding`) gains a ratio column and a seeded-time column, so a
user can see *why* something is about to stop.

**Step 6 — per-category limits (engine, after #46).**
`effective_limits` consults `item.category` and falls back to the global. One function changes;
nothing else does. That is the entire payoff of step 3.

## Files to create / modify

- `SPEC.md` — FR-94…FR-97 in §4.5.
- `src/core/types.rs` — `ShareLimits`, `LimitAction`, `QueueItem::seeded_secs`.
- `src/queue.rs` — the tick-based seed clock, `effective_limits`, `limit_reached`, the
  `Vec<(InfoHash, LimitAction)>` action list, `set_share_limits` replacing `set_stop_ratio`.
- `src/persist.rs` — `seed_time_limit_mins`, `share_limit_action` on `Config`; the dirty-only
  ledger write for `seeded_secs`.
- `src/app/settings.rs` — apply live via `set_share_limits`; the row-15 toggle becomes the
  action cycle.
- `src/ui/settings.rs` — the three rows in the step-1 table.
- `src/ui/downloads.rs` — ratio + seeded-time columns in `draw_seeding`.
- `docs/plans/share-limits.md` — this file.

## Key APIs / libraries

No librqbit API changes. `uploaded_bytes` / `downloaded_bytes` already arrive through
`to_snapshot` (`src/engine/rqbit.rs:363-403`) from librqbit's `TorrentStats`; verified against
the vendored `librqbit-8.1.1` source on 2026-08-16. librqbit 8.1.1 and ratatui 0.30.2 are
current ([crates.io/crates/librqbit](https://crates.io/crates/librqbit),
[github.com/ratatui/ratatui/releases](https://github.com/ratatui/ratatui/releases), checked
2026-08-16).

Reference behaviour from qBittorrent's share-ratio limiting, cited above.

**New crates: none.** Elapsed time comes from the `Instant` already passed into `tick`; no
`chrono`. `TorrentResult::added` already sets the precedent of unix seconds over a `DateTime`
(`src/core/types.rs:204-205`).

## Risks / edge cases

- **Ratio is undefined when `downloaded_bytes == 0`.** The existing code already guards with
  `d > 0` (`src/queue.rs:492`) — keep it, and add the test. A local re-seed uploads without ever
  downloading; dividing by zero there would give `inf`, instantly trip any limit, and delete a
  user's seed on the first tick. FR-94 states this explicitly so it cannot be "simplified" away.
- **`Remove` must never delete files.** Hard-code `delete_files: false` at the call site with a
  comment. If someone later wants delete-on-limit it is a separate, loudly-confirmed feature —
  not a default that silently eats data.
- **Ledger write amplification.** Naively persisting `seeded_secs` every tick rewrites the whole
  ledger many times a second for every seed. Round to seconds, write only on change. This is
  called out in step 2 and is the reason `QueueItem`'s docs say live stats are *not* persisted
  (`src/core/types.rs:383-386`) — `seeded_secs` is the deliberate exception, and it needs the
  comment explaining why it is not a live stat (it is a cumulative fact, not an instantaneous
  reading).
- **Time must not accumulate while paused.** Only tick-observed `Seeding` counts. A paused item
  short-circuits at `src/queue.rs:433-437` before reaching the accumulator, which gives this for
  free — but it is exactly the kind of thing a later refactor breaks, so it gets a test.
- **A user resuming a stopped seed must not be instantly re-stopped.** If the limit is already
  exceeded, resuming would trip on the next tick and look like the resume key is broken. Resume
  must clear or bypass the limit for that item (qBittorrent's behaviour is to require the user
  to raise the limit). Decide once: **`p` on a limit-stopped seed exempts that item until the
  app restarts**, recorded in FR-96. Without this decision the feature is user-hostile.
- **A limit lowered below current progress fires immediately, for everything.** Setting ratio to
  0.1 with fifty seeds above it pauses or removes fifty items on one tick. Removal is
  irreversible from the queue's perspective — so with `Remove` selected, a lowered limit should
  require a confirmation in the UI before it applies. Note in the plan; implement in step 5.
- **Interaction with #44's seed cap.** A seed sitting in `queued` because of the seed cap is not
  seeding, so its clock does not run. Correct, and worth a cross-reference in both SPECs.

## Test strategy

- **Unit, `src/queue.rs`** — `limit_reached` as a pure function: ratio met / not met /
  undefined (`downloaded == 0`); time met / not met; both unset never trips; whichever is met
  first trips.
- **Unit, `src/queue.rs`** against `FakeEngine`, driving `tick(now)` at synthetic instants (no
  sleeping — the reason `now` is a parameter):
  - `seeded_secs` accumulates only across ticks observed in `Seeding`.
  - a paused seed's clock does not advance.
  - reaching the ratio with `action = Stop` pauses and keeps the item; with `Remove` it drops
    the item and calls `engine.remove(id, false)` — assert the `delete_files` flag is `false`.
  - a resumed limit-stopped seed is not re-stopped on the next tick.
  - an item that downloaded nothing never trips the ratio even after large uploads.
- **Unit, `src/core/types.rs`** — `QueueItem` round trips with and without `seeded_secs`; a
  legacy ledger loads with `0`.
- **Unit, `src/persist.rs`** — the ledger is not rewritten when `seeded_secs` is unchanged.
- **Buffer snapshot, `src/ui/tests.rs`** — the seeding tab renders ratio and seeded time; ratio
  renders as `—` (not `0.00`) for an item that downloaded nothing, matching the em-dash
  convention `EngineStats` already uses for unknown peers.
- **No engine integration test** — the conditions are queue policy and are fully covered by
  `FakeEngine`. A real-network test would only add flakiness.

## Verification

1. `SPEC.md` §4.5 contains FR-94…FR-97, and FR-96 records the resume-exemption decision.
2. `cargo run`, set the seeding time limit to 1 minute and the action to Stop. Seed anything.
   **After a minute the item moves to `paused` with its files intact**, and the seeding tab's
   seeded-time column shows ~1:00. That is the observable proof the time limit works and is the
   half that does not exist today.
3. Quit the app after 40 seconds of seeding, relaunch, resume the seed: the seeded-time column
   resumes near 0:40, not 0:00. **This is the verification that matters** — it is the one
   behaviour a non-durable clock cannot fake.
4. Set the action to Remove and a ratio of 0.01 on a seed with real upload: the item leaves the
   queue and the files are still on disk (`ls` the download dir).
5. `grep -n "delete_files" src/queue.rs` shows the automatic path passing `false`.
