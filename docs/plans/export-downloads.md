# Export finished downloads / .torrent
Ref: #42

## Goal
From the downloads screen: copy a finished download's files to a folder you pick, save its
`.torrent` beside them, and put its path on the clipboard — without moving or unseeding the
original.

## SPEC / FR reference

Exists today:

- **FR-37** — *"When torrent metadata arrives, the `.torrent` bytes are saved to
  `<state>/cache/torrents/<hash>.torrent`."* Implemented in
  `RqbitEngine::cache_metadata_if_new` with an atomic temp-file-plus-rename write, keyed by
  `paths::torrent_cache_file(root, hash)`. `RqbitEngine::torrent_bytes(hash)` reads the same
  bytes live from librqbit metadata.
- **FR-38** — completed items drop into "recently downloaded" (`Queue::completed()` →
  `CompletedItem`).
- **FR-52** — the cache layout, including `cache/torrents/`.
- **FR-55** — all persistence writes are crash-safe (temp file + atomic rename on the same
  filesystem). `write_atomic` in `src/engine/rqbit.rs` is the existing implementation.
- **FR-29 / FR-40** — the `FolderPrompt` / `FolderPromptMode` overlay, which is the exact UI
  shape an export target needs (`src/ui/mod.rs`, `src/app/settings.rs`).
- `o` on the downloads screen already opens the containing directory in the file manager
  (`open_selected_item`, `src/app/actions.rs:554`).

**So the `.torrent` half is already on disk — this feature is mostly plumbing to it.** Missing
from SPEC: exporting anything, and clipboard copy. Proposed **FR-82 … FR-85** in §4.6:

- **FR-82** `e` on a finished item opens the folder prompt in export mode. Committing copies
  the item's files into `<target>/<item name>/`, leaving the originals in place so seeding is
  never interrupted.
- **FR-83** The export also writes `<target>/<item name>.torrent` from the FR-37 cache,
  falling back to the engine's live metadata when the cache entry is absent. A missing
  `.torrent` does not abort the file export; it banners.
- **FR-84** Export never overwrites: an existing destination file is skipped and counted, and
  the result banner reports `copied N, skipped M`. Copies are crash-safe per FR-55.
- **FR-85** `y` copies the selected item's directory path to the clipboard and also shows it
  in a banner, so the path is recoverable by hand when the terminal does not support OSC 52.
  `shift+Y` copies one line per contained file.

## Workstream

- **UI (Ishan)** — the `FolderPromptMode::Export` variant, the `e` / `y` / `shift+Y`
  bindings, the progress and result banners, snapshots.
- **Engine & Foundation (Sarthak)** — `src/core/export.rs` (the recursive copy + the
  `.torrent` resolution) and the `Engine::torrent_bytes` trait addition, since both are
  load-bearing and one touches the frozen trait.
- **Indexer (Dhruv)** — nothing.

Shared-type dependencies: one **additive** `Engine` trait method,
`fn torrent_bytes(&self, _id: &str) -> Option<Vec<u8>> { None }`. `RqbitEngine` already has
this exact method as an inherent fn (`src/engine/rqbit.rs:321`) — it just is not on the trait,
so the app cannot reach it through `Arc<dyn Engine>`. Promoting it with a default impl follows
the `stream_url` / `set_speed_limits` precedent and keeps `FakeEngine` compiling.

## Approach

Four PRs.

**Step 1 — SPEC (docs only).** FR-82…FR-85 into §4.6; `e`, `y`, `shift+Y` into the keybind
table and `src/ui/help.rs::BINDINGS` in the same PR (the
`every_action_the_keymap_can_produce_is_documented_in_the_help` test enforces it).

**Step 2 — `src/core/export.rs` (engine track, no UI).** Pure functions, fully unit-testable
with temp dirs and no engine:

```
pub struct ExportReport { pub copied: usize, pub skipped: usize, pub bytes: u64 }

/// Recursively copies `src` into `dst`, never overwriting. Returns what happened.
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<ExportReport>

/// The `.torrent` for `hash`: the FR-37 cache first, then `engine.torrent_bytes`.
pub fn torrent_metadata(state_root: &Path, hash: &str, engine: &dyn Engine) -> Option<Vec<u8>>
```

`write_atomic` moves out of `src/engine/rqbit.rs` into `src/core/paths.rs` so the export and
the FR-37 cache share one crash-safe writer instead of growing a second one. That is a small
mechanical refactor and belongs in this PR, not a later one.

**Step 3 — `Engine::torrent_bytes` on the trait (engine track).** Default `None`;
`RqbitEngine`'s existing inherent method becomes the impl. Three lines plus a `FakeEngine`
impl that returns whatever bytes it was added with, so step 2's tests can run against the
fake.

**Step 4 — the UI (UI track).** `FolderPromptMode::Export` joins `DownloadTo` and
`SetDefault`; `commit_folder_prompt` grows a third arm that calls `copy_tree` +
`torrent_metadata`. `Action::Export`, `Action::CopyPath`, `Action::CopyContentPaths` in
`src/input.rs`, bound on `Screen::Downloads` only.

## Files to create / modify

Create:

- `src/core/export.rs` — `copy_tree`, `torrent_metadata`, `ExportReport`.
- `src/core/clipboard.rs` — `copy(text) -> io::Result<()>`, one `execute!` of crossterm's
  `CopyToClipboard`.

Modify:

- `SPEC.md` — FR-82…FR-85 in §4.6; keybind table.
- `Cargo.toml` — `crossterm` gains the `osc52` feature (see below).
- `src/core/mod.rs` — `pub mod export; pub mod clipboard;`.
- `src/core/paths.rs` — `write_atomic` moves here from `src/engine/rqbit.rs`.
- `src/engine/rqbit.rs` — use `paths::write_atomic`; drop the local copy;
  `torrent_bytes` becomes the trait impl.
- `src/core/types.rs` — `Engine::torrent_bytes` with a `None` default (engine track).
- `src/engine/fake.rs` — the fake's `torrent_bytes`.
- `src/ui/mod.rs` — `FolderPromptMode::Export`.
- `src/app/settings.rs` — the `Export` arm in `commit_folder_prompt`;
  `open_folder_prompt(app, FolderPromptMode::Export)` seeded with the *parent* of
  `config.download_dir` (exporting into the download dir itself is the common mistake).
- `src/input.rs` — `Action::Export` (`e`), `CopyPath` (`y`), `CopyContentPaths` (`shift+Y`)
  in the `Screen::Downloads` arm only.
- `src/app/events.rs` — dispatch.
- `src/app/actions.rs` — `copy_selected_path`, `copy_selected_content_paths`.
- `src/ui/help.rs` — the three new bindings.

## Key APIs / libraries

**crossterm 0.29.0 clipboard** — verified 2026-08-16 by reading
`~/.cargo/registry/src/index.crates.io-*/crossterm-0.29.0/src/clipboard.rs` and its
`Cargo.toml`:

- `crossterm::clipboard::{CopyToClipboard, ClipboardType, ClipboardSelection}` exists and
  implements `Command` by writing `OSC 52;<dest>;<base64>`.
- It is behind **`feature = "osc52"`**, which pulls exactly one transitive dep, `base64`.
- `impl Command for CopyToClipboard` has `#[cfg(windows)] fn execute_winapi` that returns
  `ErrorKind::Unsupported`. On Windows, crossterm only falls back to winapi when ANSI is
  unavailable; Windows Terminal (harbour's primary target per **NFR-08**) supports ANSI and
  OSC 52, so the `write_ansi` path is what runs. **This is the one risk to verify by hand** —
  see Risks.

*Dependency justification (AGENTS.md §8):* enabling `osc52` on a crate already in the tree
adds `base64` only. The alternative, `arboard`, pulls x11/wayland/objc stacks on Linux and
macOS for one string copy — strictly worse for a TUI whose terminal already speaks the
protocol. Enabling an existing crate's feature beats adding a crate.

**librqbit 8.1.1** — two facts checked on 2026-08-16:

- `pub use create_torrent_file::{create_torrent, CreateTorrentOptions};` (`src/lib.rs:79`)
  can build a `.torrent` from a folder. **Not used here**, deliberately: FR-37 already stores
  the *original* metainfo, and re-creating one would produce a different infohash than the
  swarm's, which is a subtly wrong file to hand a user. Noted so nobody reaches for it.
- `ManagedTorrent::with_metadata(|m| m.torrent_bytes.to_vec())` is the live source behind
  `RqbitEngine::torrent_bytes`, used as the fallback when the cache file is missing.

**std** — `std::fs::copy` preserves permissions and is the whole of `copy_tree`'s work; no
`fs_extra`, no `walkdir`. A hand-rolled recursive walk is ~25 lines and this repo already
hand-rolls an HTTP server.

## Risks / edge cases

- **OSC 52 can silently do nothing.** Not every terminal enables it (tmux needs
  `set -g set-clipboard on`; some emulators disable it as a security measure), and crossterm
  cannot tell us whether it worked. A silently failing copy is exactly the silent-fallback the
  project rules forbid, which is why **FR-85 also puts the path in the banner** — the user can
  always select it by hand. Verify on Windows Terminal before merging; if the winapi arm turns
  out to be the one taken, fall back to piping to `clip.exe` on Windows and say so in the PR.
- **Export must never move or unseed.** `std::fs::copy`, never `rename`. A rename would break
  the seed and trip the FR-45 file-gone detector, telling the user their data vanished — the
  precise failure FR-45 exists to prevent.
- **Never overwrite.** Check `dst.exists()` before every copy and count skips. An export that
  clobbers a user's folder is unrecoverable.
- **Cross-filesystem, out of space, permissions.** `copy_tree` returns the partial
  `ExportReport` alongside the error so the banner can say `copied 12 of 40 — <err>` instead
  of a bare failure. Do not roll back a partial copy; deleting files after a failed export is
  more dangerous than leaving them.
- **A big export blocks the render loop.** Copying 40 GiB inside the event loop freezes the
  TUI and blows **NFR-01/NFR-02**. Run `copy_tree` on `tokio::task::spawn_blocking` and land
  the result as an `EngineEvent`-style message; per the project rule that every async UI op
  shows state, the row shows `exporting…` while it runs.
- **`only_files` exports.** A partially-selected torrent's directory contains only the files
  that were downloaded. `copy_tree` walks the directory, so it is correct by construction —
  but the report count will not match the torrent's file count. Do not "fix" that.
- **Export target inside the source.** Reject a target that is the item's own `dir` or a
  descendant of it — otherwise the walk copies into itself, forever.
- **Path with `~`.** Run the typed target through `paths::expand_home`, as
  `RqbitEngine::add` already does.
- **A `missing` item has nothing to export.** Banner and stop.

## Test strategy

- **Unit, `src/core/export.rs`** (temp dirs, no engine, no network):
  - nested tree of 3 files in 2 directories → all copied, `ExportReport { copied: 3, .. }`,
    and the **originals still exist**;
  - re-running the same export → `copied: 0, skipped: 3`, and the existing files are
    byte-identical (the never-overwrite guarantee);
  - target inside source → `Err`, nothing copied;
  - `torrent_metadata` finds the FR-37 cache file when present, falls back to
    `engine.torrent_bytes` when the cache file is deleted, and returns `None` when neither
    exists.
- **Unit, `src/core/paths.rs`.** `write_atomic` keeps its existing test
  (`the_torrent_cache_write_is_atomic_and_hash_keyed`) after the move — it moves with the
  function, it is not rewritten.
- **Unit, `src/engine/fake.rs`.** `FakeEngine::torrent_bytes` returns the bytes an item was
  added with, so the export tests never need a real engine.
- **Keymap, `src/input.rs`.** `e`, `y`, `shift+Y` map on `Screen::Downloads`; on
  `Screen::Search` with input focus they are `Action::Type` (the "Dune" regression class);
  with `folder_open` they are `FolderType`, never actions.
- **Buffer snapshot, `src/ui/tests.rs`.** The folder prompt in `Export` mode renders the
  export title and the seeded target path; a completed export renders
  `copied 3, skipped 0` in the banner.
- **Clipboard.** Not asserted against a real clipboard — assert that `copy()` writes the
  expected `\x1b]52;c;<base64>\x07` sequence into a `Vec<u8>` sink, which is the deterministic,
  testable half. The delivery half is manual verification.
- **Integration, `HARBOUR_TEST_NET=1`.** Add a tiny magnet, wait for completion, export, and
  assert both the file tree and the `<name>.torrent` exist in the target and that the item is
  still `Seeding` afterwards.

## Verification

`cargo run` with a completed download:

1. Seeding tab → select the item → press `e`. The folder prompt opens in export mode, seeded
   with the parent of the download dir. Type `~/exports`, Enter.
2. While it runs, the row reads `exporting…` and the TUI still redraws and accepts arrow keys
   (NFR-01 holds — no freeze).
3. `ls ~/exports/<name>/` shows every downloaded file; `ls ~/exports/<name>.torrent` exists and
   `sha1sum` of its `info` dict matches the item's infohash (it is the *original* metainfo,
   not a re-created one).
4. The original files are still in the download folder and the item is still `seeding` with a
   live peer count — the export changed nothing about the torrent.
5. Press `e` again to the same target: the banner reads `copied 0, skipped N`; nothing is
   overwritten.
6. Press `y`, then paste into another window — the item's directory path arrives. The same
   path is also in the banner, so the copy is never a silent no-op even where OSC 52 is
   blocked.
