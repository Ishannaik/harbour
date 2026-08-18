# Watch / play finished files directly
Ref: #40

## Goal
On a finished item, `w` plays the file from disk in the user's own default player — no
loopback server, no stream URL — and offers a per-file choice when the torrent has more than
one video.

## SPEC / FR reference

Exists today, §4.7 **Watch mode (FR-57 … FR-61)**:

- **FR-57** `w` on a playable item opens the now-playing view and streams it.
- **FR-58** the stream endpoint serves HTTP Range so the player can seek.
- **FR-59** the player exiting ends the session and returns to the TUI.
- **FR-61** the endpoint binds loopback only.

Implemented in `src/watch.rs` (a hand-rolled Range-capable single-file server +
`find_players()` + `WatchSession`), `src/app/watch.rs`, and `src/ui/now_playing.rs`.
`src/engine/rqbit.rs` additionally exposes `stream_url`, `list_video_files`, and
`stream_file_url` over librqbit's loopback HTTP API for the watch-while-downloading path.

**Missing from SPEC — add first.** SPEC has no notion of handing a *finished, on-disk* file to
the OS default handler. Every current path goes through a harbour-owned HTTP server, which is
the right design while pieces are still arriving and pure overhead once they are not.
Proposed **FR-57a / FR-57b** in §4.7:

- **FR-57a** For an item that is `finished`, `w` launches the OS default handler on the file
  path directly (`xdg-open` on Linux, `open` on macOS, `start` on Windows) instead of starting
  a stream server. Harbour does not enter the now-playing screen for this path: the handler is
  detached and harbour has no player process to track.
- **FR-57b** When a finished item contains more than one video file, `w` opens the existing
  episode picker first; the chosen file is what gets opened. A single-video item opens
  immediately with no prompt.

The now-playing screen and FR-59's player-exit lifecycle stay exactly as they are for the
unfinished / streaming path — this adds a branch, it does not replace anything.

## Workstream

- **UI (Ishan)** — the `w` branch on finished items, reusing `EpisodePicker`; the banner copy.
- **Engine & Foundation (Sarthak)** — `Engine::list_files` (the on-disk sibling of
  `list_video_files`), because it is a frozen-trait addition. Small, but it is theirs.
- **Indexer (Dhruv)** — nothing.

Shared-type dependencies: one **additive** `Engine` trait method with a default impl,
following the precedent set by `stream_url` / `list_video_files` / `set_speed_limits` — the
trait is frozen, so a default keeps `FakeEngine` and every other implementor compiling.

## Approach

Four PRs, and **step 1 is a bug fix that must land first.**

**Step 0 — fix the broken non-Windows build (engine track, ~10 lines).**
`src/app/actions.rs:569` calls `open::that_detached(dir)` under `#[cfg(not(windows))]`, but
`open` appears in **neither `Cargo.toml` nor `Cargo.lock`** (verified 2026-08-16:
`grep -n '^name = "open"' Cargo.lock` returns nothing). Linux and macOS builds do not compile
today. Fix it in the direction the repo already leans — `src/watch.rs` hand-rolls its whole
HTTP server rather than pull a framework, and AGENTS.md rule 8 says justify every crate. So:
add `src/core/openers.rs` with one function and delete the `open::` call. No new dependency.

```rust
/// Hands `path` to the OS default handler, detached.
///
/// Hand-rolled rather than the `open` crate: three `Command` invocations do not
/// justify a dependency (AGENTS.md §8), and this is the same std-only policy
/// `src/watch.rs` already follows for its stream server.
pub fn open_default(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    // `start` is a cmd builtin, not an executable, so it needs `cmd /C`. The
    // empty "" is the window-title argument — without it, a quoted path is
    // swallowed as the title and nothing opens.
    let mut cmd = { let mut c = Command::new("cmd"); c.args(["/C", "start", ""]); c };
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = Command::new("xdg-open");

    cmd.arg(path).stdin(Stdio::null()).stdout(Stdio::null())
       .stderr(Stdio::null()).spawn().map(|_| ())
}
```

**Step 1 — SPEC (docs only).** FR-57a / FR-57b into §4.7.

**Step 2 — `Engine::list_files` (engine track).** `list_video_files` today reads librqbit
metadata and returns `TorrentFileView { id, name, size_bytes }` for video files only. Add a
sibling that also returns the **absolute on-disk path**, since the open path needs a real
file, not an index. Simplest honest shape: reuse `TorrentFileView` and resolve the path in the
app as `item.dir.join(&file.name)` — `relative_filename` is exactly that. No new type; assert
the join in a test rather than adding a field to a frozen struct.

**Step 3 — the `w` branch (UI track).** In `src/app/watch.rs::start_watch`:

```
if item.finished {
    let files = engine.list_video_files(&id).await;      // already exists
    match files.len() {
        0 => fall back to watch::primary_media(&item.dir)  // already exists
        1 => open_default(item.dir.join(&files[0].name))
        _ => episode_picker.open_for(...)                   // already exists
    }
} else {
    ...the current stream path, untouched...
}
```

Everything in that branch already exists except `open_default`. The episode picker's chosen
index routes to the same `open_default` call instead of `stream_file_url`.

**Step 4 (explicitly out of scope) — in-terminal preview via chafa/mpv.** Not planned, not
built. It needs an external binary harbour cannot assume, a sixel/kitty-graphics capability
probe, and a second render path that fights the 30fps ratatui loop. If it is ever wanted it is
its own issue with its own SPEC entry. Say so in the PR and move on.

## Files to create / modify

Create:

- `src/core/openers.rs` — `open_default(path)`, plus `open_reveal(path)` for the existing
  "show in file manager" behaviour so `open_selected_item` has one home.

Modify:

- `SPEC.md` — FR-57a / FR-57b in §4.7.
- `src/core/mod.rs` — `pub mod openers;`.
- `src/app/actions.rs` — `open_selected_item` calls `openers::open_reveal`; the `open::` call
  and its `#[cfg]` split are deleted. This is the build fix.
- `src/app/watch.rs` — the `item.finished` branch in `start_watch`; the episode-picker
  confirm routes to `open_default` for finished items.
- `src/ui/help.rs` — `w` already documented; update the description to
  `watch / open in your player`.
- `SPEC.md` §UR keybind table — same wording change.

Deliberately **not** modified: `src/watch.rs`'s server, `src/ui/now_playing.rs`,
`src/engine/rqbit.rs`'s stream methods. The streaming path is correct for unfinished items and
this change must not touch it.

## Key APIs / libraries

**Platform openers**, verified as the current correct invocations on 2026-08-16:

- Linux: `xdg-open <path>` (freedesktop `xdg-utils`).
- macOS: `open <path>`.
- Windows: `cmd /C start "" <path>`. `start` is a **cmd builtin**, so `Command::new("start")`
  fails with "program not found" — this is the single most common way this gets shipped
  broken. The empty `""` is the window title; omitting it makes cmd treat a quoted path as the
  title and silently open nothing.

`src/watch.rs::default_video_handler()` already resolves the Windows default video handler by
walking `HKCR\.mkv → ProgID → shell\open\command`, and `find_players()` already prefers it.
For a finished file the OS association *is* the answer, so `cmd /C start` is both simpler and
more correct than re-deriving it — but the registry walk stays, because the stream path has no
file extension to associate on.

**`config.player` still wins.** If the user has explicitly set a player in settings, honour it
(`Command::new(player).arg(path)`) before falling back to the OS default. Ignoring an explicit
setting because a file happens to be finished would be a surprise.

**New crates: none.** The `open` crate is *removed* as a phantom dependency, not added.

## Risks / edge cases

- **The non-Windows build is broken right now.** Step 0 is not optional and should arguably be
  its own hotfix PR ahead of this whole feature. Confirm with
  `cargo check --target x86_64-unknown-linux-gnu` in CI once it lands.
- **`finished` is not the same as "the files are there."** `QueueStatus::Missing` exists
  precisely because a finished item's files can vanish (FR-45). Check
  `path.exists()` before spawning and banner honestly (`the file is gone — press r to
  re-check`) rather than launching a player onto nothing.
- **No lifecycle.** A detached handler gives harbour no child process, so FR-59's "player
  exited → return to the TUI" cannot apply. That is why FR-57a says harbour does **not** enter
  the now-playing screen for this path. Do not fake a now-playing screen it cannot exit.
- **Multi-file torrents nest.** librqbit puts multi-file torrents in a sub-folder by default,
  so `item.dir.join(&file.name)` must use the `relative_filename` librqbit reports
  (which already includes the sub-path), not just the basename. Test the join.
- **`xdg-open` may be absent** on a bare server or a minimal container. `spawn()` returns
  `Err` — banner it, never swallow it.
- **`only_files` batch downloads.** A partially-selected torrent reports files that were never
  downloaded. Filter to files that exist on disk before offering the picker.
- **Spaces and non-ASCII in paths** are handled by passing the path as a single `Command::arg`
  — never build a shell string.

## Test strategy

- **Unit, `src/core/openers.rs`.** The command construction is platform-`cfg`'d, so test the
  shape rather than the launch: a `fn opener_argv(path) -> Vec<String>` split out from the
  spawn, asserting Windows yields `["cmd", "/C", "start", "", "<path>"]` (the empty-title
  regression) and unix yields `["xdg-open", "<path>"]`. One assert per platform under `cfg`.
- **Unit, `src/app/watch.rs`.** Against `FakeEngine`: a finished item with one video file
  takes the open path and never constructs a `WatchSession`; a finished item with three video
  files opens the episode picker; an unfinished item still takes the stream path. Assert on
  `app.episode_picker.open` and `app.watch.is_none()` — no process is spawned in tests.
- **Unit.** `item.dir.join(&file.name)` for a multi-file torrent whose `relative_filename` is
  `Season 1/ep01.mkv` produces the nested path, not `dir/ep01.mkv`.
- **Unit.** A finished item whose file does not exist produces an `error_banner` containing
  `gone` and spawns nothing.
- **Buffer snapshot, `src/ui/tests.rs`.** The episode picker opened from a *finished* item
  renders the file list; no now-playing screen is entered.
- **Integration, `HARBOUR_TEST_NET=1`.** Not applicable — this path spawns a GUI process.
  Covered by the manual verification below instead, which is the honest answer.

## Verification

`cargo run` with at least one completed download:

1. Downloads screen → Seeding tab → select a finished single-video item → press `w`. The
   user's own video player opens the file **from disk**; `netstat` shows no new harbour
   loopback listener; the TUI stays on the downloads screen (no now-playing view).
2. Select a finished season pack → `w`. The episode picker opens. Choose episode 3 — that
   file opens, not the largest one.
3. Rename the file on disk, press `w` again. A banner says the file is gone; no player
   launches.
4. Select an item that is still **downloading** → `w`. The old behaviour is unchanged: the
   now-playing screen appears with a `http://127.0.0.1:<port>/...` stream URL, and quitting
   the player returns to the TUI (FR-59).
5. `cargo check` on Linux succeeds — which it does not today, before step 0.
