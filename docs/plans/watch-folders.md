# Watch-folders auto-add
Ref: #51

## Goal
Watch one or more configured directories and automatically enqueue any `.torrent` file dropped
into them, without stealing the user's current screen and without ever adding a half-copied
file.

## The finding that shapes this whole plan

Almost every part already exists. Checked in harbour's own source, 2026-08-16:

**1. `notify` 8.2.0 is already a dependency**, and `src/theme_watch.rs` is a complete,
working template for exactly this: `recommended_watcher` → `std::sync::mpsc` → a detached
`std::thread` that owns the watcher (with the load-bearing comment *"`watcher` must live here:
dropping it would stop event delivery"*), degrading to a loud no-op rather than a panic when
the directory cannot be watched. **No new crate, and no new concurrency pattern.**

**2. The add-a-`.torrent` path is already written and already correct.**
`src/app/actions.rs:505` — `enqueue_torrent(app, path)` reads the file, parses the infohash via
`engine.torrent_info_hash(&bytes)` (loud banner if unparseable), and hands it to `queue.add`,
which already performs **FR-56 duplicate detection**. `src/app/events.rs:483` already reuses it
for drag-and-drop. So auto-add is *"call the function that already exists, from a new trigger"*.

**3. One thing in it must not be reused as-is.** `enqueue_torrent` ends with:

```rust
app.state.screen = Screen::Downloads;   // actions.rs:539
```

That is right for a command-line launch (FR-02/FR-39) and **wrong** for a background auto-add —
it would yank a user out of a search they are typing because a file landed in a folder. Step 3
splits that line out to the caller.

**4. The app loop has no channel for this yet, and must not borrow one.** `src/app/mod.rs:495`
is a `tokio::select!` over crossterm input and `events_rx: UnboundedReceiver<EngineEvent>`.
`EngineEvent` is a **frozen shared type** owned by the engine track, and a watch-folder hit is
not an engine observation. Add a **third arm with its own channel** rather than adding a
variant to the frozen enum. This is the single most important structural decision here.

## SPEC / FR reference

Exists today: **FR-02 / FR-39** (a `.torrent` on the command line is validated and enqueued),
**FR-56** (duplicate infohash focuses the existing item), **FR-51** (config persists),
**FR-55** (crash-safe writes). **Nothing in SPEC describes a watched directory** or harbour
acting on filesystem changes it did not initiate.

**Missing from SPEC — add first, then implement.** Proposed **FR-100 … FR-103**, §4.4:

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then. **The settings-row index claimed below is
> provisional for the same reason**: the parallel batch (`speed-limits`, `share-limits`,
> `protocol-toggles`, `encryption-mode`, and especially `categorized-settings`, which may
> restructure the rows entirely) also adds rows, so the row-collision note below is
> understated — treat it as "coordinate with the whole batch", not just #48/#50.

- **FR-100 (opt-in, multi-folder).** `watch_folders` is a list, empty by default. Each existing
  entry is watched non-recursively for `.torrent` files. A configured folder that does not
  exist is reported once at startup and skipped — never created silently.
- **FR-101 (complete files only).** A file is enqueued only once its size has been stable across
  two consecutive checks and it parses as a `.torrent`. A file still being copied is retried,
  not rejected.
- **FR-102 (handled once, visibly).** A successfully added file is renamed to `<name>.added`
  in place. Harbour never deletes a user's file. An unparseable file is renamed
  `<name>.invalid` with a banner, so it is not retried forever.
- **FR-103 (never steals focus).** An auto-add enqueues the torrent and shows a banner. It does
  not change the current screen, selection, or search input.

## Workstream

**Terminal UI (Ishan)** owns it: the watcher thread, the loop arm, and the settings row all sit
in app-loop territory, and it reuses `enqueue_torrent` unchanged apart from step 3's split.

Shared-type dependencies: **none.** No `EngineEvent` variant (see *The finding* §4), no
`QueueItem` field, no `Engine` method. `AddBytesRequest` and `Engine::torrent_info_hash` are
already frozen and already used by this path.

## Approach

**Step 1 — SPEC (docs only).** FR-100…FR-103.

**Step 2 — the watcher (UI).** New `src/watch_folders.rs`.

> **Naming, deliberately.** `src/watch.rs` is the external-player session and `src/app/watch.rs`
> is watch *mode*. A third module called `watch` anything is a maintenance trap; the plural
> `watch_folders` reads unambiguously against both.

```rust
pub fn spawn_watch_folders(dirs: Vec<PathBuf>, tx: tokio::sync::mpsc::UnboundedSender<PathBuf>)
```

Modelled line-for-line on `theme_watch::spawn_theme_watcher`: `recommended_watcher`, one
`watch(dir, RecursiveMode::NonRecursive)` per configured folder, a detached thread that owns
the watcher, and loud-but-non-fatal degradation. It sends **paths**, not parsed torrents — all
parsing stays on the app loop where the banner lives.

**Step 3 — stop `enqueue_torrent` stealing the screen (UI, tiny).** Split
`app.state.screen = Screen::Downloads` out of `enqueue_torrent` into its two existing callers
(the CLI launch path and the drag-and-drop path), so the function becomes screen-neutral and
FR-103 is satisfied by construction rather than by a flag argument.

**Step 4 — the loop arm (UI).** A third `tokio::select!` arm in `src/app/mod.rs:495`:

```rust
Some(path) = watch_rx.recv() => auto_add_torrent(&mut app, &path).await,
```

plus the matching drain in the existing `try_recv` catch-up block below it, so a burst of
dropped files is absorbed in one frame like the other two sources.

**Step 5 — stability + post-add handling (UI).** `auto_add_torrent` implements FR-101/FR-102:
size-stable check, then `enqueue_torrent`, then rename to `.added` (or `.invalid`), then a
banner naming the torrent. Renaming rather than deleting is the FR-102 promise.

**Step 6 — startup scan (UI).** On boot, scan each configured folder once for pre-existing
`.torrent` files and feed them through the same path. Without this, files dropped while harbour
was closed are invisible forever — the single most likely "it doesn't work" report.

**Step 7 — settings row (UI).** One text row, comma-separated folders, mirroring how the
existing "Custom Trackers" row already stores a `Vec<String>`.

### Settings-row collision (cross-PR)

`ui/settings.rs` hardcodes row indices across four `match` blocks against
`const APP_ROWS: usize = 17`, re-matched in `app/settings.rs::settings_toggle_row`.
**Issues #48, #50 and #51 all add rows.** This plan adds **one** text row before the per-source
block and bumps `APP_ROWS` by 1; the later PRs rebase rather than textually merge.

## Files to create / modify

Create:

- `src/watch_folders.rs` — `spawn_watch_folders`, the `.torrent` filter, the startup scan
  helper, and unit tests.

Modify:

- `src/main.rs` — `mod watch_folders;`
- `src/persist.rs` — `Config.watch_folders: Vec<PathBuf>`, defaulting empty (the container's
  `#[serde(default)]` keeps old configs loading).
- `src/app/mod.rs` — the channel, the `spawn_watch_folders` call at boot, the third `select!`
  arm, and the matching `try_recv` drain.
- `src/app/actions.rs` — screen-neutral `enqueue_torrent` (step 3) + `auto_add_torrent`.
- `src/app/events.rs`, `src/main.rs`/CLI path — set `Screen::Downloads` at the two call sites
  that used to rely on `enqueue_torrent` doing it.
- `src/ui/settings.rs` + `src/app/settings.rs` — the row and its commit arm.
- `src/ui/help.rs` / `README.md` — document the folder and the `.added` convention.

## Key APIs / libraries

**New crates: none.** `notify = "8.2.0"` is already a direct dependency used by
`src/theme_watch.rs`.

- `notify::recommended_watcher(cb) -> Result<RecommendedWatcher>` and
  `Watcher::watch(&Path, RecursiveMode::NonRecursive)` — the exact calls
  `theme_watch.rs:52-65` already makes; `RecommendedWatcher` selects the platform backend
  (ReadDirectoryChangesW on Windows, inotify on Linux, FSEvents on macOS).
- `notify::EventKind::{Create, Modify}` — editors and browsers write via temp-file + rename, so
  match on both, exactly as `theme_watch::handle_theme_event` already does.
- `Engine::torrent_info_hash(&bytes) -> Option<InfoHash>` — `core/types.rs:701`, implemented in
  `rqbit.rs:446` over `librqbit::torrent_from_bytes`. The parse gate for FR-101.
- `Queue::add(AddInput { bytes: Some(..), .. })` — `src/queue.rs:291` routes to
  `Engine::add_bytes(AddBytesRequest)`; FR-56 dedupe already applies.
- `std::fs::rename` — same-directory rename, atomic, for FR-102.

## Risks / edge cases

- **The partial-copy race is the defining bug of this feature.** `notify` fires `Create` the
  instant the file appears, which for a large `.torrent` copied over a network share is well
  before the last byte lands. Parsing then fails and the file is condemned as `.invalid` —
  destroying a perfectly good torrent from the user's point of view. FR-101's size-stability
  check (poll `metadata().len()` twice ~300 ms apart, require equality **and** a successful
  parse) is the mitigation, and it must be in the first PR, not a follow-up.
- **Editors/downloaders write via temp + rename**, so a `Modify` on an existing path is a real
  event, not a duplicate. Debounce by path: collapse repeated events for the same path within a
  short window rather than enqueuing twice.
- **Rejected: deleting the file after adding.** Tempting and tidy; it destroys user data on a
  false positive and is irreversible. FR-102's rename is reversible and self-documenting. Named
  here so it is rejected once, in writing.
- **Rejected: adding an `EngineEvent::WatchFolderHit` variant.** It would be the shortest
  diff — and it would mutate a frozen shared type owned by another workstream for something
  that is not an engine observation. The dedicated channel costs ~6 lines.
- **Watching the download directory is a footgun.** If a user points a watch folder at their
  downloads dir, harbour's own completed `.torrent` files (FR-37 writes to the state dir, but
  users do move files) could loop. The `.added` rename breaks the cycle; additionally refuse to
  watch a folder equal to `config.download_dir` with an explanatory banner.
- **A watch folder on a network/removable mount** may fail to register or vanish mid-session.
  Degrade loudly and keep running, exactly as `theme_watch` does — never panic, never retry in
  a hot loop.
- **Case sensitivity.** Match `.torrent` case-insensitively (`.TORRENT` off a Windows share) —
  `cli.rs:126` and `events.rs:483` already use `to_ascii_lowercase().ends_with(".torrent")`;
  reuse that, do not write a third variant.
- **Read-only folders** make FR-102's rename fail. Report once per path and keep an in-memory
  "already handled" set so the file is not re-added on every event.
- **Duplicate torrents** are already handled by FR-56 — the auto-add should still rename to
  `.added` (it *was* handled) and say "already in the queue" rather than reporting an error.

## Test strategy

- **Unit, `src/watch_folders.rs`** — the `.torrent` extension filter (including `.TORRENT` and
  a file merely *containing* `.torrent` mid-name); the startup scan lists exactly the
  `.torrent` files in a temp dir and ignores `.added`/`.invalid`; a configured folder that does
  not exist yields a warning and no watcher rather than an error.
- **Unit, size-stability** — a fake file whose length changes between the two checks is *not*
  ready; one whose length repeats is. This is FR-101 and it is the highest-value test here.
- **Unit, `src/app/actions.rs`** — `enqueue_torrent` no longer touches `state.screen`
  (a regression guard on step 3, which is easy to undo by accident). The existing test at
  `actions.rs:653` asserting FR-02/FR-39 behaviour must be updated to assert the screen change
  at the *caller*, keeping the CLI guarantee covered.
- **Integration-ish, offline** — using `engine/fake.rs` and a temp dir: write a valid `.torrent`
  into a watched folder, drive the auto-add path, and assert the queue gained one item, the file
  is now `*.added`, and `state.screen` is unchanged (FR-103).
- **No network tests.** Everything here is filesystem + the fake engine.

## Verification

1. `SPEC.md` §4.4 contains FR-100…FR-103.
2. `cargo run`, set a watch folder in settings, then from another terminal
   `cp some.torrent ~/watched/`. Within a second or two: a banner names the torrent, it appears
   in the downloads list, and `~/watched/some.torrent` is now `some.torrent.added`. The
   **screen does not change** — if the search box was focused mid-query, the query is intact.
   That is FR-103, observable.
3. Simulate a slow copy: `head -c 100 some.torrent > ~/watched/partial.torrent` then append the
   rest a second later. The file is added **once**, correctly, and is never marked `.invalid`.
   That is FR-101, the bug this feature lives or dies on.
4. Drop a garbage file named `junk.torrent` → banner, renamed `junk.torrent.invalid`, and it is
   not retried on the next event.
5. Quit harbour, drop a `.torrent` into the folder, relaunch → it is picked up by the startup
   scan (step 6).
6. Drop the *same* torrent twice → FR-56 focuses the existing item, the second file still
   becomes `.added`, and no duplicate row appears.
