# External program hook on completion
Ref: #50

## Goal
Run a user-configured program once when a torrent finishes, passing the torrent's name, its
on-disk path, and its `.torrent` comment — without blocking the UI loop and without ever
handing an attacker-controlled string to a shell.

## The finding that shapes this whole plan

Two things were checked in harbour's own source and in the pinned librqbit, 2026-08-16:

**1. The fire-exactly-once point already exists.** `src/queue.rs:449-456`:

```rust
let newly_finished = snap.finished && !item.finished;
if newly_finished {
    item.finished = true;
    events.push(EngineEvent::Done { id: snap.id.clone() });
}
```

`Done` is emitted only on the `false → true` edge, and `QueueItem.finished` is **persisted**,
so a restored seed comes back with `finished == true` and never re-fires. The consumer today is
a single arm in `src/app/actions.rs:391`:

```rust
EngineEvent::Done { .. } => persist(app),
```

That is the entire hook site. **No new `EngineEvent` variant is needed**, which matters because
that enum is a frozen shared type owned by the engine track.

**2. The comment *is* available — the obvious answer was wrong.** librqbit's `TorrentMetadata`
(`src/torrent_state/mod.rs:138`) has no `comment` field, so the first conclusion is that the
comment cannot be passed. But it stores `torrent_bytes: Bytes`, and the *top-level* metainfo
struct in `librqbit-core` does carry it —
`~/.cargo/registry/src/*/librqbit-core-*/src/torrent_metainfo.rs:70`:

```rust
pub struct TorrentMetaV1<BufType> {
    pub announce: Option<BufType>,
    pub info: TorrentMetaV1Info<BufType>,
    pub comment: Option<BufType>,      // <-- here
    pub created_by: Option<BufType>,
    …
}
```

harbour already parses exactly this, in exactly this way, in
`src/engine/rqbit.rs:193` (`librqbit::torrent_from_bytes::<ByteBuf>(bytes)`), and already
exposes `RqbitEngine::torrent_bytes(hash) -> Option<Vec<u8>>` (`rqbit.rs:321`). **So the
comment is one existing call plus one field access** — no new dependency, no bencode parser,
and no "pass empty and explain why" compromise.

Better still, FR-37 already writes every torrent's bytes to
`<state>/cache/torrents/<hash>.torrent`, so the comment is recoverable even for an item the
session no longer holds.

## SPEC / FR reference

Exists today: **FR-38** (completed items drop into "recently downloaded"), **FR-51** (config
persists), **FR-37** (the `.torrent` cache the comment is read from). **Nothing in SPEC
describes executing an external program.** This feature gives a config file the power to run
arbitrary code, so it must be specified before it is built.

**Missing from SPEC — add first, then implement.** Proposed **FR-96 … FR-99**, §4.4:

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then. **The settings-row index claimed below is
> provisional for the same reason**: the parallel batch (`speed-limits`, `share-limits`,
> `protocol-toggles`, `encryption-mode`, and especially `categorized-settings`, which may
> restructure the rows entirely) also adds rows, so the row-collision note below is
> understated — treat it as "coordinate with the whole batch", not just #48/#51.

- **FR-96 (opt-in, once per completion).** When `completion_hook` is set, harbour runs it once
  per torrent on the transition to finished. It never fires for an already-finished item
  restored at startup, and never twice for the same completion.
- **FR-97 (no shell).** The hook is executed directly, never through `sh -c` /`cmd /c`, and
  torrent-supplied text is passed only as separate argv entries and environment variables.
  Harbour performs no string interpolation into a command line.
- **FR-98 (never blocks, never inherits).** The hook is spawned detached with stdio to null;
  harbour does not wait for it. A hook that hangs, spews output, or never exits cannot stall
  the UI or corrupt the terminal.
- **FR-99 (loud on failure).** A hook that cannot be spawned (missing binary, not executable)
  raises an error banner naming the path. A hook that runs and exits non-zero is reported once,
  with its exit code. Failure never affects the torrent's own state.

## Workstream

**Terminal UI (Ishan)** owns it end to end. The trigger lives in the app loop
(`src/app/actions.rs`), not in `src/queue.rs` — the queue is Sarthak's and already emits
everything needed. Do not put process spawning in the queue: it is a UI-loop side effect of an
event the queue already publishes, and `queue.rs` is deliberately testable without a process
table.

Shared-type dependencies: **none.** No new `EngineEvent` variant, no `QueueItem` field, no
`Engine` method. That is a deliberate design goal of this plan, not an accident.

One **engine-track courtesy PR**: expose the comment. `RqbitEngine::torrent_bytes` is already
public but is not on the `Engine` trait, so `app/actions.rs` (which holds `&dyn Engine`) cannot
reach it. Add an additive default-`None` trait method — see step 3.

## Approach

**Step 1 — SPEC (docs only).** FR-96…FR-99.

**Step 2 — the hook runner, pure and testable (UI).** New `src/hook.rs`:

```rust
pub struct HookInfo { pub name: String, pub path: PathBuf, pub comment: Option<String>,
                      pub info_hash: String, pub size_bytes: u64 }

/// Builds the command without running it — this is the unit-testable half.
pub fn build_command(program: &str, info: &HookInfo) -> std::process::Command
pub fn spawn(program: &str, info: &HookInfo) -> std::io::Result<Child>
```

Splitting `build_command` from `spawn` is what makes FR-97 testable without executing anything:
a test asserts the argv vector and the env map for a torrent named
`"; rm -rf ~ #.mkv` and proves it arrives as **one argument**.

Contract, matching how transmission and rtorrent hand off:

- argv: `<program> <name> <path> <comment-or-empty>`
- env: `HARBOUR_TORRENT_NAME`, `HARBOUR_TORRENT_PATH`, `HARBOUR_TORRENT_COMMENT`,
  `HARBOUR_TORRENT_HASH`, `HARBOUR_TORRENT_SIZE`

Both, because argv is convenient for one-liners and env survives arguments containing anything
at all. `stdin/stdout/stderr` → `Stdio::null()` (FR-98): a child inheriting harbour's stdout
would paint over the alternate-screen TUI.

**Step 3 — reach the comment (engine, tiny additive PR).** On the `Engine` trait, following the
established additive-default pattern (`stream_url`, `add_bytes`, `torrent_info_hash`):

```rust
/// The `.torrent` comment field, when metadata has arrived. Default `None`.
fn torrent_comment(&self, _id: &str) -> Option<String> { None }
```

`RqbitEngine` implements it as `torrent_bytes(id)` → `librqbit::torrent_from_bytes::<ByteBuf>`
→ `meta.comment`, with a fallback read from the FR-37 cache file when the session no longer
holds the torrent. `engine/fake.rs` returns a canned comment for tests.

**Step 4 — wire the trigger (UI).** In `src/app/actions.rs`, the `Done` arm becomes: persist as
today, then look up the item, gather `HookInfo`, and spawn. Keep the arm itself two or three
lines by delegating to a `run_completion_hook(app, &id)` helper — `actions.rs` is 748 LOC and
FR-67's review pressure starts at 700.

**Step 5 — report the outcome (UI).** Spawning returns immediately (FR-98). To honour FR-99's
exit-code half without ever blocking, hand the `Child` to a detached
`tokio::task::spawn_blocking` that waits and pushes a warning through the app's existing event
channel on non-zero. Do **not** `.wait()` on the UI thread.

**Step 6 — settings row (UI).** One text row, "Run on completion (path to program)".

### Settings-row collision (cross-PR)

`ui/settings.rs` hardcodes row indices across four `match` blocks against
`const APP_ROWS: usize = 17`, and `app/settings.rs::settings_toggle_row` matches the same
integers. **Issues #48, #50 and #51 all add rows.** This plan adds **one** text row immediately
before the per-source block and bumps `APP_ROWS` by 1; whichever of the three merges later
rebases rather than textually merging index changes.

## Files to create / modify

Create:

- `src/hook.rs` — `HookInfo`, `build_command`, `spawn`, and its unit tests.

Modify:

- `src/main.rs` — `mod hook;`
- `src/persist.rs` — `Config.completion_hook: Option<String>` (the container already carries
  `#[serde(default)]`, so old `config.toml` files load unchanged).
- `src/core/types.rs` — `Engine::torrent_comment`, additive default `None`, with `///` docs.
- `src/engine/rqbit.rs` — implement it (reusing `torrent_bytes` + the FR-37 cache fallback).
- `src/engine/fake.rs` — canned comment.
- `src/app/actions.rs` — `run_completion_hook`, called from the existing `Done` arm.
- `src/ui/settings.rs` — one row, `APP_ROWS` 17 → 18, `TextField::CompletionHook`.
- `src/app/settings.rs` — the commit arm.
- `src/ui/help.rs` / `README.md` — document the argv + env contract so users can write hooks.

## Key APIs / libraries

**New crates: none.** `std::process::Command` covers it; `tokio` (already `features = ["full"]`)
provides `spawn_blocking` for the exit-code watcher.

- `std::process::Command::new(program).args([...]).envs([...]).stdin/stdout/stderr(Stdio::null()).spawn()`
  — spawn returns a `Child` without waiting. No shell is involved, which is the whole of FR-97.
- `librqbit::torrent_from_bytes::<ByteBuf>(&bytes)` — already used at
  `src/engine/rqbit.rs:194`; yields `TorrentMetaV1` whose `comment: Option<BufType>` is at
  `librqbit-core/src/torrent_metainfo.rs:70` (checked 2026-08-16).
- `RqbitEngine::torrent_bytes(hash) -> Option<Vec<u8>>` — `src/engine/rqbit.rs:321`, already
  public.
- `crate::core::paths::torrent_cache_file(state_dir, hash)` — the FR-37 cache path, for the
  fallback read.

**Deliberately not used:** `std::process::Command::new("sh").arg("-c")`, and any crate that
builds a command line from a template string. See Risks.

## Risks / edge cases

- **Command injection is the headline risk, and it is not hypothetical.** Torrent names come
  from the internet and routinely contain quotes, semicolons, backticks and newlines. A hook
  implemented as `sh -c "notify-send '$NAME'"` executes whatever a scraper returned. FR-97 and
  the `build_command`/`spawn` split exist solely to make this structurally impossible, and the
  adversarial-name unit test is the thing that keeps it that way. If a user *wants* shell
  features, they point the hook at their own script — the shell is then their choice, on their
  side of the boundary.
- **Rejected: `sh -c` / `cmd /c` convenience.** It is one line shorter and it reintroduces the
  above. Named here so it is rejected once, in writing.
- **A hook that never exits.** FR-98's detached spawn means harbour does not care. But do not
  accumulate `Child` handles — the `spawn_blocking` waiter owns each one and drops it, so there
  is no zombie pile after a long session.
- **Child inheriting the terminal.** Without `Stdio::null()`, a chatty hook writes over the
  alternate-screen TUI and corrupts the display. This is the most likely "works on my machine,
  looks broken in the demo" bug.
- **`dir` is the torrent's output directory, not the file.** `QueueItem.dir` is where the
  torrent was placed; for a multi-file torrent the content sits in a subfolder named after the
  torrent. Document precisely what `HARBOUR_TORRENT_PATH` means (the output dir) rather than
  implying it is a playable file. `crate::watch::primary_media(&dir)` already exists if a
  future `HARBOUR_TORRENT_FILE` is wanted — out of scope now.
- **`~` in the configured hook path** will not expand — `Command` does not glob.
  `crate::core::paths::expand_home` already exists and is used for download dirs; use it, or
  the first user with `~/bin/hook.sh` gets a confusing "not found".
- **Windows.** A `.ps1` is not directly executable by `CreateProcess`; users must point at
  `powershell.exe` or a `.bat`/`.exe`. FR-99's spawn-failure banner covers it; say so in the
  README line rather than special-casing extensions.
- **Comment is often absent or junk.** Many torrents have no comment; some carry tracker ads.
  Pass it through verbatim as an empty string when absent — never invent a default.
- **Double-fire on a re-added torrent.** Removing an item and re-adding the same infohash is a
  genuinely new completion and fires again. That is correct, not a bug; note it in FR-96.

## Test strategy

- **Unit, `src/hook.rs` — the security test.** `build_command` for a torrent named
  ``"; rm -rf ~ # `whoami`.mkv`` yields argv `[name, path, comment]` where the name is exactly
  **one** element, byte-identical to the input, and the program is the configured path (never
  `sh`). Assert the env map likewise. This is the highest-value test in the feature.
- **Unit, `src/hook.rs`** — a missing comment becomes an empty env var, not the string `None`;
  `~` in the program path is expanded; stdio is null.
- **Unit, end-to-end without a real binary** — point the hook at a tiny helper that writes its
  argv and env to a temp file (on unix `/bin/echo`-style; portably, the test binary re-invoked
  with an env flag), run the `Done` arm, and assert the file contents. Proves the wiring, not
  just the builder.
- **Unit, `src/queue.rs` (existing, unchanged)** — the fire-once guarantee is already covered by
  the existing tests asserting `Done` on the `false → true` edge; add one asserting a ledger-
  restored `finished: true` item produces **no** `Done` on first poll. That is FR-96's
  no-fire-on-restart half and it currently has no explicit test.
- **Buffer snapshot, `src/ui/tests.rs`** — the settings row renders the configured path, and
  the FR-99 failure banner renders.
- **No network tests.**

## Verification

1. `SPEC.md` §4.4 contains FR-96…FR-99.
2. Set the hook to a script that appends its five env vars to `/tmp/harbour-hook.log`. Download
   a small torrent with `cargo run`. On completion the log gains **exactly one** line with the
   correct name, output dir, hash and size — and the comment when the torrent has one
   (verify against `~/.harbour/cache/torrents/<hash>.torrent`).
3. Restart harbour with that completed item still in the ledger → **no new line** in the log.
   That is the once-only guarantee, observable.
4. Rename a test torrent to `; touch /tmp/pwned #.mkv` and complete it → `/tmp/pwned` does
   **not** exist, and the hook log shows the literal name. FR-97, demonstrated.
5. Point the hook at `/does/not/exist` → an error banner naming the path; the torrent still
   completes and still seeds.
6. Point the hook at a script that `sleep 300`s → the TUI stays responsive and the terminal is
   not corrupted.
