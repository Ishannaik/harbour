# Watch-while-downloading streaming
Ref: #52

## Goal
Make starting playback on a still-downloading torrent a **supported, specified, honest** path:
allow it explicitly, tell the user when the swarm cannot sustain it, and stop the one code path
that currently launches a player against a stream that is not ready.

## The finding that shapes this whole plan

The issue frames this as *"depends on sequential download + first/last piece priority"*. Both
dependencies are **already satisfied**, and one of them is already documented in this repo.

**1. The piece-order dependency does not need building.** `docs/plans/sequential-download.md`
(#41) established, from the pinned crate source, that librqbit 8.1.1 already requests each
file's first and last piece first and then the remainder in ascending order
(`librqbit-8.1.1/src/file_info.rs:15-35`), and that an open stream's pieces are chained *ahead*
of natural order (`torrent_state/live/mod.rs:1242`). Re-verified 2026-08-16. **This plan must
not re-plan that work** — it depends on #41's proposed FR-80/FR-81 and adds the user-facing
half.

**2. The plumbing already exists and is already status-blind.** `src/app/watch.rs:99`
`start_watch` does **not** check `QueueStatus` — it resolves the selected item, calls
`engine.stream_url(&id)`, probes, and launches. `RqbitEngine::stream_url_for`
(`src/engine/rqbit.rs:100`) waits out a metadata grace period and returns a loopback
`/torrents/{id}/stream/{file_id}` URL served by librqbit's own HTTP API. So watching a
downloading torrent **largely works today** — it is simply undocumented, unspecified, untested,
and unguarded.

**3. The guard that is supposed to protect it does nothing.** `src/app/watch.rs:311-331`:

```rust
for _ in 0..6 {
    if let Ok(resp) = client.get(url).header("Range", "bytes=0-1024").send().await {
        let status = resp.status();
        if status.is_success() || status == StatusCode::PARTIAL_CONTENT { return Ok(()); }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
}
Ok(())          // <-- every failure path lands here
```

`probe_stream` returns `Ok(())` after all six attempts fail. Its own doc comment claims it
*"turns that into our own banner with the real reason"*, and all three call sites are written as
`if let Err(reason) = probe_stream(&url).await { app.warn(...) }` — **but that `Err` is
unreachable except when the HTTP client itself cannot be constructed.** A dead swarm therefore
still launches the player, which hangs on a baffling *"unable to open MRL"*: precisely the
failure the function was written to prevent, and precisely the "silent fallback" the project
rules forbid. **This is the single highest-value fix in the issue**, and it is ~3 lines.

**4. A second, smaller mismatch, same file.** `stream_url_for`'s doc says *"the stream URL for
`id`'s largest video file"* (`rqbit.rs:96`) but the code sorts by filename and takes
`videos.first()` — the alphabetically-first video, not the largest. For a season pack with a
`sample.mkv`, those differ, and the user gets the sample. Fix the code to match the doc (largest
wins; it is the reliable "the feature presentation" heuristic), or fix the doc — but not
silently leave them disagreeing.

## SPEC / FR reference

Exists today, and is **wrong for this feature as written**:

- **FR-57** — *"`w` on a **playable (seeding/complete)** item…"*. The parenthetical excludes
  the entire feature. The code already ignores it, so SPEC and behaviour are out of sync today.
- **FR-58** — Range serving, *"seeking works on complete and partially-downloaded-but-watchable
  files"*. Already supports the intent.
- **FR-60** — *"Watch mode only activates while the swarm has the requested piece ranges;
  insufficient data shows an error banner instead of a broken stream."* This is exactly the
  guarantee finding §3 shows is **not currently enforced**.
- **FR-61** — loopback-only binding. Unchanged.

**SPEC changes needed — amend first, then implement.** Proposed:

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then. The FR-57 *amendment* is not affected —
> that is an existing, already-assigned FR.

- **Amend FR-57**: replace *"playable (seeding/complete)"* with *"any item whose metadata has
  arrived, including one still downloading"*.
- **New FR-104 (readiness gate).** Playback launches only after the stream endpoint has served a
  real `206 Partial Content` for the opening byte range within a bounded deadline. A timeout is
  an error banner naming the reason; harbour never launches a player against an unproven
  stream. *(This is FR-60 made enforceable — and testable.)*
- **New FR-105 (honest streaming status).** While watching a still-downloading item, the
  now-playing view shows the item's download progress and current speed, and warns when the
  swarm is too slow to sustain playback. Harbour shows only measured facts; it does not
  fabricate a buffer-percentage it cannot compute.

**Depends on #41's FR-80/FR-81** (piece-order guarantee). If #41 has not landed, this plan's
step 2 test is still valid but should cite FR-60 alone.

## Workstream

**Terminal UI (Ishan)** owns steps 1, 3, 5, 6 — `src/app/watch.rs` and the now-playing view are
app-loop/UI territory. **Engine & Foundation (Sarthak)** owns steps 2 and 4 (the `rqbit.rs`
file-selection fix and the network-gated test).

Shared-type dependencies: **none new.** `Engine::stream_url` / `stream_file_url` /
`list_video_files` are already on the frozen trait with additive defaults, and `NowPlaying`
(`src/ui/mod.rs:215`) is a UI-owned struct — extending it needs no engine sign-off.

## Approach

**Step 1 — SPEC (docs only).** Amend FR-57's parenthetical; add FR-104/FR-105. Note in FR-57's
margin that the code already behaved this way — this is SPEC catching up to reality, which is
worth recording so the change is not mistaken for a behaviour change.

**Step 2 — make the readiness gate real (UI, ~10 lines, do this first).** `probe_stream`
returns `Err` when the loop exhausts, with the last observed status (or the transport error) in
the message. Additionally treat a non-206 success as suspicious: librqbit's stream endpoint
answers `206` to a `Range` request, so a bare `200` means the range was ignored and seeking will
not work (FR-58). This turns three already-written but currently-dead `Err` arms into live
error paths — no new call sites.

**Step 3 — allow it explicitly and say so (UI).** `start_watch` gains no status gate (it never
had one), but the *help text and the banner copy* change: a `Queued` item with no metadata yet
gets "waiting for metadata — try again in a moment" rather than the current generic
"the swarm cannot stream it yet". Distinguish the three real cases: no metadata, no video file,
and not-enough-data — they have different user actions.

**Step 4 — pick the right file (engine).** Resolve finding §4: select the **largest** video file
in `stream_url_for`, matching the existing doc comment, and add a `sample`/`extras` de-prioritise
only if the largest-file rule proves insufficient. Keep `list_video_files_for`'s name-sort for
the *episode picker* (a list humans read) — the two functions want different orders, which is
worth a comment at the decision site.

**Step 5 — honest status while streaming (UI).** Extend `NowPlaying` with the fields the
now-playing view needs, filled from the existing poll (`ItemView` already carries progress,
speed and ETA — no new engine call):

- download progress and speed for the streamed item,
- a "swarm may be too slow" warning derived from **measured** speed versus the file's average
  bitrate (`size_bytes / duration`) — and only when a duration is known. Harbour has no
  demuxer, so where duration is unknown, show speed and progress and **no** prediction.

**Step 6 — buffer-ahead, only if it can be measured honestly (UI, optional).**
`TorrentStats.file_progress` (`librqbit-8.1.1/src/torrent_state/stats.rs:72`, public) gives
per-file downloaded **bytes** — not *which* bytes, and there is **no public piece bitfield**
(`ChunkTracker::is_piece_have` is `pub(crate)`, established in #41's plan). So a true
"buffered up to HH:MM" readout is **not computable** against 8.1.1. Ship step 5's measured
facts; do not ship a fake buffer bar. If a real one is wanted, it needs the same upstream
change #41 already files for.

## Files to create / modify

Modify only — **no new modules**:

- `SPEC.md` — amend FR-57; add FR-104/FR-105 in §4.7.
- `src/app/watch.rs` — `probe_stream` returns a real `Err` (step 2); the three-way banner copy
  (step 3). This is the core of the change and it is small.
- `src/engine/rqbit.rs` — largest-video-file selection + the comment explaining why it differs
  from `list_video_files_for`'s name-sort (step 4).
- `src/ui/mod.rs` — extra `NowPlaying` fields (progress, speed, warning).
- `src/ui/now_playing.rs` — render them (146 LOC today, comfortably within FR-67).
- `src/app/mod.rs` — refresh those fields from the existing poll while `Screen::NowPlaying`.
- `src/ui/help.rs` — state that `w` works on a downloading item.
- `tests/engine_net.rs` — step 2's gated test.
- `docs/plans/sequential-download.md` — cross-link this plan from its step 3.

## Key APIs / libraries

**New crates: none.** `reqwest` (already present, `stream` feature enabled) does the probe;
librqbit's HTTP API already serves the stream.

Verified 2026-08-16 against `librqbit-8.1.1` source and harbour's own:

- `librqbit::http_api::HttpApi::make_http_api_and_run(listener, None)` — already started lazily
  on loopback with a random port by `RqbitEngine::stream_server` (`rqbit.rs:78`), satisfying
  FR-61. Unchanged by this plan.
- `GET /torrents/{id}/stream/{file_id}` — librqbit's Range-serving stream endpoint
  (`src/http_api/handlers/`). Blocks on missing pieces and registers a `StreamState` whose
  32 MiB read-ahead window is chained ahead of natural piece order
  (`torrent_state/streaming.rs`, `live/mod.rs:1242`) — this *is* the watch-while-downloading
  mechanism, and harbour already gets it for free.
- `ManagedTorrent::with_metadata(|meta| meta.file_infos)` — `FileInfo { relative_filename, len }`
  for step 4's largest-file selection.
- `TorrentStats.file_progress: Vec<u64>` — public per-file have-**bytes**; explicitly *not*
  sufficient for a buffer-ahead readout (see step 6).
- `reqwest::StatusCode::PARTIAL_CONTENT` — the 206 that FR-104 requires as proof of readiness.

## Risks / edge cases

- **The dead-`Err` bug is a regression risk in both directions.** Making `probe_stream` return
  `Err` turns three currently-unreachable arms live at once. A too-aggressive deadline would
  start refusing streams that work today (six attempts × 300 ms ≈ 1.8 s is short for a cold
  swarm). Raise the budget to ~5 s while making it a real gate, and tune with the step-2 test.
  This is the one change most likely to be *felt* by an existing user.
- **A slow swarm is not a broken swarm.** FR-104 gates on the *opening* range only. Mid-playback
  starvation is the player's problem and shows as buffering; harbour must not kill the session
  for it. FR-105's warning is advisory, never an auto-stop.
- **Rejected: a synthetic buffer bar.** Estimating "buffered 45 s ahead" from
  `file_progress` bytes assumes downloaded bytes are contiguous from the read position, which
  first/last-piece-first ordering makes *false* by construction. It would look great and lie.
  Rejected in writing; revisit only with an upstream piece-bitfield API.
- **Rejected: pumping a `FileStream` to force read-ahead.** Already rejected in #41's plan for
  the same crate and the same reasons (32 MiB window, advances only on read, burns real I/O).
  Do not re-propose it here under a different name.
- **`w` on a `Queued` item with no metadata** is now an expected, common case, not an error.
  The metadata grace in `stream_url_for` is 8 s (`METADATA_GRACE`); a magnet with a cold swarm
  can exceed it. Banner must say "waiting for metadata", not "cannot stream".
- **Seeking backwards past the read-ahead window** re-blocks the stream. Expected librqbit
  behaviour; FR-58's promise is that seeking *works*, not that it is instant.
- **Ephemeral watch-now (2.3) shares every one of these paths** (`launch_ephemeral_session`
  calls the same `probe_stream`). Verify both flows after step 2 — the stream-and-delete
  cleanup in `end_watch` must still run when the probe now legitimately fails *before* a
  session exists, or a failed watch-now leaks a cache dir. **Check this explicitly**: today the
  probe never fails, so that path has never executed.
- **Largest-file selection changes behaviour** for existing users with season packs. It is a
  fix (doc-matching), but call it out in the PR.

## Test strategy

- **Unit, `src/app/watch.rs`** — the probe's status classification as a pure helper
  (`fn probe_outcome(status) -> Result<(), String>`): 206 → ok, 200 → "range ignored", 404/500 →
  the status in the message. Extracting it makes the FR-104 decision testable without a server.
- **Unit, `src/engine/rqbit.rs`** — largest-video-file selection over a synthetic `file_infos`
  list including a small `sample.mkv` that sorts alphabetically first: assert the feature file
  wins. This is finding §4, pinned.
- **Unit, exhaustion returns `Err`** — a probe pointed at a closed port returns `Err` rather
  than `Ok(())`. One assertion; it is the regression guard for the entire finding §3 bug and it
  would have caught it.
- **Buffer snapshot, `src/ui/tests.rs`** — now-playing renders progress + speed for a
  downloading item; renders the slow-swarm warning when speed is low; renders **no** buffer bar
  (a negative assertion, guarding step 6's decision); the three distinct banners for
  no-metadata / no-video / not-ready.
- **Integration, gated `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — add a real magnet, and as
  soon as metadata lands (well before completion) assert `stream_url` yields a URL whose
  `Range: bytes=0-1024` returns **206** within the FR-104 deadline. That is watch-while-
  downloading proven end to end, and it is the test the feature currently lacks entirely.
  Pairs with #41's tail-range test rather than duplicating it.

## Verification

1. `SPEC.md` FR-57 no longer says "seeding/complete"; FR-104/FR-105 exist in §4.7.
2. `cargo run`, start a large well-seeded single-file torrent, press `w` at **~2%** progress →
   the player opens and plays from the start. The now-playing view shows a live download
   percentage and speed that visibly advance while playing. That is the feature, user-visible.
3. Seek to ~50% in the player → playback resumes after a pause (FR-58), and the now-playing
   view does not claim anything it cannot measure.
4. **The bug fix, demonstrated:** press `w` on an item whose swarm is dead (add a magnet with no
   seeders). Today the player launches and hangs on "unable to open MRL"; after this change a
   harbour banner names the reason within ~5 s and **no player process starts**. Confirm with
   `ps`/Task Manager that no mpv/VLC was spawned.
5. Watch-now (2.3) on a dead magnet fails the same way **and** leaves no directory behind under
   `~/.harbour/cache/` — the leak check from Risks.
6. On a season pack, `w` streams the feature file, not `sample.mkv`.
7. `HARBOUR_TEST_NET=1 cargo test --test engine_net` passes the new mid-download 206 assertion.
