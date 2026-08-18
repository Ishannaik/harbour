# DHT / PeX / LSD / uTP protocol toggles
Ref: #53

## Goal
Expose the peer-discovery and transport protocols librqbit actually implements as honest
settings toggles — DHT, LSD, uTP — and write down in SPEC that PeX has no toggle because the
engine has no switch for it.

## The finding that shapes this plan

Read on **2026-08-16** from two sources: the version harbour compiles against today,
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/`, and the current release
`librqbit 9.0.0`, downloaded from crates.io
(`curl https://crates.io/api/v1/crates/librqbit/9.0.0/download`, extracted to
`/tmp/librqbit-9.0.0`).

**harbour is a major version behind.** `crates.io/api/v1/crates/librqbit` reports
`"max_stable_version":"9.0.0"`, `"newest_version":"9.0.0"`, `"updated_at":"2026-08-15"`.
`Cargo.toml:12` pins `librqbit = "8.1.1"`.

**Three of the four protocols in this issue only have a knob in 9.0.0.**

| Protocol | librqbit 8.1.1 | librqbit 9.0.0 |
| --- | --- | --- |
| DHT | `SessionOptions.disable_dht: bool` (`session.rs`) | `SessionOptions.dht: Option<DhtSessionConfig>` (`session.rs:421`) — `None` disables |
| LSD | **absent** — no local service discovery at all | `SessionOptions.disable_local_service_discovery: bool` (`session.rs:474`), wired at `session.rs:764-775` |
| uTP | **absent** | `ListenerOptions.mode: ListenerMode` (`listen.rs:53`) + `ConnectionOptions.enable_tcp` (`stream_connect.rs:40`) |
| PeX | always on | always on |

`ListenerMode` (`/tmp/librqbit-9.0.0/src/listen.rs:25-49`) is a three-way enum, not a bool:

```rust
pub enum ListenerMode { TcpOnly, UtpOnly, TcpAndUtp }
```

with `impl Default for ListenerOptions` at `listen.rs:62-75` choosing `TcpOnly` and carrying an
upstream comment that is load-bearing for this plan:

```rust
// TODO: once uTP is stable upgrade default to both
mode: ListenerMode::TcpOnly,
```

**PeX has no configuration surface at any version.** In 9.0.0 it is driven entirely by the
peer's extended handshake and the torrent's private flag
(`/tmp/librqbit-9.0.0/src/torrent_state/live/mod.rs:1179`):

```rust
if !self.state.metadata.info.info().private && hs.m.ut_pex.is_some() {
```

There is no `SessionOptions` field, no `AddTorrentOptions` field, and no runtime setter. A
"PeX" row in harbour's settings would be a bool nothing reads. **It is not built.** The private
flag is already honoured, which is the behaviour a PeX toggle is usually wanted for.

**Fourth finding, and the reason step 0 is not optional:**
`impl Default for SessionOptions` in 9.0.0 (`session.rs:484-512`) sets **`listen: None`**. A
mechanical upgrade that keeps harbour's current `..Default::default()` spread produces a session
with **no incoming listener and no UPnP at all** — inbound peers silently stop arriving and
nothing errors. The upgrade must always construct `Some(ListenerOptions { .. })`.

## Step 0 — the librqbit 8.1.1 → 9.0.0 upgrade (shared prerequisite)

**Issues #53, #55, #56 and #57 all depend on this one PR. It is described here once; the other
plans reference it and must not re-specify it.**

Owner: **Engine & Foundation (Sarthak)**. Branch `engine/librqbit-9`. It is a pure upgrade —
no new settings rows, no user-visible feature — and lands green before any of the four features
start.

What the upgrade must handle, each verified against `/tmp/librqbit-9.0.0`:

1. **`SessionOptions` restructure.** `peer_opts` and `socks_proxy_url` moved into
   `connect: Option<ConnectionOptions>` (`session.rs:443`, `stream_connect.rs:35-42`);
   `enable_upnp_port_forwarding` and the listen port moved into `listen: Option<ListenerOptions>`
   (`session.rs:441`, `listen.rs:52-60`). `listen_port_range: Option<Range<u16>>` is gone —
   the replacement is a single `listen_addr: SocketAddr`.
2. **`listen: None` means no listener.** Always build `Some(ListenerOptions { .. })` in
   `RqbitEngine::new`. A why-comment at the construction site, citing
   `librqbit-9.0.0/src/session.rs:493`, so a later `..Default::default()` cleanup cannot
   silently undo it.
3. **`disable_dht: bool` → `dht: Option<DhtSessionConfig>`.** Map `enable_dht == false` to
   `None`. `DhtSessionConfig` (`session.rs:395-406`) additionally carries `persistence:
   Option<DhtPersistenceConfig>` and an explicit `port` — see the state-dir note below.
4. **`TorrentStatsState::Initializing` is now a struct variant.**
   `/tmp/librqbit-9.0.0/src/torrent_state/stats.rs:46-58`:
   ```rust
   pub enum TorrentStatsState { Initializing { paused: bool }, Live, Paused, Error }
   ```
   `to_snapshot` in `src/engine/rqbit.rs:366-371` matches it as a unit variant today and will
   not compile. Change to `Initializing { .. }`. **Do not** start mapping
   `Initializing { paused: true }` to `EngineItemState::Paused` in this PR — `project_status`'s
   FR-47 split (a restored complete seed passes through `Initializing`) is deliberate and any
   change to it is a separate, tested decision.
5. **Re-verify the stats mapping.** `TorrentStats` moved to
   `torrent_state/stats.rs:72-85`; `LiveStats` is at `stats.rs:9-15`. The
   `stats.live.snapshot.peer_stats.live` path that `src/engine/rqbit.rs:378` depends on must be
   re-read against 9.0.0 rather than assumed — it is the source of the `peers: Option<u32>`
   contract the whole downloads view rests on.
6. **Error type.** 9.0.0 adds `pub use error::{Error, Result}` (`lib.rs:85`) while
   `Session::new_with_opts` still returns `anyhow::Result<Arc<Session>>`
   (`session.rs:568-571`). Every `.map_err(|e| e.to_string())` in `src/engine/rqbit.rs` needs a
   compile-check, not a rewrite.
7. **Feature flags survive.** `tracing-subscriber-utils` still exists in 9.0.0
   (`Cargo.toml:67`) and still gates `Api::new`'s third parameter (`api.rs:179-190`), so
   `RqbitEngine::stream_server`'s `Api::new(session, None, None)` call keeps its arity. `http-api`
   still exists (`Cargo.toml`). Both stay in harbour's `Cargo.toml:12`.
8. **Free win: DHT state moves under harbour's state dir.** `DhtSessionConfig.persistence`
   lets the DHT routing table live under `<state>/engine/` instead of librqbit's own
   configuration directory. Today that is a live violation of the AGENTS invariant
   "`HARBOUR_STATE_DIR` relocates *all* state for testing" — the engine's session state moved in
   `RqbitEngine::new` but the DHT table did not. One field fixes it.

Verification for step 0 is behavioural, not just `cargo build`: a torrent that downloaded before
the upgrade still downloads after it, and `HARBOUR_TEST_NET=1 cargo test` passes.

## SPEC / FR reference

**Nothing in SPEC.md covers protocol selection.** `grep -n "DHT" SPEC.md` matches only NFR-10's
"tracker/DHT traffic for the user's own torrents". FR-51 lists config as "default output folder,
theme name, and seed-by-default toggle" — it does not cover the DHT and UPnP rows that already
ship, let alone new ones. Per AGENTS rule 2, **SPEC first**.

FR numbers **FR-96 … FR-99** (FR-69…FR-95 are claimed by existing `docs/plans/*.md`; verified
with `grep -oh "FR-[0-9]\+" docs/plans/*.md | sort -t- -k2 -n | tail -1`). Add to §4.5 under a
new "Connection & protocol" heading that FR-101…FR-112 (issues #55, #56, #57) also extend.

- **FR-96 (peer discovery).** harbour exposes DHT and Local Service Discovery as independent
  on/off settings, defaulting to on. Both are read once at engine start; changing either
  persists to `config.toml` and applies at the next launch, which the settings rows state.
- **FR-97 (transport).** harbour exposes uTP as an *additional* transport alongside TCP,
  defaulting to off and labelled experimental. TCP is never disabled by this setting: harbour
  offers TCP-only and TCP+uTP, never uTP-only.
- **FR-98 (PeX is not configurable).** Peer Exchange is always active for non-private torrents
  and always inactive for torrents whose metainfo sets `private`. harbour exposes no PeX
  setting, because the engine has no switch for it; a control that changed nothing would be a
  lie in the interface. Re-evaluate when librqbit adds one.
- **FR-99 (protocol changes are boot-time).** Every protocol toggle is applied at session
  construction. harbour never restarts the engine to apply one, and never implies it did:
  the row label carries "(next launch)".

## Workstream

- **Step 0 (librqbit 9 upgrade)** — **Engine & Foundation (Sarthak)**. Load-bearing; blocks
  #55, #56, #57 too.
- **Step 1 (SPEC)** — docs; Sarthak reviews.
- **Step 2 (config + engine mapping)** — **Engine & Foundation (Sarthak)**:
  `EngineLaunchOptions` is the engine's own type and `Config` is persistence.
- **Step 3 (settings rows)** — **Terminal UI (Ishan)**, against the frozen table.

**Shared types: none change.** `TorrentResult`, the `Source` trait, `QueueStatus` and
`EngineEvent` are untouched. `EngineLaunchOptions` (`src/engine/rqbit.rs:581-586`) is not a
frozen shared type — it is the engine module's private boot struct — but it is still Sarthak's
file.

### The settings-row prerequisite (stated identically in all five plans)

`src/ui/settings.rs:98-131` identifies rows by bare integer (`row_kind`, `text_field`,
`source_at`, `row_label`) and `src/app/settings.rs:36-59` dispatches toggles on the same bare
integers. Five issues in this batch each add rows. **The row-table refactor already planned as
step 1 of `docs/plans/speed-limits.md` (#43) and step 1 of `docs/plans/categorized-settings.md`
(#63) lands first, once.** After it, a row is identified by a `TextField` / `ToggleField` value,
so these five plans *append table entries* and never renumber anything. None of the five
re-specifies that refactor.

**The agreed final order of the Connection / BitTorrent category, across all five issues:**

```
Listening Port (empty = auto)        text    #55 (exists today)
Use a Random Port Each Launch        toggle  #55
Bind Address (empty = all)           text    #55
Network Interface (empty = auto)     text    #55
UPnP Port Forwarding                 toggle  #55 (exists today)
Enable DHT                           toggle  #53 (exists today)
Enable Local Peer Discovery (LSD)    toggle  #53
Enable uTP (experimental)            toggle  #53
Global Max Connections               text    #56
Proxy URL                            text    #57 (exists today, relabelled)
Proxy Search & Indexer Traffic       toggle  #57
```

Whichever issue merges first creates the block; the rest slot into their stated positions.

## Approach

**Step 0 — librqbit 9.0.0 upgrade.** Above. Merges alone, green, before anything else.

**Step 1 — SPEC FR-96…FR-99 (docs only, ~40 lines).** Independently reviewable.

**Step 2 — config + engine mapping (engine, ~90 lines).**

`src/persist.rs`'s `Config` gains two fields next to the existing `enable_dht`:

```rust
/// Local Service Discovery (multicast peer discovery on the LAN). Boot-time.
pub enable_lsd: bool,     // default: true
/// uTP transport in addition to TCP. Experimental upstream. Boot-time.
pub enable_utp: bool,     // default: false
```

`#[serde(default)]` is already on the struct (`src/persist.rs:40`), so configs written by older
builds keep working — the existing `a_partial_config_keeps_defaults_for_the_rest` test is the
proof and gains two assertions.

`EngineLaunchOptions` (`src/engine/rqbit.rs:581`) gains the same two bools, and
`RqbitEngine::new` maps them — **the only place librqbit types are allowed to appear**, per the
module's own `//!` contract:

```rust
disable_local_service_discovery: !opts.enable_lsd,
listen: Some(ListenerOptions {
    mode: if opts.enable_utp { ListenerMode::TcpAndUtp } else { ListenerMode::TcpOnly },
    ..
}),
```

`ListenerMode::UtpOnly` is deliberately unreachable from harbour's config. A user who turns uTP
"on" and loses every TCP peer would have no way to tell what happened, and upstream still calls
uTP unstable (`listen.rs:65`).

**Step 3 — the settings rows (UI, ~70 lines).**

Two `ToggleField` variants (`Lsd`, `Utp`) and two table entries in the positions fixed above.
Labels state the boot-time contract, matching the existing `enable_upnp` / `enable_dht` rows:

- `Enable Local Peer Discovery (LSD)` — `[● ON]` / `[○ OFF]`
- `Enable uTP (experimental)` — same glyphs

The category blurb (once #63 lands) is where "next launch" lives; until then the existing rows'
convention — the app-side comment at `src/app/settings.rs:33-35` — is the contract, and the
`?`-detail text from #63 will carry the sentence.

## Files to create / modify

- `SPEC.md` — FR-96…FR-99 in a new §4.5 "Connection & protocol" block.
- `Cargo.toml` — `librqbit = "9.0.0"` (step 0). Features unchanged: `http-api`,
  `tracing-subscriber-utils` both still exist in 9.0.0.
- `src/engine/rqbit.rs` — step 0: the `SessionOptions` restructure, `ListenerOptions`
  construction, `dht: Option<DhtSessionConfig>`, the `Initializing { .. }` match arm, the stats
  path re-verification, DHT persistence under `<state>/engine`. Step 2: `enable_lsd` /
  `enable_utp` on `EngineLaunchOptions` + the mapping + the "`listen: None` means deaf"
  why-comment.
- `src/persist.rs` — `enable_lsd` (default true), `enable_utp` (default false); the round-trip
  and partial-config tests.
- `src/ui/settings.rs` — two `ToggleField` variants + two table rows.
- `src/app/settings.rs` — two arms in the value-dispatched `settings_toggle_row`.
- `src/ui/tests.rs` — snapshot assertions for the two new rows.
- `docs/plans/protocol-toggles.md` — this file; update when upstream adds a PeX switch.

## Key APIs / libraries

Verified 2026-08-16 by reading the extracted crate source, with file:line above:

- librqbit **9.0.0** — `crates.io/api/v1/crates/librqbit` (`max_stable_version 9.0.0`,
  updated 2026-08-15); source from `crates.io/api/v1/crates/librqbit/9.0.0/download`.
- `SessionOptions` `session.rs:418-482`; `Default` `session.rs:484-512`.
- `ListenerMode` / `ListenerOptions` `listen.rs:25-75`.
- `ConnectionOptions` `stream_connect.rs:35-52`.
- LSD wiring `session.rs:764-775`.
- PeX gating `torrent_state/live/mod.rs:1179`.
- The rqbit release notes describe uTP as opt-in and experimental
  ([github.com/ikatson/rqbit/releases](https://github.com/ikatson/rqbit/releases), checked
  2026-08-16), matching the in-source TODO.

**New crates: none.** `librqbit-utp` arrives as a transitive dependency of librqbit 9.0.0
(`librqbit/Cargo.toml:202`, version 0.7) whether or not uTP is enabled — it is not a harbour
dependency decision, but the upgrade PR description must name it so the tree growth is on the
record for AGENTS rule 8, and `deny.toml` may need its licence entry.

## Risks / edge cases

- **The upgrade is the risk; the toggles are trivial.** Keep step 0 in its own PR. If it lands
  mixed with new rows, a regression in peer connectivity is indistinguishable from a settings
  bug.
- **`listen: None` is a silent connectivity regression.** Named twice on purpose. It does not
  fail, it does not warn, it just stops accepting inbound peers. The `HARBOUR_TEST_NET=1`
  integration test is what catches it.
- **LSD is multicast and will make some corporate/VPN networks unhappy.** Default on matches
  qBittorrent and every mainstream client; users on locked-down networks get a toggle, which is
  the point of the issue.
- **uTP default off is deliberate.** Upstream says it is not stable. A default-on experimental
  transport that halves someone's download speed is a support burden with no upside.
- **Rejected: a PeX toggle wired to nothing.** Named here so it is rejected once, in writing.
  There is no engine field to bind it to. FR-98 documents the behaviour instead.
- **Rejected: exposing `ListenerMode::UtpOnly`.** Three-state UI for a two-state decision, with
  a failure mode (no TCP peers) the user cannot diagnose from the TUI.
- **Rejected: restarting the session to apply a toggle live.** It would tear down every active
  torrent and re-verify pieces on disk. Boot-time is the honest contract, and it is what the
  existing UPnP/DHT rows already promise.
- **`private` torrents.** FR-98 asserts PeX is off for them; the assertion is upstream's
  (`live/mod.rs:1179`) and is worth a why-comment referencing that line, because the next
  librqbit bump is when it could silently change.

## Test strategy

- **Unit, `src/persist.rs`** — a config round-trips `enable_lsd` / `enable_utp`; a config file
  written before this change loads with LSD on and uTP off (the additive-default guarantee).
- **Unit, `src/engine/rqbit.rs`** — a small pure mapping fn
  (`fn listener_mode(enable_utp: bool) -> ListenerMode`) so the TCP-is-never-disabled invariant
  is asserted without constructing a `Session`: `false → TcpOnly`, `true → TcpAndUtp`, and
  `UtpOnly` is unreachable. Same shape for `dht_config(enable_dht)` returning `None` / `Some`.
- **Unit, `src/ui/settings.rs`** — the two new `ToggleField` variants each map from exactly one
  table row (the totality test the row-table refactor introduces).
- **Buffer snapshot, `src/ui/tests.rs`** — the settings overlay renders
  `Enable Local Peer Discovery (LSD)` and `Enable uTP (experimental)` with `[○ OFF]` for uTP by
  default; toggling in state flips the glyph.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — step 0's real regression net:
  add a real magnet against librqbit 9.0.0 and assert metadata arrives and
  `snapshot()` reports `peers: Some(n)` with `n > 0`. A second case with `enable_dht = false`
  and a *trackered* magnet must still connect, proving the DHT mapping disables DHT rather than
  the session.

## Verification

1. `cargo tree -p librqbit` shows `9.0.0`; `cargo run` starts, and an existing torrent from
   before the upgrade resumes **without a full re-verify** (fastresume survived).
2. `cargo run` → `shift+S` → the Connection block shows `Enable DHT`, `Enable Local Peer
   Discovery (LSD)`, `Enable uTP (experimental)` in that order, with LSD on and uTP off.
3. Toggle LSD off, quit, check `~/.harbour/config.toml` contains `enable_lsd = false`, relaunch.
   With a second harbour on the same LAN seeding a torrent, the two no longer find each other
   without a tracker — and do when LSD is back on. That LAN pair is the user-visible proof.
4. Toggle uTP on, relaunch, start a download: it still completes (TCP was not disabled), and
   `netstat`/Resource Monitor shows a UDP socket bound on the listen port that was not there
   before.
5. `ls ~/.harbour/engine/` (or `$HARBOUR_STATE_DIR/engine/`) contains the DHT routing table —
   the state-dir invariant, previously broken.
6. `grep -rn "pex" src/` returns nothing outside comments: no PeX row was shipped, and SPEC
   FR-98 says why.
