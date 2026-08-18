# Sequential download + first/last piece priority
Ref: #41

## Goal
Make harbour's streaming-friendly download order **true, documented, and tested** — and be
honest that a per-torrent *toggle* is not implementable against librqbit 8.1.1 without an
upstream change.

## The finding that shapes this whole plan

Read on 2026-08-16 from the exact source harbour compiles against,
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/`:

**`src/file_info.rs:15-35`** — the per-file piece order:

```rust
fn iter_piece_priorities(range: Range<usize>) -> impl Iterator<Item = usize> {
    // First and last of each file first, then the rest of pieces in that file.
    let first = once(r.start);
    let last  = once(r.start + r.len().overflowing_sub(1).0);
    let mid   = r.clone().skip(1).take(r.len().overflowing_sub(2).0);
    first.chain(last).chain(mid).take(r.len())
}
```

Its own upstream test asserts `it(0..4) == vec![0, 3, 1, 2]`.

**`src/chunk_tracker.rs:215-228`** — `iter_queued_pieces` walks `file_priorities`, then each
file's `iter_piece_priorities()`.

**`src/torrent_state/live/mod.rs:233-245`** — `file_priorities` is built once, sorted by
filename, above a literal `// TODO: make it configurable`.

**`src/torrent_state/live/mod.rs:1242-1252`** — piece reservation is
`priority_streamed_pieces.chain(natural_order_pieces)`.

**`src/session.rs:234-282`** — `AddTorrentOptions` has **no** `sequential` field. Full field
list: `paused`, `only_files_regex`, `only_files`, `overwrite`, `list_only`, `output_folder`,
`sub_folder`, `peer_opts`, `force_tracker_interval`, `disable_trackers`, `ratelimits`,
`initial_peers`, `preferred_id`, `storage_factory`, `defer_writes`, `trackers`.

**Therefore, in librqbit 8.1.1:**

1. **First/last-piece priority is already on, unconditionally, for every torrent.** There is
   nothing to build and nothing to turn on.
2. **Within a file, the remaining pieces are already requested in ascending order** — i.e.
   sequential. There is nothing to build.
3. **File order is filename-ascending and is not configurable.**
4. **There is no way to turn any of it off**, so a per-torrent boolean toggle would be a
   control that controls nothing.

librqbit 8.1.1 is the current release ([crates.io/crates/librqbit](https://crates.io/crates/librqbit),
[github.com/ikatson/rqbit/releases](https://github.com/ikatson/rqbit/releases), both checked
2026-08-16), so this is not a "wait for the next version" situation.

## SPEC / FR reference

Exists today: **FR-60** — *"Watch mode only activates while the swarm has the requested piece
ranges"* — which is the closest SPEC gets, and `src/engine/rqbit.rs`'s stream server already
depends on the behaviour above without SPEC ever writing it down.

**Missing from SPEC — add first, then implement.** Proposed **FR-80 / FR-81** in §4.4:

- **FR-80 (guarantee).** harbour requests pieces in a streaming-friendly order: for each file,
  its first and last piece first, then the remainder in ascending order; files are ordered by
  filename. This is the engine's unconditional behaviour, not a per-torrent setting. Harbour
  does not expose a switch to disable it.
- **FR-81 (watch-while-downloading).** An open stream (the FR-57 watch path) additionally
  raises a 32 MiB window ahead of the reader's position to the front of the request queue.
  This is what makes seeking-then-waiting work, and it is driven by an actual reader — harbour
  never synthesises one.

FR-80 written as a *guarantee we depend on* rather than a *feature we implement* is the whole
point: it turns the librqbit behaviour into a contract with a test that fails loudly if an
upgrade changes it.

## Workstream

**Engine & Foundation (Sarthak)** owns all of it. There is no UI step, because there is
nothing for a UI to control. Ishan's only involvement is the downloads-row copy in step 3.

Shared-type dependencies: **none, and that is deliberate.** Do not add
`QueueItem.sequential` — a persisted field nothing reads is worse than no field.

## Approach

**Step 1 — SPEC (docs only).** FR-80 / FR-81 into §4.4, cross-referenced from §4.7. This is
the deliverable that actually closes the "first/last piece priority" half of the issue: it is
already true, it was just undocumented and untested.

**Step 2 — a characterisation test that pins the guarantee (engine track).**
`tests/engine_net.rs`, gated behind `HARBOUR_TEST_NET=1`: add a real multi-piece magnet, poll
until ~15% complete, then prove the *last* bytes of the largest file are already on hand while
the middle is not. If a librqbit upgrade ever reorders this, CI says so instead of the watch
feature quietly degrading six months later. This is the highest-value ~60 lines in the issue.

**Mechanism — checked before writing this, because the obvious version is not available.**
There is **no public have-piece bitfield** on `ManagedTorrent` in 8.1.1: `with_chunk_tracker`
and `ChunkTracker::is_piece_have` are `pub(crate)`. What *is* public
(`src/torrent_state/stats.rs:70-79`) is:

```rust
pub struct TorrentStats {
    pub file_progress: Vec<u64>,   // per-file HAVE BYTES — public
    pub progress_bytes: u64,
    pub total_bytes: u64,
    ...
}
```

`file_progress` is byte-granular, not piece-granular, so it cannot by itself distinguish
"first and last piece" from "the first N bytes". Two workable assertions, in preference order:

1. **Range-request the tail through the loopback stream server (preferred).** harbour already
   runs it (`RqbitEngine::stream_server`, FR-61) and already builds
   `/torrents/{id}/stream/{file_id}` URLs. Issue a `Range: bytes=-16384` for the largest video
   file at ~15% overall progress and assert the response completes within a short deadline.
   librqbit's stream **blocks on missing pieces**, so a prompt 206 *is* the proof the last
   piece is present — and it asserts the property in exactly the form the user experiences it
   (seek to the end of a half-downloaded file, it plays). Pair it with a `Range` in the middle
   of the file that is expected to be slow/absent, so the test proves an *ordering*, not just
   that data exists.
2. **Read librqbit's own fastresume state (piece-precise fallback).** harbour configures
   `SessionPersistenceConfig::Json { folder: <state>/engine }`, so the have-bitfield is on
   disk. Parsing it is piece-exact but couples the test to an undocumented on-disk format
   across librqbit versions — use it only if (1) proves too flaky.

Write (1). Do **not** hand Sarthak the bitfield version; it cannot be written against the
public API.

**Step 3 — surface it, do not toggle it (UI track, tiny).** The downloads row for an item with
an open stream gets a `stream` marker, and the help/SPEC copy states that streaming order is
always on. One line of state (`ItemView` already carries everything needed), no new keybind.

**Step 4 — upstream the real toggle (engine track, out-of-repo).** File an issue on
`ikatson/rqbit` asking to expose piece-selection policy on `AddTorrentOptions`, pointing at the
existing `// TODO: make it configurable` at `torrent_state/live/mod.rs:233`. The natural shape
is `enum PieceSelection { Rarest, Sequential }` plus an explicit `file_priorities` override.
Link the issue number from FR-80. Only once that lands do steps 5–6 exist:

**Step 5 (blocked on upstream) —** `AddRequest.sequential: bool` →
`AddTorrentOptions`; `QueueItem.sequential` with `#[serde(default)]`.

**Step 6 (blocked on upstream) —** the add-dialog row from #38 and a downloads-row toggle.

## Files to create / modify

Now:

- `SPEC.md` — FR-80 / FR-81 in §4.4; a pointer from §4.7's FR-60.
- `tests/engine_net.rs` — the characterisation test from step 2.
- `src/engine/rqbit.rs` — a module-level `//!` comment recording the piece-order guarantee and
  the exact upstream file/line it comes from, next to the three mappings already documented
  there. This is precisely the "invariants get a comment at the decision site" rule: the next
  person to bump librqbit needs to see it.
- `src/ui/downloads.rs` — the `stream` marker (step 3).
- `docs/plans/sequential-download.md` — this file; update it when the upstream issue moves.

Deliberately **not** created now: any `sequential` field, any `Action::ToggleSequential`, any
settings row.

## Key APIs / libraries

Everything above is from reading librqbit 8.1.1's source directly (paths and line numbers
cited in *The finding*). The relevant public surface harbour can use today:

- `ManagedTorrent::stream(file_id) -> anyhow::Result<FileStream>`
  (`src/torrent_state/streaming.rs:327`) — registers a `StreamState` whose `queue()` yields
  the pieces covering `[position, position + 32 MiB)`. Those pieces are chained **ahead** of
  natural order at `live/mod.rs:1242`. This is FR-81's mechanism and harbour already gets it
  for free through librqbit's HTTP API in `RqbitEngine::stream_url_for`.
- `AddTorrentOptions.only_files: Option<Vec<usize>>` — already wired via
  `AddRequest.only_files`. Narrowing a torrent to one file is the *only* file-ordering control
  harbour has today, and it is already exposed through the batch picker.
- `TorrentStats.file_progress: Vec<u64>` (`src/torrent_state/stats.rs:72`) — per-file have
  **bytes**, public. The only observability librqbit offers into what has landed;
  `with_chunk_tracker` / `ChunkTracker::is_piece_have` are `pub(crate)`, so there is **no
  public piece bitfield**. This is what forces step 2's Range-request mechanism.

*Format note:* this plan carries an extra "The finding" section ahead of the SPEC reference, a
deliberate deviation from the standard plan template. The librqbit source evidence is the
entire reason the approach looks the way it does, so burying it under "Key APIs" would put the
conclusion before its premise.

**New crates: none.** No `[patch.crates-io]`, no vendored fork.

## Risks / edge cases

- **Rejected approach: the FileStream read-and-discard pump.** It is tempting to
  `handle.stream(file_id)` and spawn a task that reads and throws away bytes to drag the
  32 MiB window forward, simulating whole-file sequential. Reject it, for three reasons:
  (1) the window is *only* 32 MiB (`PER_STREAM_BUF_DEFAULT`,
  `torrent_state/streaming.rs:27`) and advances **only on read**, so without a reader it
  prioritises the first 32 MiB and then stops; (2) with a pump, every byte is read back off
  disk and discarded — real I/O burned to imitate a scheduler harbour does not control;
  (3) it is exactly the band-aid the project rules forbid — a workaround masquerading as a
  feature, in place of an upstream fix. Named here so it is rejected once, in writing, rather
  than re-proposed each sprint.
- **Rejected approach: a UI toggle that toggles nothing.** Shipping
  `[x] sequential download` when the engine has no off switch is a lie in the interface. If a
  reviewer asks for the toggle anyway, the correct answer is step 4.
- **A librqbit upgrade could silently change the order.** That is precisely what step 2's
  characterisation test exists to catch. Without it this issue's "done" is unfalsifiable.
- **First/last-piece-first costs a little throughput.** Chasing the last piece of every file
  early is slightly worse for rare-piece health than pure rarest-first. It is librqbit's
  choice, harbour cannot change it, and for a streaming-oriented client it is the right
  trade — note it in FR-80 rather than pretending it is free.
- **File order is filename-ascending, which is not always episode order.** `Ep 10` sorts
  before `Ep 2`. `RqbitEngine::list_video_files_for` already sorts by name and inherits the
  same quirk. Out of scope here; it belongs to a natural-sort issue of its own.
- **Scope honesty for the issue.** #41 lists four bullets. Two ("first/last piece priority",
  "supports watch while it downloads") are already true and this plan documents + tests them.
  One ("sequential download") is already true within a file and not configurable. One ("UI
  exposes per-torrent toggle") is blocked upstream. Close the issue on that basis; do not
  fabricate the fourth.

## Test strategy

- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — the characterisation test,
  using mechanism (1) above (there is no public bitfield). Add a known multi-piece magnet,
  poll `snapshot()` to ~15% progress, then issue two Range requests against
  `stream_file_url(id, largest_video)`: `bytes=-16384` (the tail) must return 206 inside a
  short deadline, and a mid-file range must not. That asserts the *ordering*, which is the
  actual guarantee. Skip cleanly (not fail) when the env var is unset, matching the existing
  gate.
- **Unit, `src/engine/rqbit.rs`** — a pure reimplementation of `iter_piece_priorities`'
  expected output as a `first_last_then_ascending(range)` helper, asserted against
  `0..4 → [0, 3, 1, 2]` and the degenerate `0..0`, `0..1`, `0..2` cases. This is a *documented
  expectation* of the dependency, so when the integration test is not run the intent is still
  captured in a fast test.
- **Buffer snapshot, `src/ui/tests.rs`** — the `stream` marker renders on an item with an
  active stream and not on one without.
- **No queue unit tests** — nothing in `src/queue.rs` changes.

## Verification

1. `SPEC.md` §4.4 contains FR-80 / FR-81, and `src/engine/rqbit.rs`'s module docs cite
   `librqbit-8.1.1/src/file_info.rs:15` for the guarantee.
2. `HARBOUR_TEST_NET=1 cargo test --test engine_net` passes: at ~15% overall progress, a
   `Range: bytes=-16384` on the largest file returns 206 promptly while a mid-file range does
   not. That is the observable proof that first/last-piece priority is real, and it is the
   first time harbour has ever proven it.
3. `cargo run`, start a large single-file torrent, press `w` while it is at ~5%. The player
   opens and plays from the beginning without waiting for the whole file — the user-visible
   result the issue is actually asking for.
4. An upstream issue exists on `ikatson/rqbit` for the configurable policy, linked from FR-80.
5. `grep -rn "sequential" src/` returns only comments and SPEC references — no dead field, no
   toggle that toggles nothing.
