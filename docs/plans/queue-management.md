# Torrent queue management (max active)
Ref: #44

## Goal
Extend harbour's single download cap into qBittorrent's three-cap model (max active downloads,
max active seeds, max total active) and give the user explicit control over **which** item
takes the next free slot.

## The finding that shapes this plan

This feature is **entirely harbour's**, not librqbit's. librqbit 8.1.1 has no queueing layer:
`Session::add_torrent` starts a torrent immediately and there is no "max active" anywhere in
`src/session.rs`. `src/queue.rs`'s module docs already state the division — *"The queue owns
policy; the engine owns transfer."* Nothing in this plan needs an upstream change, and nothing
in it should reach into librqbit.

**What already exists in harbour (do not rebuild it):**

- `Queue::max_downloads` (`src/queue.rs:96`), `0` = unlimited, live-settable via
  `set_max_downloads` (`src/queue.rs:129`).
- `Queue::active_count` / `slot_free` (`src/queue.rs:190-199`).
- `Queue::promote` (`src/queue.rs:262-282`) — starts queued items **oldest-first** by
  `added_at_epoch_ms`, skipping items still awaiting magnet resolution so one unresolved row
  cannot stall the ones behind it.
- `freed_slot` bookkeeping in `Queue::tick` (`src/queue.rs:406, 455, 463, 523-526`).
- `Config::max_active_downloads` (`src/persist.rs:75`) and settings row 10.
- `HARBOUR_MAX_DOWNLOADS` env + **FR-07** in SPEC.

**The load-bearing contract this issue changes.** `QueueStatus::is_active_download`
(`src/core/types.rs:376-379`) returns true for `Downloading` **and nothing else**, and the test
`only_downloading_consumes_a_concurrency_slot` (`src/core/types.rs:995-1009`) pins that
explicitly. `Queue::restore` (`src/queue.rs:595`) restarts seeds before downloads with the
comment *"they hold no download slot"*. Adding a seed cap and a total cap means **seeds now
consume a slot too** — the single riskiest change in this batch of five issues, because two
existing tests and one restart path assert the opposite today.

## SPEC / FR reference

**Exists today: FR-07** — `HARBOUR_MAX_DOWNLOADS` read at startup, `0`/unset = unlimited
(SPEC.md:73). **FR-30** — any number of items can be queued (SPEC.md:156).

**Missing from SPEC — add first, then implement.** FR numbers **FR-90 … FR-93** (FR-69…FR-89
are claimed: FR-69…FR-85 by the existing plans in `docs/plans/`, FR-86…FR-89 by
`speed-limits.md`). Add to §4.4, cross-referenced from FR-07.

- **FR-90 (three caps).** harbour bounds *max active downloads*, *max active seeds*, and *max
  total active* independently. Each is `0` = unlimited. An item starts only when **every**
  applicable cap has room. `HARBOUR_MAX_DOWNLOADS` remains the boot default for the downloads
  cap only, preserving FR-07.
- **FR-91 (what occupies a slot).** A `downloading` item occupies a download slot; a `seeding`
  item occupies a seed slot; both occupy a total slot. `queued`, `paused`, `failed` and
  `missing` occupy nothing. A seed displaced by the seed cap becomes `queued`, not `paused` —
  it is waiting for a slot, and `paused` means the user asked for it.
- **FR-92 (queue order).** The queue has an explicit user-controlled order. Promotion takes the
  highest-priority eligible item, not the oldest. Default order is insertion order, so
  behaviour is unchanged until the user reorders. The user can move an item up, down, to top,
  and to bottom.
- **FR-93 (order is durable).** Queue order survives a restart, and a restored queue promotes
  in the same order it would have before the restart.

## Workstream

**Engine & Foundation (Sarthak)** owns every step. `QueueStatus::is_active_download`,
`QueueItem` and `Queue` are all frozen shared types/contracts — this issue changes their
semantics, so it cannot be done from the UI track.

**Terminal UI (Ishan)** owns only step 5 (the reorder keybinds and the queue-position column),
building against the frozen types.

**Depends on:** `speed-limits.md` **step 1** (the settings-row table). This plan adds two
settings rows; doing it against the hardcoded index match in `src/ui/settings.rs:98-124` would
conflict with the other three issues in this batch.

## Approach

**Step 1 — SPEC FR-90…FR-93 (docs only).**

**Step 2 — slot accounting becomes explicit (engine, no behaviour change yet).**
Replace the single `is_active_download` predicate with a small enum-side helper on
`QueueStatus`:

```rust
pub enum SlotKind { Download, Seed, None }
pub fn slot_kind(self) -> SlotKind
```

Keep `is_active_download` as a `#[deprecated]`-free thin wrapper (`slot_kind() ==
SlotKind::Download`) so the FR-07 tests and any UI call sites keep compiling and keep passing.
Introduce `Queue::seed_count()` and `Queue::total_active_count()` alongside `active_count()`.
**No caps are enforced yet** — this step is pure accounting and ships with tests proving the
counts are right, which is what makes step 3 safe.

**Step 3 — the three caps gate promotion (engine).**
`slot_free()` becomes `slot_free_for(kind: SlotKind) -> bool`, checking the item's own cap and
the total cap. `promote()` consults it per candidate. Because a seed now occupies a slot,
`Queue::tick` must also **demote**: when the seed cap is exceeded (e.g. the user lowers it, or
a download finishes into a full seed tier), the lowest-priority seeds move to `Queued` and are
stopped in the engine. This is the mirror of promotion and must live in the same place.

**Step 4 — explicit queue order (engine).**
`QueueItem` gains `#[serde(default)] queue_pos: u32` — following the `bytes` precedent at
`src/core/types.rs:406`, so ledgers written by older builds still load. `promote()` sorts by
`(queue_pos, added_at_epoch_ms)`; the tiebreak on `added_at_epoch_ms` means a ledger where
every `queue_pos` defaults to `0` promotes **exactly as it does today**, which is what makes
this change safe to land. `Queue` gains `move_up`/`move_down`/`move_to_top`/`move_to_bottom`,
each renormalising positions to `0..n` so they cannot drift.

**Step 5 — the UI (UI track).**
Downloads view: a queue-position column for `queued` items only, and `K`/`J` (or
`Shift+↑`/`Shift+↓`) to move the selected item up/down, `Home`/`End` variants for top/bottom.
Two settings rows: max active seeds, max total active. Follow the existing "views are pure
paint" rule — `draw` renders `queue_pos`, the app loop calls the `Queue` mutators.

**Step 6 — restart ordering (engine).**
`Queue::restore` (`src/queue.rs:568-618`) currently restarts *all* seeds unconditionally before
promoting. With a seed cap that is now wrong: it would start every seed regardless of the cap.
Change it to mark restorable seeds `Queued` and let `promote()` place them under the caps,
keeping the seeds-before-downloads intent by giving restored seeds priority within the same
pass. This preserves the comment's actual intent (*"a user expects their seeds back"*) while
respecting FR-90.

## Files to create / modify

- `SPEC.md` — FR-90…FR-93 in §4.4; a pointer from FR-07.
- `src/core/types.rs` — `SlotKind`, `QueueStatus::slot_kind`, `QueueItem::queue_pos`; keep
  `is_active_download` as a wrapper so FR-07's test survives.
- `src/queue.rs` — `seed_count`, `total_active_count`, `slot_free_for`, demotion in `tick`,
  order-aware `promote`, the four move methods, the `restore` change.
- `src/persist.rs` — `max_active_seeds`, `max_total_active` on `Config`.
- `src/app/settings.rs` — apply both live via new `Queue` setters (same shape as
  `set_max_downloads`).
- `src/ui/settings.rs` — two rows in the step-1 table.
- `src/ui/downloads.rs` — the queue-position column.
- `src/input.rs`, `src/app/actions.rs` — reorder keybinds and dispatch.
- `docs/plans/queue-management.md` — this file.

## Key APIs / libraries

No librqbit API is involved; verified by reading `librqbit-8.1.1/src/session.rs` (no queueing
concept exists) on 2026-08-16. librqbit 8.1.1 and ratatui 0.30.2 are both current
([crates.io/crates/librqbit](https://crates.io/crates/librqbit),
[github.com/ratatui/ratatui/releases](https://github.com/ratatui/ratatui/releases), checked
2026-08-16); neither needs bumping for this.

Reference semantics for the three-cap model taken from qBittorrent's Options → BitTorrent
queueing panel; the "any one condition triggers" style is the same one confirmed for share
limits (see `share-limits.md`).

**New crates: none.**

## Risks / edge cases

- **This changes a frozen contract, and two existing tests encode the old one.**
  `only_downloading_consumes_a_concurrency_slot` (`src/core/types.rs:995`) asserts seeding holds
  no slot, and `completion_moves_to_seeding_and_frees_a_slot` (`src/queue.rs:902`) asserts the
  same from the queue side. Both must be **rewritten deliberately, with a comment saying why**,
  not deleted. A silently deleted test is how a contract change becomes a regression.
- **Seed-cap thrash.** A download finishing into a full seed tier immediately gets demoted to
  `queued`, which frees a download slot, which promotes another download, which finishes… If
  the caps are set tightly this can oscillate every tick. Renormalise positions once per
  mutation, and demote only the *lowest-priority* excess seeds — never re-sort the whole tier
  per tick.
- **A demoted seed must not look like a user pause.** FR-91 exists for this: `queued`, not
  `paused`. Conflating them means `p` no longer round-trips, and a user cannot tell whether the
  app or they stopped a seed.
- **`queue_pos` collisions after a partial ledger recovery.** `src/persist.rs` salvages what it
  can from a corrupt ledger (`Loaded::Recovered`). Duplicate or sparse positions must be
  tolerated — the `(queue_pos, added_at_epoch_ms)` tiebreak makes ties deterministic, and the
  first `promote()` renormalises.
- **The total cap must not deadlock the download tier.** If `max_total_active` equals
  `max_active_seeds` and the seed tier is full, downloads never start. Guard: the total cap is
  checked *per candidate*, and seeds are demoted before downloads are promoted within one
  `tick`, so a download can always displace an excess seed.
- **Do not add a fourth cap for "active checking".** librqbit's `Initializing` state is
  projected to `Downloading`/`Seeding` by `project_status` (`src/core/types.rs:585`) and is not
  separately observable. Out of scope; say so rather than inventing a state.

## Test strategy

- **Unit, `src/core/types.rs`** — `slot_kind` for all six statuses; `is_active_download` still
  agrees with `slot_kind() == Download` (the FR-07 compatibility net); `QueueItem` round trips
  with and without `queue_pos` (a legacy ledger loads with `0`).
- **Unit, `src/queue.rs`** against `FakeEngine` — the heart of this issue:
  - each cap enforced independently; `0` means unlimited for each.
  - a finished download demotes an excess seed rather than exceeding the seed cap.
  - the total cap blocks a start even when the per-tier cap has room.
  - lowering a cap at runtime demotes down to it; raising it promotes back up.
  - **default order is unchanged**: with every `queue_pos == 0`, promotion is oldest-first,
    identical to `a_freed_slot_promotes_the_oldest_waiter_first` (`src/queue.rs:713`) today.
  - `move_to_top` promotes that item next; `move_to_bottom` promotes it last; positions stay
    contiguous after repeated moves.
  - `restore` under a seed cap starts at most `max_active_seeds` seeds and leaves the rest
    `Queued`, not `Paused`.
- **Buffer snapshot, `src/ui/tests.rs`** — the position column renders for `queued` rows and is
  blank for active ones; two new settings rows render their values and `unlimited` at `0`.
- **No engine integration test.** Nothing here touches the network; a `HARBOUR_TEST_NET=1` test
  would prove nothing that `FakeEngine` does not prove faster.

## Verification

1. `SPEC.md` §4.4 contains FR-90…FR-93 and FR-07 points at FR-90.
2. `cargo run` with max active downloads = 2, max active seeds = 1, max total = 2. Queue five
   torrents. **Exactly two download at a time; when one finishes it seeds and the second seed
   drops back to `queued`, not `paused`.** That combination is the user-visible proof all three
   caps interact, and it is impossible to produce with today's single cap.
3. Select a `queued` item, press the move-to-top key: its position column reads `1`, and it is
   the next row to start when a slot frees.
4. Quit and relaunch: the queue-position column is unchanged, and promotion order after the
   bootguard resume matches step 3.
5. Set every cap to `0`: everything starts, matching FR-07's unlimited behaviour exactly as
   before this issue.
