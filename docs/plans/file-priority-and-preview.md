# File priority + preview
Ref: #49

## Goal
A torrent content view that lists every file with its size and per-file progress, lets the user
include or skip files on a live torrent, and surfaces a file summary on the downloads row.

## The finding that shapes this whole plan

Read 2026-08-16 from the exact source harbour compiles against,
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/`:

**There are no per-file priority *levels* in librqbit 8.1.1.** The only public per-file control
is inclusion:

```rust
// src/session.rs:1406
pub async fn update_only_files(
    self: &Arc<Self>,
    handle: &ManagedTorrentHandle,
    only_files: &HashSet<usize>,
) -> anyhow::Result<()>
```

`FilePriorities` exists (`src/torrent_state/live/mod.rs:131`) but it is **`pub(crate)`**, it is
built once from filename-ascending order (`live/mod.rs:233`, directly above a literal
`// TODO: make it configurable`), and nothing public can set it. `AddTorrentOptions` has
`only_files` and `only_files_regex` and **no** priority field.

**Therefore the issue's "skip / normal / high" cannot be built as three levels.** Two of the
three would be a UI control that maps to the same engine call — a toggle that toggles nothing,
which is the exact band-aid `docs/plans/sequential-download.md` (#41) already rejected in
writing for the same crate. This plan ships **skip / normal** — two states that are both real
— and records `high` as blocked upstream on the same `ikatson/rqbit` issue #41 files.

**What *is* available, and is better than expected:**

- **`TorrentStats.file_progress: Vec<u64>`** (`src/torrent_state/stats.rs:72`) — public,
  per-file downloaded **bytes**, indexed parallel to `file_infos`. This is what makes a genuine
  per-file progress column possible; harbour currently throws it away in `to_snapshot`.
- **`update_only_files` works on a live torrent** and calls `try_update_persistence_metadata`,
  so a change survives restart without harbour persisting anything extra. `QueueItem.only_files`
  is *already* a persisted field.

## SPEC / FR reference

Exists today: **FR-40** (per-item output folder at enqueue time), and the batch picker
(`src/ui/batch_picker.rs`) already selects files **at add time** via
`AddRequest.only_files` → `AddTorrentOptions.only_files`. **Nothing in SPEC covers changing the
selection after a torrent has started, a content/file view, or per-file progress.**

**Missing from SPEC — add first, then implement.** Proposed **FR-92 … FR-95**, §4.4:

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then.

- **FR-92 (content view).** A selected download opens a content view listing every file with
  index, name, size, and per-file progress. It is read-only for a torrent whose metadata has
  not arrived, which it says rather than rendering an empty list.
- **FR-93 (two real states).** A file is `included` or `skipped`. harbour exposes exactly the
  two states librqbit implements; it does not present priority levels it cannot honour.
  Skipping every file is refused with a banner — that is a delete, not a priority change.
- **FR-94 (live and durable).** Toggling applies to the running torrent immediately and is
  persisted in `QueueItem.only_files`, so it survives a restart.
- **FR-95 (file summary on the row).** The downloads row shows file count and, when a selection
  is active, `n of m files` — so a partially-selected torrent is never mistaken for a stalled
  one at 40%.

## Workstream

**Engine & Foundation (Sarthak)** owns steps 2–3: two additive `Engine` trait methods and the
`EngineStats` change. **Terminal UI (Ishan)** owns steps 4–6.

Shared-type dependencies — **all additive, none breaking:**

- `TorrentFileView` (frozen, `core/types.rs:642`) gains **nothing**. It already carries
  `{ id, name, size_bytes }` and the batch picker already renders it. Per-file progress travels
  separately (see below) so the frozen struct is untouched.
- New `Engine` methods follow the established additive-default pattern that `stream_url`,
  `list_video_files` and `add_bytes` already use — every existing implementor
  (`engine/fake.rs`) keeps compiling with no edit.
- `QueueItem.only_files` **already exists** and is already persisted. No ledger migration.

## Approach

**Step 1 — SPEC (docs only).** FR-92…FR-95.

**Step 2 — engine: list *all* files, not just video (engine).** `RqbitEngine::list_video_files_for`
filters to a video-extension allowlist. The content view needs everything. Add to the `Engine`
trait, additive with a default returning `Vec::new()`:

```rust
fn list_files<'a>(&'a self, _id: &'a str) -> EngineFuture<'a, Vec<TorrentFileView>> { … }
```

Implement it in `rqbit.rs` by reusing the existing `with_metadata` + metadata-grace retry loop
that `list_video_files_for` already contains — factor that loop into one helper taking a filter
predicate, so the two functions cannot drift on retry behaviour.

**Step 3 — engine: per-file progress (engine).** `to_snapshot` currently drops
`stats.file_progress`. Carry it: add `file_progress: Vec<u64>` to `EngineStats`.

> **Note for Sarthak — this touches a frozen type.** `EngineStats` is `Copy` today; a `Vec`
> makes it non-`Copy`, and it is used behind `Option<EngineStats>` in `ItemView` and copied in
> `queue.rs`. Two options, decide before step 3 starts:
> **(a)** add the field and drop `Copy` (a handful of `.clone()`s in `queue.rs`/`ui`), or
> **(b)** keep `EngineStats` untouched and add a separate
> `fn file_progress<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Vec<u64>>` to `Engine`,
> called only while the content view is open.
> **Prefer (b).** It keeps a frozen `Copy` type frozen, and per-file progress is needed by
> exactly one view that is open rarely — paying for it on every poll tick for every torrent is
> the wrong trade. `EngineStats`' own doc comment says it holds volatile per-*torrent* stats;
> per-file arrays do not belong there.

**Step 4 — engine: apply a selection (engine).**

```rust
fn set_only_files<'a>(&'a self, _id: &'a str, _files: &'a HashSet<usize>)
    -> EngineFuture<'a, Result<(), EngineError>> { /* default: Unavailable */ }
```

`rqbit.rs` implements it as `self.session.update_only_files(&handle, files)`. Default is a loud
`EngineError::Unavailable`, matching `add_bytes`' precedent — never a silent success.

**Step 5 — the content view (UI).** `src/ui/content.rs`, a new pure-paint view.
**Reuse `batch_picker.rs` as the template, do not extend it**: the batch picker is an *add-time*
overlay that owns a magnet and a target dir and commits once; the content view targets a *live*
queue item and applies immediately. Same visual language (checkbox column, scrollbar,
`TorrentFileView` rows), different lifecycle. Follow the shared-layout convention the settings
view documents: a `row_kind`-style helper so the view and the key handler agree.

**Step 6 — the row summary and the keybind (UI).** `n of m files` on the downloads row when
`only_files.is_some()`; a keybind on the downloads screen opens the content view; help line.

## Files to create / modify

Create:

- `src/ui/content.rs` — the file list + per-file progress + include/skip view, and its
  `ContentState { open, torrent_id, files, progress, selected, included }`.

Modify:

- `src/core/types.rs` — `Engine::list_files`, `Engine::set_only_files`, and (per step 3's
  decision) `Engine::file_progress`, all additive with defaults + `///` docs.
- `src/engine/rqbit.rs` — implement all three; factor the metadata-grace retry loop into one
  helper shared with `list_video_files_for`.
- `src/engine/fake.rs` — implement them over its in-memory torrents so the UI and queue tests
  work offline.
- `src/ui/mod.rs` — `pub mod content;`, `ContentState` on `AppState`.
- `src/app/actions.rs` — open the view; commit a toggle (engine call + `QueueItem.only_files` +
  `persist(app)`).
- `src/input.rs` — key dispatch while the view owns the keyboard.
- `src/ui/downloads.rs` — the FR-95 summary column.
- `src/ui/help.rs` — the new keybind.

**Not modified:** `src/ui/batch_picker.rs` (different lifecycle), `src/persist.rs`
(`only_files` is already persisted), `src/queue.rs`.

## Key APIs / libraries

**New crates: none.** Everything is librqbit surface already in the tree, plus ratatui widgets
already used by `batch_picker.rs` (`Paragraph`, `Clear`, `Scrollbar`, `ScrollbarState`).

Verified 2026-08-16 against `librqbit-8.1.1` source:

- `Session::update_only_files(&Arc<Self>, &ManagedTorrentHandle, &HashSet<usize>)`
  — `src/session.rs:1406`, public, async, persists metadata. `RqbitEngine` already holds
  `session: Arc<Session>`, so the `self: &Arc<Self>` receiver is satisfied as-is.
- `TorrentStats.file_progress: Vec<u64>` — `src/torrent_state/stats.rs:72`, public.
- `ManagedTorrent::with_metadata(|meta| …)` → `meta.file_infos: Vec<FileInfo>` with
  `relative_filename: PathBuf` and `len: u64` — the same call `list_video_files_for` uses today.
- `AddTorrentOptions.only_files: Option<Vec<usize>>` — the add-time path, already wired.
- The HTTP-API equivalent (`POST /torrents/{id}/update_only_files`,
  `src/http_api/handlers/mod.rs:116`) confirms this is the supported way to change a selection
  on a live torrent, not a private back door.

## Risks / edge cases

- **Rejected: shipping a three-level priority control.** See *The finding*. `high` would map to
  the same `update_only_files` call as `normal`. Ship two states, and add `high` only if the
  upstream issue #41 files (configurable piece-selection policy) lands.
- **Rejected: "high" implemented by opening a hidden stream.** librqbit *does* prioritise pieces
  for open streams (`live/mod.rs:1242`), so one could fake `high` by opening a `FileStream` and
  pumping it. #41's plan already rejected this same trick in writing: the window is only 32 MiB,
  it advances only on read, and it burns real I/O to imitate a scheduler harbour does not
  control. Do not resurrect it here.
- **Skipping every file.** `update_only_files` with an empty set is meaningless (and may error).
  Refuse in the UI with a banner suggesting remove — FR-93.
- **Already-downloaded bytes are not reclaimed.** Skipping a file that is 80% downloaded stops
  further work but does not delete anything, and `progress` will *drop* as the denominator
  shrinks. That looks like a bug to a user. Say it in the view's footer.
- **Metadata may not have arrived.** A magnet-added torrent has no `file_infos` for the first
  seconds; `with_metadata` returns `Err`. Reuse the existing `METADATA_GRACE` retry and render
  "waiting for metadata" — never an empty list that reads as "no files" (FR-92).
- **Index stability.** File indices are metadata order and are stable for a given infohash, so
  the persisted `only_files` set stays valid. It is **not** the display order — the view sorts
  by name for humans, so it must key off `TorrentFileView.id`, never the row position. This is
  the most likely off-by-one in the feature.
- **`only_files` is `HashSet<usize>` in harbour and `Vec<usize>` in `AddTorrentOptions`** —
  the conversion already exists in `rqbit.rs::add`; keep it in that one place.
- **FR-67 file size.** `src/ui/downloads.rs` is 732 LOC and already past the 700 review line.
  Add the summary column there but do not grow it further; if the change pushes it, split the
  row renderer rather than adding to the bottom.

## Test strategy

- **Unit, `src/engine/fake.rs` + `src/queue.rs`** — `set_only_files` updates the fake engine's
  selection; a toggle round-trips through `QueueItem.only_files` and survives a
  `save_ledger`/`load_ledger` cycle (the persistence path already exists, so this is a
  regression guard on FR-94).
- **Unit, `src/ui/content.rs`** — the row-kind/index helper: display order is name-sorted while
  toggles key off `TorrentFileView.id`. Feed a deliberately adversarial list (`Ep 10` before
  `Ep 2`) and assert the toggled index is the file's real index, not the row position.
- **Buffer snapshot, `src/ui/tests.rs`** — content view renders sizes, per-file progress and
  checkboxes; the "waiting for metadata" state renders instead of an empty list; the downloads
  row shows `3 of 12 files` when a selection is active and no summary when it is not.
- **Integration, gated `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — add a real multi-file
  magnet, wait for metadata, call `set_only_files` with one index, and assert the other files'
  `file_progress` entries stay at 0 while the selected one advances. This is the only way to
  prove the engine actually honoured the call rather than harbour just recording it.

## Verification

1. `SPEC.md` §4.4 contains FR-92…FR-95, and FR-93 states plainly that only two states exist and
   why.
2. `cargo run`, start a multi-file torrent (a season pack), open the content view: every file
   is listed with size and its own progress — including non-video files, which
   `list_video_files` hides today.
3. Skip two episodes → their progress bars **stop advancing within one poll tick** while the
   rest continue. That is the user-visible proof the engine honoured it, not just the UI.
4. Quit and relaunch → the same two files are still skipped (read back from
   `~/.harbour/downloads.json`'s `only_files`), and the downloads row reads `10 of 12 files`.
5. Attempt to skip every file → banner, no engine call.
6. `grep -rn "high" src/ui/content.rs` returns nothing — no priority level that does not exist.
