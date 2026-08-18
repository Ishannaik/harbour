# Content layout + unwanted folder + pre-allocation
Ref: #47

## Goal
Give the user control over where a torrent's files land (original / subfolder / no subfolder),
fix the fact that harbour currently has **no** containing folder for any multi-file torrent, and
be honest about which of `.unwanted` and pre-allocation librqbit 8.1.1 can actually support.

## The finding that shapes this whole plan

Read on **2026-08-16** from the exact sources harbour compiles against,
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/` and
`…/librqbit-core-5.0.0/`.

### 1. harbour today always flattens multi-file torrents. This is a live bug.

**`librqbit-8.1.1/src/session.rs:1100-1110`** — how the output folder is chosen:

```rust
let output_folder = match (opts.output_folder, opts.sub_folder) {
    (None, None) => self.output_folder.join(
        self.get_default_subfolder_for_torrent(&metadata.info, name.as_deref())?
            .unwrap_or_default(),
    ),
    (Some(o), None) => PathBuf::from(o),
    (Some(_), Some(_)) => bail!("you can't provide both output_folder and sub_folder"),
    (None, Some(s)) => self.output_folder.join(s),
};
```

**`librqbit-8.1.1/src/session.rs:988-1013`** — `get_default_subfolder_for_torrent` returns
`None` when the torrent has fewer than 2 files, otherwise `info.name`. So librqbit's *default*
is exactly qBittorrent's **Original** layout.

**`librqbit-core-5.0.0/src/torrent_metainfo.rs:280-292`** — for a multi-file torrent each
file's name comes from `f.path` alone. **`info.name` is not part of any file path.** The
containing folder exists *only* as the subfolder librqbit joins on.

**`harbour/src/engine/rqbit.rs:427` and `:455`** — harbour sets
`output_folder: Some(resolved_dir…)` on **every** add, magnet and bytes alike.

**Therefore:** harbour takes the `(Some(o), None)` branch every time, `info.name` is never
applied, and every multi-file torrent's files are written **directly into the download
directory with no containing folder**. Two torrents that both contain `Sample/sample.mkv` or a
bare `readme.txt` overwrite each other — and `overwrite: true` is also set unconditionally
(`src/engine/rqbit.rs:430`, `:456`), so the collision is silent. This is not a missing feature;
it is a data-loss path, and it is the most important thing in this issue.

### 2. Unselected files are still created on disk.

**`librqbit-8.1.1/src/storage/filesystem/fs.rs:159-190`** — `FilesystemStorage::init` loops
over **every** entry in `metadata.file_infos`, skipping only padding files, and for each one
runs `std::fs::create_dir_all(parent)` and `OpenOptions::…open(&full_path)`. It never consults
`only_files`.

**`librqbit-8.1.1/src/torrent_state/initializing.rs:242-268`** — the *length-setting* loop, by
contrast, **does** check `only_files` and skips unselected files.

**Therefore:** a deselected file is created as a **0-byte file**, in a directory tree that is
also created, and then never grown. So harbour already litters the output folder with empty
stubs for every file the user unticked — the exact complaint qBittorrent's `.unwanted` folder
exists to answer.

### 3. "Pre-allocate" is half-true, and the true half is sparse.

`ensure_file_length` (`src/storage/filesystem/fs.rs:126-133`) is `File::set_len(len)`. On NTFS
and ext4 `set_len` produces a **sparse** file: it reports the full size but reserves no blocks.
It is called for every selected, non-padding file during initialization
(`initializing.rs:242-268`), unconditionally.

**Therefore:** "files appear at full size immediately" is already true and unconditional.
"Disk space is actually reserved against other writers" is **not** true, is not offered by
librqbit, and cannot be done with `std` alone — `std::fs` exposes no `fallocate`.

### 4. The one legitimate hook, and one that already exists.

- **`librqbit-8.1.1/src/session.rs:271`** — `pub storage_factory: Option<BoxStorageFactory>` on
  `AddTorrentOptions`, with `StorageFactory` / `TorrentStorage` fully public
  (`src/storage/mod.rs:18`, `:117`). A custom storage is the **only** supported way to change
  where bytes physically land. This is the hook for `.unwanted` and for real allocation.
- **`librqbit-8.1.1/src/session.rs:1406-1414`** — `pub async fn update_only_files(&self,
  handle, &HashSet<usize>)`. File selection **can** be changed at runtime. Relevant because it
  interacts badly with `.unwanted` (see Risks).

librqbit 8.1.1 is current ([crates.io/crates/librqbit](https://crates.io/crates/librqbit),
[docs.rs/crate/librqbit/latest](https://docs.rs/crate/librqbit/latest), checked 2026-08-16), so
none of this is "wait for the next release".

## SPEC / FR reference

**Nothing in SPEC.md covers content layout, file allocation, or unselected-file handling.**
The nearest is **FR-40** (output folder per item, at enqueue time only, SPEC.md:187) and
**FR-29** (`d` enqueues to the default output folder, SPEC.md:159). Per AGENTS rule 2, **add to
SPEC first.**

FR numbers **FR-103 … FR-106** (FR-69…FR-102 are claimed: FR-69…FR-85 by the existing plans,
FR-86…FR-89 `speed-limits.md`, FR-90…FR-93 `queue-management.md`, FR-94…FR-97
`share-limits.md`, FR-98…FR-102 `categories-tags.md`). Add to §4.4.

- **FR-103 (content layout).** Each torrent has a layout: **Original** (a multi-file torrent
  gets a containing folder named after the torrent; a single-file torrent does not),
  **Subfolder** (always create the containing folder, including for single-file torrents), or
  **No subfolder** (write files directly into the output folder). The default is **Original**.
- **FR-104 (no silent collisions).** Under *No subfolder*, harbour must not silently overwrite
  an existing file belonging to a different torrent. A collision is surfaced to the user.
- **FR-105 (unselected files).** Files the user deselected are not left as empty stubs in the
  output tree. When "keep unselected files" is on, whatever was already downloaded of them is
  preserved under a `.unwanted` directory; when it is off, they are not created at all.
- **FR-106 (allocation).** harbour sets each selected file to its final length at start, so the
  file browser shows the real size immediately. This is *length-setting*, which on NTFS and
  ext4 creates a sparse file — it does **not** reserve blocks against other writers. harbour
  does not offer block-level pre-allocation; FR-106 says so rather than implying a guarantee it
  cannot keep.

## Workstream

**Engine & Foundation (Sarthak)** owns steps 1–4 and 6 — layout resolution, `AddRequest`, and
anything touching `AddTorrentOptions` or storage.

**Terminal UI (Ishan)** owns step 5 (the settings row and the add-dialog row).

**Depends on:** `speed-limits.md` **step 1** (the settings-row table).
Shared-type dependency: `AddRequest` / `AddBytesRequest` / `QueueItem` gain a layout field —
Sarthak's, following the `bytes` precedent at `src/core/types.rs:406`.

## Approach

**Step 1 — SPEC FR-103…FR-106 (docs only).**

**Step 2 — fix the flattening bug (engine). Ships alone, first, and is the highest-value PR in
this issue.**
Implement **Original** as the default by computing the containing folder in harbour rather than
delegating to librqbit — harbour cannot use librqbit's `(None, None)` branch, because that
branch joins onto the *session* output folder and harbour needs per-item directories (FR-40,
and #46's per-category paths).

Where the name and file count come from:
- **`.torrent` bytes adds:** free and immediate. `src/engine/rqbit.rs:193-196` already parses
  the payload with `librqbit::torrent_from_bytes` to get the infohash; the same
  `TorrentMetaV1Info` yields `info.name` and the file list. No extra work, no network.
- **Magnet adds:** the name is unknown until metadata resolves. Use librqbit's two-phase add —
  `AddTorrentOptions { list_only: true, .. }` returns `AddTorrentResponse::ListOnly(
  ListOnlyResponse { info, output_folder, .. })` (`src/session.rs:1113-1121`) — then add for
  real with the computed `output_folder`. This costs one metadata round-trip that a magnet add
  pays anyway.

`resolve_output_folder(dir, layout, name, file_count) -> PathBuf` is a **pure function** in
`src/engine/rqbit.rs` (or `core::paths`), which is what makes the whole layout matrix testable
without a session.

**Step 3 — the layout is a per-item choice (engine).**
`ContentLayout { Original, Subfolder, NoSubfolder }` in `core::types`; `layout` on
`AddRequest`, `AddBytesRequest`, and `QueueItem` (`#[serde(default)]` → `Original`).
`Config::default_content_layout` supplies the default. Threaded through
`Queue::add_item_to_engine` (`src/queue.rs:286`) exactly like `only_files` already is.

**Step 4 — collision safety under No subfolder (engine).**
Today `overwrite: true` is unconditional. Under `NoSubfolder`, check the resolved file paths
against existing files not belonging to this infohash before adding, and fail loudly with a
clear error rather than overwriting. This is FR-104 and it is the reason `NoSubfolder` is safe
to offer at all.

**Step 5 — the UI (UI track).**
A settings row for the default layout (three-state cycle — extend the step-1 table's toggle
kind to an enum cycle, the same shape the theme row already uses) and a row in the add dialog
(#38 / `add-torrents.md`) for the per-item override.

**Step 6 — `.unwanted` and real allocation: one custom storage, or neither.**
Both FR-105's "keep what was downloaded" and any true pre-allocation require
`AddTorrentOptions.storage_factory`. That means implementing `TorrentStorage` —
`init`, `pread_exact`, `pwrite_all`, `remove_file`, `ensure_file_length`, `take`
(`src/storage/mod.rs:117`) — because `FilesystemStorage`'s own fields are `pub(super)` and
cannot be wrapped by composition, and `relative_filename` lives in `TorrentMetadata`, which
harbour cannot rewrite.

**This is a self-contained ~250-line module and it must be its own PR**, not a tail on step 2.
Scope it as: delegate everything to a plain `std::fs` implementation copied in shape from
librqbit's `fs.rs`, differing in exactly one place — `init` maps unselected files to
`<output>/.unwanted/<relative path>` and, when "keep unselected" is off, does not create them
at all. That single difference is the entire feature; if the diff against librqbit's `fs.rs`
grows beyond it, the approach is wrong.

**Step 7 — upstream, because step 6 should not need to exist (out-of-repo).**
File an issue on `ikatson/rqbit`: `FilesystemStorage::init` (`src/storage/filesystem/fs.rs:159`)
creates and `create_dir_all`s files excluded by `only_files`, while
`initializing.rs:242` correctly skips them for length-setting — an inconsistency that produces
empty stub files and empty directories for every deselected file. A five-line upstream fix
removes harbour's need for a custom storage for the "off" case entirely. Link the issue number
from FR-105. **If the upstream fix lands, step 6 shrinks to the `.unwanted` case only.**

**Step 8 — block-level pre-allocation is explicitly out of scope for v1.**
Doing it needs `fallocate` / `posix_fallocate` / `SetFileInformationByHandle`, none of which
`std::fs` exposes. The options are a new dependency (`fs4` offers `FileExt::allocate`) or
`unsafe` libc calls — and **SPEC FR-62 forbids `unsafe` in shipped code**, which settles it:
a crate or nothing. Per AGENTS rule 8 a new crate needs justification, and the benefit
(fragmentation avoidance on spinning disks) does not clear that bar for a v1 whose users are on
SSDs. FR-106 documents the sparse-file reality instead of shipping a toggle that lies. Revisit
with `fs4` only if a user reports real fragmentation.

## Files to create / modify

- `SPEC.md` — FR-103…FR-106 in §4.4; cross-references from FR-29 and FR-40.
- `src/core/types.rs` — `ContentLayout`; `layout` on `AddRequest`, `AddBytesRequest`,
  `QueueItem`.
- `src/engine/rqbit.rs` — `resolve_output_folder`; the two-phase `list_only` magnet path; the
  FR-104 collision check; module `//!` docs recording the `(Some(o), None)` branch finding with
  the exact upstream file:line.
- `src/engine/fake.rs` — record the resolved folder so queue tests can assert layout.
- `src/queue.rs` — thread `layout` through `add_item_to_engine`.
- `src/persist.rs` — `default_content_layout`, `keep_unselected_files` on `Config`.
- `src/storage/unwanted.rs` — **new** (step 6), the custom `TorrentStorage`.
- `src/ui/settings.rs`, `src/app/settings.rs` — the layout and keep-unselected rows.
- `src/ui/batch_picker.rs` — surface where deselected files will go.
- `docs/plans/content-layout.md` — this file; update when the upstream issue moves.

## Key APIs / libraries

Everything above is from reading the vendored librqbit 8.1.1 and librqbit-core 5.0.0 sources
(file:line cited inline) on 2026-08-16. Reference semantics for `.unwanted` come from
qBittorrent, which re-introduced *"Keep unselected files in .unwanted folder"* in 5.0 —
deselected files are written into a hidden `.unwanted` directory so whatever was already
downloaded is preserved and can still be seeded
([qbittorrent#239](https://github.com/qbittorrent/qBittorrent/issues/239),
[qbittorrent#13531](https://github.com/qbittorrent/qBittorrent/issues/13531), checked
2026-08-16). ratatui 0.30.2 is current
([github.com/ratatui/ratatui/releases](https://github.com/ratatui/ratatui/releases), checked
2026-08-16); no new widgets needed.

**New crates: none in this plan.** `fs4` is named in step 8 as the *only* viable route to real
pre-allocation and is explicitly **not** adopted.

## Risks / edge cases

- **Step 2 changes where files land for every future multi-file torrent.** Existing items must
  keep their current paths — `QueueItem.dir` is already durable and librqbit's own persistence
  holds the resolved folder, so restored torrents are unaffected. But this must be verified,
  not assumed: a restart after step 2 that relocates an in-progress torrent would trigger a
  full re-download. Test it explicitly.
- **The two-phase magnet add doubles the add path.** `list_only` returns without starting the
  torrent, so a failure between the two phases must leave the item `Queued`, not half-added.
  Reuse the existing `Failed`-on-start handling in `Queue::promote` (`src/queue.rs:276-280`),
  which already tolerates a start that fails without stalling the queue.
- **`update_only_files` breaks `.unwanted` after the fact.** Selection can change at runtime
  (`src/session.rs:1406`), but a custom storage fixes paths in `init`. A file moved into
  `.unwanted` at add time and re-selected later would keep writing into `.unwanted`. Decide
  once: **`.unwanted` placement is fixed at add time**, and re-selecting a file after the fact
  requires a re-add. Record it in FR-105 rather than shipping a control that half-works.
- **Path traversal.** librqbit validates torrent names for traversal in
  `check_valid` (`src/session.rs:1001-1006`) *only* on the default-subfolder path — the branch
  harbour does not take. Computing the containing folder in harbour means **harbour must do
  that validation itself**: reject any `info.name` with a non-`Normal` path component before
  joining it. NFR-11 requires it and this is the exact place a malicious torrent name would
  escape the download directory. Non-negotiable, and it ships in step 2, not later.
- **Windows path length.** `Original` adds a path component; a deep torrent already near
  `MAX_PATH` can start failing on Windows, which is harbour's primary target (NFR-08). Surface
  the OS error rather than silently falling back to a flatter layout.
- **`.unwanted` and the hidden attribute.** qBittorrent sets the hidden attribute on Windows and
  loses it on move ([qbittorrent#20826](https://github.com/qbittorrent/qBittorrent/issues/20826)).
  harbour should not chase that; a dot-prefixed name is enough, and the leading dot is
  cosmetic-only on Windows. Say so rather than adding a platform-specific call.
- **Scope honesty for the issue.** #47 lists three bullets. *Content layout* is fully
  buildable and also fixes a real bug. *Keep unselected in `.unwanted`* is buildable but needs a
  custom storage, and the "off" half may be solved upstream for free. *Pre-allocate disk space*
  is **not** buildable without a new dependency, because FR-62 forbids `unsafe`; the length-
  setting half is already true and gets documented. Close the issue on that basis; do not
  fabricate the third.

## Test strategy

- **Unit, `src/engine/rqbit.rs`** — `resolve_output_folder` across the full matrix: {Original,
  Subfolder, NoSubfolder} × {single-file, multi-file}. Original + multi-file appends the name;
  Original + single-file does not; Subfolder always appends; NoSubfolder never does.
- **Unit, `src/engine/rqbit.rs`** — traversal rejection: `info.name` of `..`, `../x`, `/abs`,
  `a/../..`, and (on Windows) `C:\x` are all refused before any join. This is the security test
  and it should read like one.
- **Unit, `src/queue.rs`** against `FakeEngine` — `layout` reaches the engine; a legacy ledger
  item loads as `Original`; the layout on a restored item is not recomputed.
- **Unit, `src/storage/unwanted.rs`** (step 6) — against a temp dir: selected files are created
  at full length in the output tree; unselected files with "keep" on land under `.unwanted/`
  preserving their relative path; with "keep" off they are not created at all, **and neither are
  their parent directories** (the empty-directory half of the current bug).
- **Buffer snapshot, `src/ui/tests.rs`** — the layout settings row cycles through all three
  values; the batch picker shows where deselected files will go.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — add a real **multi-file**
  magnet with the default layout and assert the download directory contains exactly one new
  entry, a directory named after the torrent. Today that assertion **fails**, which is the
  clearest possible statement of the bug in step 2.

## Verification

1. `SPEC.md` §4.4 contains FR-103…FR-106, and FR-106 states the sparse-file reality in plain
   words rather than promising reserved blocks.
2. `cargo run`, add a multi-file torrent with the default layout. **The download directory
   contains one folder named after the torrent, with the files inside it** — not a scatter of
   loose files. That is the user-visible proof of step 2 and the single most important result in
   this issue.
3. Add the same torrent with layout = No subfolder into a directory that already contains a
   file of the same name from another torrent: harbour reports the collision and refuses,
   rather than overwriting (FR-104).
4. Add a single-file torrent with layout = Subfolder: it lands inside a folder of its own.
5. Deselect files in the batch picker with "keep unselected" **off**: after start, `ls -a` the
   output folder shows **no** 0-byte stubs and no empty directories for the deselected files —
   the behaviour that is broken today.
6. With "keep unselected" **on**, partially-downloaded deselected data appears under
   `.unwanted/` with its relative path preserved.
7. `HARBOUR_TEST_NET=1 cargo test --test engine_net` passes the multi-file layout assertion.
8. An upstream issue exists on `ikatson/rqbit` about `FilesystemStorage::init` ignoring
   `only_files`, linked from FR-105.
