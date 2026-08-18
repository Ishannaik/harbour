# Max connections and upload slots (global + per-torrent)
Ref: #56

## Goal
Expose the peer-connection cap librqbit actually enforces — as a default and per torrent — and
record in SPEC that upload slots do not exist, because the engine has no choking algorithm.

## The finding that shapes this plan

Read on **2026-08-16** from `librqbit 9.0.0`, extracted from the crates.io tarball to
`/tmp/librqbit-9.0.0`.

**The connection cap is per torrent, in two places, and merges one way.**

```rust
// session.rs:468  — SessionOptions
/// Default peer limit per torrent.
pub peer_limit: Option<usize>,

// session.rs:284  — AddTorrentOptions
/// Max concurrent connected peers.
pub peer_limit: Option<usize>,

// session.rs:1357 — the merge, at add time
peer_limit: opts.peer_limit.or(self.peer_limit),

// torrent_state/live/mod.rs:282 — where it is finally read
paused.shared.options.peer_limit.unwrap_or(128),
```

Three consequences, all load-bearing for the UI copy:

1. **There is no session-wide total connection cap.** `SessionOptions::peer_limit` is
   documented in its own doc-comment as the *default peer limit **per torrent***. Ten torrents
   at 100 each is 1000 connections, not 100. A row labelled "Global Max Connections" —
   qBittorrent's wording, and the issue's — would be **wrong**. harbour labels it
   `Max Connections per Torrent (default)`.
2. **The default is 128**, hard-coded at the read site, not in `Default`.
3. **The per-torrent value is add-time only.** It is read from
   `paused.shared.options`, and `ManagedTorrentShared` is immutable from outside the crate —
   exactly the same shape `docs/plans/speed-limits.md` (#43) documented for per-torrent rate
   limits, at `torrent_state/mod.rs`. A pause/unpause cycle re-reads the *same* stored value, so
   there is no runtime path.

**Upload slots do not exist. librqbit never chokes anyone.**
`peer_connection.rs:334-342`, in the connection setup path, unconditionally:

```rust
let len = Message::Unchoke.serialize(&mut *write_buf, &Default::default)?;
...
trace!("sent unchoke");
```

and the live-state code tracks only the *inbound* direction —
`i_am_choked` (`torrent_state/live/mod.rs:998,1005,1748,1758`). There is no `am_choking`, no
unchoke scheduler, no optimistic-unchoke rotation, and:

```
$ grep -rn -i "upload_slot|max_uploads|unchoke_slot" /tmp/librqbit-9.0.0/   # 0 matches
```

Upload slots are the *output* of a choking algorithm — "how many peers may I unchoke at once".
With no choking algorithm there is no quantity to limit. **The upload-slots half of this issue
is not implementable and is not built** (FR-107). Upload bandwidth is already controllable via
the rate limiter that `docs/plans/speed-limits.md` covers, which is what most users actually
want from "upload slots".

**This issue depends on the librqbit 8.1.1 → 9.0.0 upgrade** (step 0 of
`docs/plans/protocol-toggles.md`): 8.1.1's `SessionOptions` has no `peer_limit` field at all.

## SPEC / FR reference

**SPEC.md says nothing about connection limits.** `grep -n -i "connection\|peer limit" SPEC.md`
matches only FR-33's peer *display* ("Peers and ETA are absent, not zero"). **SPEC first.**

FR numbers **FR-105 … FR-107** (FR-69…FR-104 claimed by existing plans plus this batch's
`protocol-toggles.md`, `encryption-mode.md`, `port-forwarding-and-binding.md`). Add to §4.5.

- **FR-105 (default connection cap).** harbour applies a maximum number of concurrent peer
  connections **per torrent**, defaulting to the engine's 128. Empty means the engine default.
  The value is read at engine start and applies to torrents started afterwards; it is not a
  session-wide total, and the setting label says so.
- **FR-106 (per-torrent connection cap).** An individual item may carry its own connection cap,
  which overrides the default. It is applied **when the item is handed to the engine** and is
  fixed for that run; editing it while the item is running stores the value and takes effect the
  next time the item starts. The UI states this rather than implying a live control.
- **FR-107 (upload slots are not supported).** harbour exposes no upload-slot setting. The
  engine has no choking algorithm — every connected peer is unchoked
  (librqbit 9.0.0 `peer_connection.rs:334`, verified 2026-08-16) — so there is no slot count to
  limit. Upload bandwidth is bounded by the upload rate limit (FR-86/FR-87) instead. Re-evaluate
  when librqbit implements choking.

## Workstream

- **Step 0 (librqbit 9 upgrade)** — **Engine (Sarthak)**; specified in
  `docs/plans/protocol-toggles.md`. Blocks this issue.
- **Steps 1–3 (SPEC, config, engine, shared types)** — **Engine & Foundation (Sarthak)**.
- **Steps 4–5 (settings row, per-item row)** — **Terminal UI (Ishan)**.

**Shared-type dependency — this is the one that needs Sarthak first.** `AddRequest` and
`AddBytesRequest` (`src/core/types.rs:600-625`) and `QueueItem` (`:387-416`) each gain a field.
Those are the frozen contract; the UI track must not define a parallel type.

**Coordinate with `docs/plans/speed-limits.md` (#43) step 4**, which adds `limits:
Option<SpeedLimits>` to the *same three structs* for the same add-time reason. Land #43's step 4
first and add `max_connections` alongside it, or the two PRs conflict in `core/types.rs`,
`queue.rs` and `engine/fake.rs` simultaneously. **If both are in flight, they should be one PR**
— same seam, same tests, and together still well under 400 lines.

**Row-table prerequisite** (stated identically in all five plans of this batch): the settings
row-table refactor from step 1 of `docs/plans/speed-limits.md` / `categorized-settings.md`
lands first; rows are values, not indices. This issue contributes the
`Max Connections per Torrent (default)` row to the Connection block whose order is fixed in
`docs/plans/protocol-toggles.md`.

## Approach

**Step 1 — SPEC FR-105…FR-107 (docs only, ~30 lines).**

**Step 2 — config + engine default (engine, ~60 lines).**

```rust
// src/persist.rs, Config
/// Max concurrent peers per torrent; None = the engine default (128).
/// Boot-time: librqbit reads it when a torrent starts.
pub max_connections: Option<usize>,     // default: None
```

`EngineLaunchOptions` carries it into `SessionOptions { peer_limit, .. }` in
`RqbitEngine::new`. A why-comment at that line records the two facts a future reader needs:
it is *per torrent*, and 128 is the engine's fallback (`live/mod.rs:282`) — so the settings row
can honestly render `engine default (128)` for `None` without that number being invented in the
UI layer.

**Step 3 — per-torrent cap reaches the engine (engine, ~80 lines).**

- `AddRequest` and `AddBytesRequest` gain `max_connections: Option<usize>`.
- `RqbitEngine::add` / `add_bytes` (`src/engine/rqbit.rs:423-475`) map it straight into
  `AddTorrentOptions { peer_limit, .. }`.
- `QueueItem` gains
  `#[serde(default, skip_serializing_if = "Option::is_none")] max_connections: Option<usize>`,
  following the `bytes` field precedent (`src/core/types.rs:406`) so old ledgers still load.
- `Queue::add_item_to_engine` passes `item.max_connections` through.
- `FakeEngine` records what it was handed, so queue tests assert arrival without a network.

**Step 4 — the settings row (UI, ~40 lines).**

One `TextField::MaxConnections` row:

```
Max Connections per Torrent (default)      engine default (128)
```

Reuses `parse_opt_number` (`src/app/settings.rs:78-87`) unchanged: empty ⇒ `None` ⇒ engine
default; a non-number ⇒ loud warning, edit stays open. One extra guard beyond that: **`0` is
rejected**, with `"0 would allow no peers at all — leave empty for the engine default"`.
`Some(0)` is a legal `usize` that librqbit would honour by connecting to nobody, and the torrent
would sit at 0% forever with no error — the archetypal silent failure. Reject at the parse site,
the same way the listen-port row rejects `> 65535` (`src/app/settings.rs:200-216`).

**Step 5 — the per-item cap (UI, ~70 lines).**

The same seam and the same honesty rule as #43's per-torrent speed limit, so they share one
key and one prompt if both land: an item-scoped editor in the downloads view that writes
`QueueItem.max_connections`, with the label
`max connections (applies on next start)` whenever the item is **not** `Queued`. It never
removes and re-adds the torrent to force the value live — that would re-run initialization and
re-verify every piece on disk (the rejection is argued in full in `docs/plans/speed-limits.md`
and is not re-litigated here).

**Step 6 — upstream issue (no harbour code).** File on `ikatson/rqbit`: implement peer choking
with a configurable unchoke-slot count, and expose per-torrent `peer_limit` at runtime (a
`Session::update_torrent_peer_limit` mirroring the existing public
`Session::update_only_files`). Link it from FR-106 and FR-107. #43's step 7 files the sibling
request for runtime rate limits — one issue covering both runtime knobs is fine and preferable.

## Files to create / modify

- `SPEC.md` — FR-105…FR-107 in §4.5.
- `src/persist.rs` — `max_connections: Option<usize>`; round-trip + partial-config tests.
- `src/core/types.rs` — `max_connections` on `AddRequest`, `AddBytesRequest`, `QueueItem`
  (Sarthak; coordinate with #43 step 4).
- `src/engine/rqbit.rs` — `EngineLaunchOptions.max_connections` → `SessionOptions.peer_limit`;
  per-request → `AddTorrentOptions.peer_limit`; the per-torrent-is-add-time-only invariant in the
  module docs with the exact upstream file:line, so the next librqbit bump re-checks it.
- `src/engine/fake.rs` — record the received cap.
- `src/queue.rs` — pass `item.max_connections` through `add_item_to_engine`.
- `src/ui/settings.rs` — `TextField::MaxConnections` + its row; `engine default (128)` rendering.
- `src/app/settings.rs` — the commit arm, including the `0` rejection.
- `src/ui/downloads.rs`, `src/input.rs`, `src/app/actions.rs` — the per-item editor (shared with
  #43's if both land).
- `src/ui/tests.rs` — snapshots.

## Key APIs / libraries

Verified 2026-08-16 by reading extracted crate source:

- `SessionOptions.peer_limit` — `librqbit-9.0.0/src/session.rs:468` (doc-comment: *"Default peer
  limit per torrent"*).
- `AddTorrentOptions.peer_limit` — `session.rs:284`.
- Merge — `session.rs:1357` (`opts.peer_limit.or(self.peer_limit)`).
- Read site and the 128 fallback — `torrent_state/live/mod.rs:282`.
- Unconditional unchoke — `peer_connection.rs:334-342`; inbound-only choke state —
  `torrent_state/live/mod.rs:998,1005,1748,1758`.
- librqbit 9.0.0 is current stable — `crates.io/api/v1/crates/librqbit`
  (`max_stable_version 9.0.0`, `updated_at 2026-08-15`).

**New crates: none.**

## Risks / edge cases

- **The naming is the biggest risk in this issue.** "Global max connections" is what the issue
  says and what qBittorrent shows, and it is not what librqbit implements. Shipping that label
  over a per-torrent field would mislead every user who sets it to 200 and then wonders why ten
  torrents opened 2000 sockets. The label, the FR and the `?` detail text must all say
  *per torrent*.
- **Rejected: synthesising a real global cap in harbour** by dividing the budget across active
  torrents and re-adding torrents when the count changes. It would need a re-add per torrent on
  every queue change — a full piece re-verify each time (see #43) — to approximate a number the
  engine could enforce properly in ten lines. Wrong layer.
- **Rejected: an upload-slots row wired to nothing.** Same argument as the encryption mode in
  `docs/plans/encryption-mode.md` (#54): a control that reports a limit which does not exist is
  worse than its absence. FR-107 documents it.
- **`Some(0)` is a silent stall.** Named twice on purpose: rejected at the parse site with a
  message that says what to do instead.
- **Very low caps look like broken downloads.** A cap of 2 on a healthy swarm gives a
  desperately slow download and no error anywhere. The `?` detail text should name a sane floor
  (the engine's own default is 128); we do not enforce one beyond rejecting 0 — refusing a
  legitimate low value would be paternalistic, and the row states the tradeoff.
- **Old ledgers.** `QueueItem` gains a field: `#[serde(default)]` plus a persist test that a
  ledger written before this change still loads, or FR-54's quarantine path fires on everyone's
  existing queue.
- **The cap is not retroactive.** Changing the default in settings does nothing to torrents that
  are already running — including after a pause/resume, because `shared.options` is immutable.
  The row's "(next launch)" phrasing must therefore be exact: it applies to torrents *started*
  after the change, which for the session default effectively means the next launch.

## Test strategy

- **Unit, `src/persist.rs`** — `max_connections` round-trips; an older config loads it as `None`.
- **Unit, `src/app/settings.rs`** — commit `""` ⇒ `None`; `"200"` ⇒ `Some(200)`; `"0"` ⇒
  unchanged config, `editing` still true, warning raised; `"abc"` ⇒ same rejection path.
- **Unit, `src/queue.rs`** against `FakeEngine` — an item with `max_connections = Some(50)` hands
  exactly `Some(50)` to the engine on start; an item without hands `None`; the value survives a
  ledger save/load round trip.
- **Unit, `src/ui/settings.rs`** — the row renders `engine default (128)` for `None` and the
  number otherwise; `TextField::MaxConnections` maps from exactly one table row.
- **Buffer snapshot, `src/ui/tests.rs`** — the settings row; the per-item editor showing
  `applies on next start` for a running item and not for a queued one.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — add a well-seeded magnet with
  `max_connections = Some(4)`, poll `snapshot()` for ~30s and assert the reported
  `peers` never exceeds 4. This is the only test that proves the value reaches librqbit rather
  than merely reaching `AddTorrentOptions`, and `EngineStats::peers` already reports *live* peers
  (`src/engine/rqbit.rs:378`), which is exactly the quantity being capped.

## Verification

1. `SPEC.md` §4.5 has FR-105…FR-107, and FR-107 cites `peer_connection.rs:334` for the
   no-choking claim.
2. `cargo run` → `shift+S` → the row reads `Max Connections per Torrent (default)` — **not**
   "Global" — and shows `engine default (128)` before anything is set.
3. Set it to `4`, relaunch, start a well-seeded torrent, and watch the downloads view's peer
   column: it plateaus at 4 and never exceeds it. Clear the value, relaunch, and the same
   torrent climbs well past 4. That before/after on the peer column is the user-visible proof.
4. Enter `0`: a banner explains why 0 is refused, the edit stays open, and `config.toml` is
   unchanged.
5. Set a per-item cap on a **running** item: the UI says `applies on next start`, the peer count
   does not change, and `grep -n "remove" src/app/actions.rs` shows no remove/re-add on that
   path. Pause and resume the item; still unchanged (the immutability finding, confirmed by
   hand once before merge). Restart harbour: now it applies.
6. `grep -rn -i "upload_slot\|max_upload" src/` returns nothing — the unimplementable half was
   not shipped, and FR-107 says why.
