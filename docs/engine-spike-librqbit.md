# Engine spike — librqbit (E0 evidence)

> Owner: Sarthak (Engine & Foundation track).
> Status: **static spike complete** (API + build + resolution verified). Behavioural
> spike (real magnet, resume, missing-file) is the remaining half — see §5.
> Everything below was re-derived on this machine; nothing is quoted from a
> third-party summary.

## 1. Why this document exists

`docs/architecture.md` §1 commits harbour to librqbit for **three** load-bearing
things at once: the download engine, session-level resume, and the phase-6
streaming endpoint. `docs/roadmap.md` schedules feasibility spikes for the *least*
risky items (phase 7: cs.rin.ru, cover art, headless daemons) and none for
librqbit, and `Cargo.toml` does not yet name it at all. That is the risk ordering
backwards. This document is the missing spike for the engine track.

## 2. Method

- `cargo search librqbit`, `cargo info librqbit@8` for version/feature/licence facts.
- `cargo add librqbit@8` in a throwaway crate → read the resulting `Cargo.lock`.
- `cargo build` on **Windows 11 / MSVC, cargo 1.97.0, rustc 1.97.0, edition 2024**.
- Read the vendored source at `~/.cargo/registry/src/*/librqbit-8.1.1/src/` for the
  API shapes below (`torrent_state/stats.rs`, `torrent_state/mod.rs`, `session.rs`,
  `torrent_state/live/stats/snapshot.rs`).

## 3. Verified facts

| # | Fact | Evidence |
| --- | --- | --- |
| V-1 | Latest **stable** is `8.1.1`; `9.0.0-rc.0` is published but pre-release. Licence **Apache-2.0** (harbour is MIT — permissive-compatible, but the NOTICE obligation is real). | `cargo info librqbit@8` |
| V-2 | `librqbit = "8"` resolves to `8.1.1` and locks **293 crates**. | `Cargo.lock` |
| V-3 | **Builds clean on Windows/MSVC in 1m37s with no native toolchain, no C++ step, no `build.rs` prompt.** | `cargo build` exit 0 |
| V-4 | Default features: `default-tls`, `http-api-client`. Optional: `http-api` (axum + tower-http), `webui`, `watch`, `upnp-serve`, `disable-upload`, `notify`, `postgres`, `sqlx`, `lru`, `rust-tls`, `storage_middleware`, `timed_existence`. | `cargo info` |
| V-5 | `TorrentStats { state, file_progress: Vec<u64>, error: Option<String>, progress_bytes, uploaded_bytes, total_bytes, finished, live: Option<LiveStats> }` | `torrent_state/stats.rs:69` |
| V-6 | `TorrentStatsState = Initializing \| Live \| Paused \| Error`. **There is no `Seeding` and no `Missing`.** A seeding torrent is `Live` with `finished == true`. | `torrent_state/stats.rs:47` |
| V-7 | **`Paused` is a first-class engine state.** | `torrent_state/stats.rs:52`, `ManagedTorrent::is_paused()` at `torrent_state/mod.rs:421` |
| V-8 | **No peer count on `TorrentStats`.** Peers live at `stats.live?.snapshot.peer_stats: AggregatePeerStats` — i.e. unavailable whenever `live` is `None` (paused, initializing, errored). | `torrent_state/live/stats/snapshot.rs:8` |
| V-9 | `LiveStats.time_remaining: Option<DurationWithHumanReadable>` — **the engine computes ETA**. Speeds are `Speed { mbps: f64 }`, i.e. MiB/s floats, not bytes/sec integers. | `torrent_state/stats.rs:9`, `:184` |
| V-10 | `ManagedTorrent::wait_until_completed()` and `wait_until_initialized()` return futures — **completion is an awaitable edge, not something to discover by polling**. | `torrent_state/mod.rs:512,532` |
| V-11 | `Session::{add_torrent, pause, unpause, delete(id, delete_files), with_torrents, get, stop, get_dht, tcp_listen_port}`. | `session.rs:900,1393,1399,1233,891,1220,872,837,1416` |
| V-12 | **librqbit persists its own session state**: `SessionPersistenceConfig::{Json { folder }, Postgres { .. }}`, with `default_json_persistence_folder()`. | `session.rs:367,375,397` |
| V-13 | `stats.error: Option<String>` — engine errors are readable from the polled stats struct, not only from an event stream. | `torrent_state/stats.rs:73` |

## 4. What these facts change

**V-3 is the headline.** The single worst thing about the reference product
(torlink compiling a C++ WebRTC module on `postinstall` with a 300-second timeout,
then degrading silently when it fails) has no equivalent here. The engine choice
is sound and the "lighter install" goal is achieved by construction.

**V-2 deserves an honest caveat.** 293 crates is not obviously "leaner" than
torlink's 222 npm production packages when counted as nodes in a graph. The win is
real but it is *link-time, not count-time*: dead-code elimination, one static
artifact, no runtime resolution, no postinstall. When we claim "lighter" in the
README we should mean **binary size and RSS**, and we should have measured numbers
(see the new NFRs proposed in `plan-engine.md` §4).

**V-6/V-7/V-8 hit the shared types freeze directly** and are the reason that freeze
has to happen before any track builds against it:

- Our five normative statuses (`queued|downloading|failed|seeding|missing`) are a
  **harbour-side projection**, not an echo of the engine. The mapping has to be
  written down once, by the engine track, or three people will invent three.
- The projection cannot be total without a `paused` status, which the vocabulary
  currently lacks while `FR-43`, `FR-47` and `FR-53` all require it.
- `peers` is not a flat field. `FR-32` and `FR-44` both render it, so the types
  must express "peers is unknown while not live" rather than defaulting to `0`,
  which would make a paused seed indistinguishable from a peerless one.

**V-9/V-10 are free performance.** ETA is the engine's, so we don't compute it; and
because completion is awaitable, the 500 ms poll only ever needs to cover items
that are actively transferring. A seedbox with 200 idle seeds should not be doing
400 stat reads per second to learn nothing.

**V-12 removes work.** `downloads.json` must not duplicate what librqbit already
stores — `FR-50` already says piece-level resume comes from the session, so
`FR-48`'s "and progress" field is redundant with it and costs a whole-file rewrite
on every poll tick.

**V-4 is a phase-7 shortcut.** `http-api`, `webui` and `watch` are upstream
features. Most of the deferred headless-daemon spike is "turn on a feature flag and
write a compatibility shim", not "build four daemons".

## 5. Remaining behavioural spike (E1)

Static analysis cannot answer these. Each is a pass/fail gate:

1. Real tiny magnet: add → metadata → download → finish → seed.
2. `kill -9` mid-download → relaunch → **resumes with no rehash** (proves fastresume).
3. Delete the file out from under a seed → **record what `TorrentStats` actually
   reports**. This is the input to the missing-file detector; the torlink constants
   (2 polls, 10 s grace) were derived for webtorrent and must not be copied blind.
4. `pause`/`unpause` round trip; `delete(id, delete_files: true)` while a handle is
   open (Windows file-locking is the risk here, not Linux).
5. Measure: `Session::new` cost, RSS at 1 and 20 torrents, release binary size.
6. Decide `default-tls` vs `rust-tls` (rustls avoids a system OpenSSL dependency on
   Linux — likely the right call for a portable static binary).

**No-go fallback**, recorded now so it is not invented under pressure: if fastresume
or the missing-file signal fails, fall back to driving the `rqbit` binary over its
HTTP API (V-4 `http-api`) as a sidecar. It costs the "single binary" property, which
is a product decision for Ishan, not an engineering dead end.

## 6. Recommendation

**Pin `librqbit = "8.1.1"` (exact) and stay off `9.0.0-rc.0`.** Rationale: 8→9 is a
breaking major mid-flight for a dependency three subsystems rest on, and we gain
nothing from an rc during M0. Revisit after v1 ships. Record the pin in `Cargo.toml`
with a why-comment at the decision site, per `AGENTS.md` rule 6.
