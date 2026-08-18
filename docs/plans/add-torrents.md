# Add torrents (magnet / .torrent / URL / watch folder)
Ref: #38

## Goal
One add path — magnet, `.torrent` file, HTTP(S) URL, or a watched folder — through a single
add dialog that sets save path, name override, and start-now/paused before the torrent is
handed to the engine.

## SPEC / FR reference

Exists today:

- **FR-02 / FR-39** — `harbour <magnet|infohash|.torrent>` validates the arg and enqueues to
  the default output folder. Implemented in `src/cli.rs` + `src/app/actions.rs`
  (`enqueue_magnet`, `enqueue_torrent`).
- **FR-29 / FR-40** — `d` enqueues to the default folder; `shift+D` opens the folder prompt
  ("output folder per item at **enqueue time only**"); `o` changes the persisted default.
  Implemented as `FolderPrompt` / `FolderPromptMode` in `src/ui/mod.rs` and
  `src/app/settings.rs` (`open_folder_prompt`, `commit_folder_prompt`).
- **FR-56** — duplicate detection by infohash: a second add focuses the existing row
  (`Queue::add` → `AddOutcome::Duplicate`).
- **FR-51** — `config.toml` persists the default output folder.

**Missing from SPEC — add first, then implement.** Nothing in SPEC covers: adding from a
remote URL, a watch folder, an add dialog at all, a name override, start-paused, categories,
or tags. Proposed new section **4.4a Add dialog & auto-add (FR-69 … FR-75)**:

- **FR-69** `a` on the downloads screen (and `shift+A` from search results) opens the add
  dialog. The dialog accepts a magnet URI, a 40-hex infohash, a local `.torrent` path, or an
  `http(s)://` URL to a `.torrent`, and is the only place those four are normalised.
- **FR-70** The dialog exposes, per add: save path (seeded with the config default), display
  name override, and start-now vs start-paused. Enter commits, Esc cancels; nothing reaches
  the engine until Enter.
- **FR-71** A start-paused add enters the ledger as `paused` with `finished == false` and
  never consumes a `HARBOUR_MAX_DOWNLOADS` slot (`QueueStatus::is_active_download` already
  says so).
- **FR-72** A URL add fetches the body with a hard deadline; a non-200, an over-size body, or
  a payload that is not a parseable `.torrent` is a loud banner and no queue item. Bodies are
  capped at 10 MiB.
- **FR-73** The watch folder is off by default. When configured, `.torrent` files appearing in
  it are added with the dialog's defaults (no prompt) and the source file is renamed to
  `<name>.torrent.added` so it is never re-offered. A file that fails to parse is renamed
  `<name>.torrent.failed` and banners once.
- **FR-74** The per-torrent save path set in the dialog is durable: it is `QueueItem.dir`, the
  same field a restart and a retry read.
- **FR-75** Sequential download is set at add time only and is engine-dependent — see #41 and
  `docs/plans/sequential-download.md`. Do **not** SPEC a toggle until the engine can honour it.

## Workstream

- **UI (Ishan)** — the add dialog, its keymap, the folder-prompt reuse, buffer snapshots.
- **Engine & Foundation (Sarthak)** — every shared-type change: `AddInput.start_paused`,
  `AddRequest.paused` / `AddBytesRequest.paused`, `QueueItem.category`, the URL fetch helper,
  and the watch-folder scanner.
- **Indexer (Dhruv)** — nothing.

Shared-type dependencies: `AddInput` (`src/queue.rs`), `AddRequest` / `AddBytesRequest` /
`QueueItem` (`src/core/types.rs`). New `QueueItem` fields land with
`#[serde(default, skip_serializing_if = ...)]` — the existing `bytes` field is the precedent,
so ledgers written by older builds keep loading (FR-54).

## Approach

Five PRs. Each is independently buildable and testable; none exceeds ~400 lines.

**Step 1 — SPEC (docs only, engine track).** Land FR-69…FR-74 in `SPEC.md` §4.4a and the new
`a` binding in §UR keybind table. Nothing else merges before this.

**Step 2 — start-paused through the stack (engine track).**
`AddRequest`/`AddBytesRequest` gain `pub paused: bool`; `RqbitEngine::add`/`add_bytes` set
`AddTorrentOptions { paused, .. }`. `AddInput` gains `start_paused: bool`; `Queue::add`
stamps `QueueStatus::Paused` instead of promoting when set. `FakeEngine` honours it so the
queue tests stay network-free. No UI yet — the CLI gains `--paused` as the test surface.

**Step 3 — the add dialog (UI track).** A modal over the current screen, structurally a copy
of the settings overlay: `row_count()` / `row_kind()` / `row_label()` in
`src/ui/add_dialog.rs` so the painter and the key handler cannot disagree about what row 3
is. Rows: `Source` (text), `Save path` (text, seeded from `config.download_dir`),
`Name` (text, optional override), `Start` (toggle: now / paused). Enter on the last row
commits. The dialog only *collects*; committing calls the existing
`enqueue_magnet` / `enqueue_torrent`.

**Step 4 — URL add (engine track).** `fetch_torrent_bytes(url) -> Result<Vec<u8>, ...>` in
`src/core/fetch_torrent.rs` using the existing `reqwest`. Wired into the dialog's commit: an
input starting with `http://` or `https://` fetches, then goes down the `add_bytes` path.

**Step 5 — watch folder (engine track).** `src/autoadd.rs`, modelled line-for-line on
`src/theme_watch.rs`. One `notify` watcher on `config.watch_dir`, debounced, non-recursive.
On a `.torrent` create/modify event it reads, parses, enqueues, and renames the source file.

**Step 6 (deferred, do not build yet) — category / tags.** See Risks.

## Files to create / modify

Create:

- `docs/plans/` entry for SPEC §4.4a wording (step 1 edits `SPEC.md` directly).
- `src/ui/add_dialog.rs` — `AddDialog` state + `draw()` + `row_count/row_kind/row_label`.
  Pure paint; the app loop owns every mutation.
- `src/app/add.rs` — `open_add_dialog`, `add_dialog_activate`, `add_dialog_type`,
  `add_dialog_backspace`, `commit_add_dialog`. Mirrors `src/app/settings.rs`.
- `src/core/fetch_torrent.rs` — the URL fetch with deadline + size cap.
- `src/autoadd.rs` — the watch-folder thread.

Modify:

- `SPEC.md` — §4.4a (FR-69…FR-74) and the keybind table.
- `src/core/types.rs` — `AddRequest.paused`, `AddBytesRequest.paused` (engine track).
- `src/queue.rs` — `AddInput.start_paused`; `Queue::add` respects it before `promote()`.
- `src/engine/rqbit.rs` — pass `paused` into `AddTorrentOptions` in both `add` and
  `add_bytes`.
- `src/engine/fake.rs` — honour `paused` so queue tests cover it.
- `src/input.rs` — `Action::OpenAddDialog`, `AddType(char)`, `AddBackspace`, `AddActivate`,
  `AddCancel`; `FocusFlags.add_dialog_open`; the modal branch goes **after** `help_open` and
  before `settings_open`, matching the existing overlay ordering.
- `src/app/mod.rs` — `add: AddDialog` field on `App`, the `draw` overlay call, the
  `FocusFlags` wiring, and `autoadd` spawn at boot.
- `src/app/events.rs` — dispatch the new actions.
- `src/ui/mod.rs` — `pub mod add_dialog;` + re-export.
- `src/ui/help.rs` — add `a` to `BINDINGS` (the
  `every_action_the_keymap_can_produce_is_documented_in_the_help` test enforces this).
- `src/persist.rs` — `Config.watch_dir: Option<PathBuf>` with `#[serde(default)]`.
- `src/ui/settings.rs` + `src/app/settings.rs` — one new text row for the watch folder
  (`APP_ROWS` 17 → 18; the `row_kind` index tests must be updated in the same PR).
- `src/cli.rs` — accept an `http(s)` positional and a `--paused` flag; extend `HELP`.

## Key APIs / libraries

**librqbit 8.1.1** — verified by reading
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/src/session.rs:234-282` on
2026-08-16. `AddTorrentOptions` fields relevant here already exist and need no upstream work:

```
paused: bool                 // FR-71 start-paused, exactly what we need
output_folder: Option<String>// already used by RqbitEngine::add
sub_folder: Option<String>   // errors if output_folder is also set — do not set both
only_files: Option<Vec<usize>>
overwrite: bool              // already set true
list_only: bool              // returns ListOnlyResponse without starting the torrent
trackers: Option<Vec<String>>
```

`list_only` is the clean hook if the dialog ever wants to show the file list before
committing — it returns `ListOnlyResponse { info, output_folder, torrent_bytes, .. }` and
`into_handle()` yields `None`, so nothing starts. Not in scope for step 3, noted so nobody
reinvents it.

librqbit 8.1.1 is the current release
([crates.io/crates/librqbit](https://crates.io/crates/librqbit), checked 2026-08-16) — the
pin in `Cargo.toml` is already at head, so no upgrade risk is bundled into this work.

**notify 8.2.0** — already a dependency (`src/theme_watch.rs`). `recommended_watcher` +
`RecursiveMode::NonRecursive` + an `EventKind::Create | EventKind::Modify` filter is the
established pattern in this repo; copy it rather than inventing a second one.

**reqwest 0.13.4** — already a dependency with the `stream` feature. URL add uses
`Response::bytes()` behind a `tokio::time::timeout`; no new crate.

**New crates: none.** Everything above is already in `Cargo.toml`.

## Risks / edge cases

- **Naming collision.** `src/watch.rs` and `src/app/watch.rs` already mean *watch mode* (play
  a video). The watch **folder** must not be called `watch` — use `autoadd` / "auto-add
  folder" in code, config, and UI copy, or the next reader loses an hour.
- **The re-add loop.** FR-56 dedupe returns `Duplicate`, which banners. Without renaming the
  consumed file, every boot re-scans the folder and re-banners for every file ever added.
  Renaming to `.torrent.added` is the fix and is why FR-73 spells it out. A `.failed` rename
  is the loud-failure half — never delete a user's file.
- **Watch folder == download folder.** If a user points the auto-add folder at their download
  directory, librqbit writes files there and the watcher fires on them. Reject the config
  value at set time when it equals or is a parent of `config.download_dir`, with a banner.
- **Partial writes.** A `.torrent` copied into the folder fires `Create` before the bytes
  land. Debounce (the theme watcher already does this) and treat a parse failure as retryable
  once before renaming to `.failed`.
- **URL add is an SSRF-ish surface.** A user-typed URL is user intent, so no blocklist, but
  the 10 MiB cap and the deadline are non-negotiable — an infinite body would otherwise hang
  the add.
- **`sub_folder` + `output_folder` together error** in librqbit. The dialog sets
  `output_folder` only.
- **Categories and tags are a taxonomy with no consumer.** The issue asks for both, but there
  is no filter, no sidebar group, and no save-path mapping that reads them today, and
  `QueueItem` is a frozen shared type. Ship steps 1–5 first. When it lands, ship
  `category: Option<String>` **only** (single string, qBittorrent-style, used to derive the
  default save path — that gives it a job); leave tags out until something filters on them.
  This is a scope call to make explicitly in the PR, not silently.

## Test strategy

- **Unit (engine track).** `Queue::add` with `start_paused: true` produces
  `QueueStatus::Paused`, does not call `engine.add`'s promote path, and does not consume a
  slot — assert `active_count() == 0` with `max_downloads: 1` and a second add still starting.
- **Unit (engine track).** `fetch_torrent_bytes`: a 404, a body over the cap, and a
  non-bencode payload each return `Err` and never produce a queue item. Served from a
  `std::net::TcpListener` on loopback, as `src/watch.rs`'s `server_serves_ranges` test does.
- **Unit (engine track).** `autoadd`: drop a fixture `.torrent` into a temp dir, assert the
  item is enqueued under the infohash from the file, and the source is renamed to
  `.torrent.added`; a garbage file is renamed `.torrent.failed` and enqueues nothing.
- **Unit (UI track).** `row_kind` / `row_label` round-trip for every dialog index, plus
  `row_kind(row_count()) == None` — the same shape as
  `src/ui/settings.rs`'s existing row tests.
- **Keymap (UI track).** With `add_dialog_open`, every printable key is `AddType`, Esc is
  `AddCancel`, Enter is `AddActivate`, Tab does **not** switch screens, and `ctrl+c` still
  quits. Mirrors `the_folder_prompt_owns_its_keys_while_open`.
- **Buffer snapshot (UI track).** `src/ui/tests.rs`: render the dialog into a
  `TestBackend` and assert the save-path row shows the configured default and the start row
  reads `now`.
- **Integration, `HARBOUR_TEST_NET=1` (engine track).** `tests/engine_net.rs`: add a real
  tiny magnet with `paused: true`, poll `snapshot()`, assert `EngineItemState::Paused` and
  zero progress after 5s.

## Verification

`cargo run`, then on the downloads screen:

1. Press `a` — the dialog opens with the save path pre-filled from `config.toml`.
2. Paste a magnet, tab to `Start`, set it to `paused`, press Enter. The row appears in
   **Downloads** as `paused` with a 0% bar and no speed. Press `p` — it starts.
3. Press `a`, paste `https://webtorrent.io/torrents/sintel.torrent`, Enter. The row appears
   named from the file's own metadata, not from the URL.
4. Set an auto-add folder in settings, drop a `.torrent` into it. Within a second the row
   appears and the file on disk is now `<name>.torrent.added`.
5. Quit and relaunch: the paused item is still paused, and the per-item save path in
   `~/.harbour/downloads.json` is the one typed in the dialog, not the config default.
