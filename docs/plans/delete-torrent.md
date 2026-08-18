# Delete torrent (+ delete files)
Ref: #39

## Goal
`x` forgets a torrent; `shift+X` offers to delete its files too — behind one confirm overlay
that refuses when another item or a live watch session still owns those files.

## SPEC / FR reference

Exists today:

- **FR-41** — all download mutations (enqueue, promote, pause, resume, remove) are applied
  through the queue, never directly on the engine.
- **FR-48** — `downloads.json` is the ledger; removal must rewrite it.
- `Engine::remove(id, delete_files)` is already on the frozen trait
  (`src/core/types.rs:661`) and its rustdoc already says *"`delete_files` is destructive and is
  never the default anywhere in the UI"*. `RqbitEngine::remove` forwards to
  `session.delete(torrent, delete_files)` and treats "already gone" as success.
  `Queue::remove(id, delete_files)` forwards to the engine, drops the item, and re-promotes.
- `Action::Remove` is bound to `x` / `Delete` on the downloads screen and calls
  `remove_selected` in `src/app/actions.rs:281`, which hard-codes `false` with the comment
  *"deleting someone's data needs a deliberate, separate confirmation."*

**So the plumbing is complete end to end. What is missing is the confirmation and the
guard — and neither is in SPEC.** Proposed new **FR-76 … FR-79** in §4.4:

- **FR-76** `x` (forget) and `shift+X` (forget + delete files) both open a confirm overlay
  naming the item and, for `shift+X`, the exact directory that will be deleted. The overlay
  defaults to the non-destructive choice; `y`/Enter confirms, `n`/Esc cancels.
- **FR-77** Delete-files is refused, with a banner explaining why, when another ledger item's
  `dir` is the same directory or an ancestor of it, or when a watch session is streaming the
  item. The item is still forgotten; only the file deletion is skipped.
- **FR-78** A delete never removes the `.torrent` cache entry from FR-37 — re-adding is how a
  user recovers, and a cached metainfo makes that a verify rather than a re-download.
- **FR-79** Deleting a `missing` item (FR-45) skips the file deletion silently: the files are
  already gone, so there is nothing destructive to confirm.

## Workstream

- **UI (Ishan)** — the confirm overlay, its keymap, the `shift+X` binding, snapshots.
- **Engine & Foundation (Sarthak)** — the shared-files guard in `src/queue.rs`, because it
  reads the ledger and is the thing that must be right.
- **Indexer (Dhruv)** — nothing.

Shared-type dependencies: **none.** `Engine::remove` already takes `delete_files` and
`QueueItem.dir` already exists. This feature adds no field to any frozen type, which is why it
should land before #38's dialog work.

## Approach

Three PRs.

**Step 1 — SPEC (docs only).** FR-76…FR-79 into `SPEC.md` §4.4, plus `shift+X` in the keybind
table. `src/ui/help.rs::BINDINGS` must gain it in the same PR or
`every_action_the_keymap_can_produce_is_documented_in_the_help` fails.

**Step 2 — the guard (engine track, no UI).**
`Queue::delete_files_blocked_by(&self, id) -> Option<String>` — returns the human-readable
reason, `None` when it is safe. Rules, in order:

1. The item is `Missing` → `Some("the files are already gone")` (FR-79, skip silently).
2. Any *other* ledger item whose `dir` equals `item.dir`, or whose `dir` is a descendant of
   it, or which `item.dir` is a descendant of → `Some("<other name> also lives in <dir>")`.
   Compare canonicalised paths via `paths::expand_home` first, then `Path::starts_with`, so
   `~/dl` and `/home/u/dl` are the same directory.
3. Otherwise `None`.

`Queue::remove` gains an assertion: when `delete_files` is true and the guard returns `Some`,
it removes with `false` and returns the reason to the caller. **The queue is the enforcement
point, not the UI** — a future headless caller must not be able to route around it.

**Step 3 — the confirm overlay (UI track).** `src/ui/confirm.rs`: a small modal with a title,
a body, and two rows. State is `ConfirmPrompt { open, title, body, destructive, on_confirm }`
where `on_confirm` is a plain enum (`ConfirmAction::Remove { id, delete_files }`), **not** a
closure — the app loop owns mutations and a boxed `FnOnce` on `App` would fight that.

## Files to create / modify

Create:

- `src/ui/confirm.rs` — `ConfirmPrompt` state + pure `draw()`. Centred modal, same
  `Clear` + bordered-block shape as `src/ui/player.rs`.

Modify:

- `SPEC.md` — FR-76…FR-79 in §4.4; keybind table gains `shift+X`.
- `src/queue.rs` — `delete_files_blocked_by`; `remove` consults it and reports.
- `src/input.rs` — `Action::RemoveWithFiles`, `ConfirmYes`, `ConfirmNo`;
  `FocusFlags.confirm_open`; a modal branch placed directly after `help_open` (a confirm must
  out-rank the settings/folder overlays — you can open a confirm from any of them, and it is
  the destructive one). `KeyCode::Char('X') => Action::RemoveWithFiles` in the
  `Screen::Downloads` arm.

  **Overlay-precedence note for the #38 merge.** `docs/plans/add-torrents.md` step 3 also
  claims the slot "after `help_open`". The final order in `map_with_focus` is:
  `help_open` → **`confirm_open`** → `add_dialog_open` → episode/batch → picker → settings →
  folder. The confirm outranks the add dialog because it is the destructive one and the only
  one that can be raised over another overlay. Whichever of #38/#39 merges second adopts this
  order rather than re-litigating it.
- `src/app/actions.rs` — `remove_selected` opens the confirm instead of removing;
  `confirm_remove(app, delete_files)` does the work and banners the guard's reason when the
  file deletion was skipped.
- `src/app/mod.rs` — `confirm: ConfirmPrompt` on `App`; the `draw` overlay call; the
  `FocusFlags` wiring. The confirm draws **after** settings/picker so it is on top.
- `src/app/events.rs` — dispatch the three new actions.
- `src/ui/mod.rs` — `pub mod confirm;` and the re-export.
- `src/ui/help.rs` — `shift+X` in `BINDINGS`.

## Key APIs / libraries

**librqbit 8.1.1** — `Session::delete(TorrentIdOrHash, delete_files: bool)`; already wired in
`src/engine/rqbit.rs:497-513`. Read at
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/src/session.rs:1233-1313` on
2026-08-16. What it actually does when `delete_files` is true:

```
remove_files_and_dirs(&metadata.file_infos, &storage);
if removed.shared().options.output_folder != self.output_folder {
    storage.remove_directory_if_empty(Path::new(""))   // only if empty
}
```

Two scoping facts the guard depends on, both confirmed from that source:

1. **Deletion is per-file, driven by the torrent's own `file_infos`.** librqbit does not
   `rm -rf` the output folder, so a sibling torrent's files in the same directory are not
   removed by librqbit itself.
2. **The directory is removed only when it is empty** *and* only when the torrent used a
   per-item output folder (the `shift+D` case). An overlapping sibling's files keep the
   directory non-empty, so it survives.

So the guard is **not** protecting against librqbit nuking a directory. It protects against
the harbour-level case where two torrents genuinely share *files*: the same release added
twice under two infohashes, or a season pack plus a single episode pointing at the same
directory — where librqbit's per-file deletion removes exactly the bytes the *other* item is
still seeding, and FR-45's file-gone detector then flags that other item `missing`. That is
the concrete harm, and directory overlap is the cheap, lexical proxy for it that a user can
actually create from the TUI via `shift+D`. State that scope honestly in the PR rather than
claiming a stronger guarantee than the check delivers.

**ratatui 0.30.2** — the modal is `Clear` + `Block::bordered()` + a two-line `Paragraph`,
exactly the widgets `src/ui/player.rs` and `src/ui/help.rs` already use. No new widget, no
version work.

**New crates: none.**

## Risks / edge cases

- **`y` is a letter.** With the confirm open it must mean yes, and the modal must own every
  key so it cannot leak to the screen underneath. Default the selection to **No** so a stray
  Enter is never destructive.
- **Overlay ordering.** The confirm can be raised while settings or the folder prompt is up.
  It has to be checked before them in `map_with_focus` and drawn after them in `draw`, or the
  user types into an invisible prompt. This asymmetry is easy to get backwards — assert both
  in tests.
- **Selection drift.** `refresh_downloads` clamps `selected`, but the confirm holds an *id*,
  not an index, so a poll tick between opening and confirming cannot delete the wrong row.
  Never store the index.
- **Stale confirm.** If the item vanishes (completed-clear, engine error) while the confirm is
  up, `Queue::remove` returns `EngineError::NotFound`. Treat that as a banner, not a panic.
- **Symlinks and `~`.** `Path::starts_with` is lexical. Run both paths through
  `paths::expand_home` first; do not `canonicalize` (it fails on a missing dir, and a
  `missing` item's dir is exactly that).
- **Active watch session.** `app.watch.is_some()` plus `state.now_playing.id == id` blocks
  file deletion — deleting the file under a playing mpv gives a confusing player error rather
  than an honest refusal.
- **`clear_completed` already exists** and removes with `false`. Leave it alone; it is
  non-destructive by design and is not what this issue asks about.

## Test strategy

- **Unit (engine track), `src/queue.rs`.** `delete_files_blocked_by`:
  - two items in the same `dir` → both blocked, reason names the other item;
  - item B in `~/dl/showname` and item A in `~/dl` → blocked in both directions;
  - two items in genuinely separate dirs → `None`;
  - a `Missing` item → blocked with the already-gone reason;
  - `~/dl` vs the expanded absolute form of the same path → blocked (the `expand_home`
    regression).
- **Unit (engine track).** `Queue::remove(id, true)` against `FakeEngine` when the guard trips:
  the item leaves the ledger, `FakeEngine` records `delete_files == false`, and the returned
  reason is non-empty. This is the "queue is the enforcement point" test — it must not go
  through the UI.
- **Keymap (UI track), `src/input.rs`.** `shift+X` on `Screen::Downloads` is
  `RemoveWithFiles` and on `Screen::Search` is `Action::Type('X')` (the "Dune" regression
  class). With `confirm_open`: `y`/Enter → `ConfirmYes`, `n`/Esc → `ConfirmNo`, Tab →
  `Action::None`, `ctrl+c` → `Quit`. With `confirm_open` **and** `settings_open`, the confirm
  wins.
- **Buffer snapshot (UI track), `src/ui/tests.rs`.** Render the destructive confirm into a
  `TestBackend` and assert the directory path is visible in the body and the highlighted row
  is **No**.
- **Integration, `HARBOUR_TEST_NET=1`.** Add a tiny magnet, wait for a file on disk, remove
  with `delete_files: true`, assert the file is gone and the session no longer holds the
  torrent.

## Verification

`cargo run` with two downloads:

1. Select one, press `x`. A confirm appears naming it, with **No** highlighted. Esc — nothing
   changed.
2. Press `x`, then `y`. The row disappears; the files are still on disk
   (`ls ~/Downloads/<name>`); `~/.harbour/downloads.json` no longer lists it.
3. Use `shift+D` to download two different torrents into the *same* folder. Select one, press
   `shift+X`, confirm. The row goes; the banner reads
   `files kept — <other name> also lives in <dir>`; **both** sets of files are still on disk.
4. Download one torrent into its own folder, `shift+X`, confirm. The row goes and the
   directory's files are gone. `~/.harbour/cache/torrents/<hash>.torrent` still exists
   (FR-78), so re-adding it verifies rather than re-downloads.
