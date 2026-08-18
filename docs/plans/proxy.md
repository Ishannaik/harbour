# Proxy (global + per-subsystem)
Ref: #57

## Goal
Make harbour's proxy support honest and complete: one validated SOCKS5 proxy for the swarm, an
independent proxy lane for harbour's own HTTP (indexer/search), real auth, and a proxy-only mode
that names exactly what it does and does not tunnel.

## The finding that shapes this plan

Read on **2026-08-16** from the vendored crate sources harbour actually compiles against
(`~/.cargo/registry/src/*/librqbit-8.1.1`, `reqwest-0.13.4`) and from `librqbit 9.0.0`
extracted from `static.crates.io` to `/tmp/librqbit-9.0.0`. crates.io API (with a UA header)
reports `max_stable_version 9.0.0`, `updated_at 2026-08-15` — docs.rs and web search still index
8.1.1, the API and the downloadable `.crate` are authoritative.

**1. harbour already ships half of this issue, undocumented.** `Config.socks_proxy_url`
(`src/persist.rs:82-83`), `TextField::SocksProxy` (`src/ui/settings.rs:71`), row 14
`"SOCKS5 Proxy URL"` (`src/ui/settings.rs:151`), commit arm (`src/app/settings.rs:217-223`),
`EngineLaunchOptions.socks_proxy_url` (`src/engine/rqbit.rs:585`) → `SessionOptions`
(`src/engine/rqbit.rs:246`). **SPEC.md contains the word "proxy" zero times outside the indexer
source.** The commit arm does **no validation at all** — any string is saved, and the failure
surfaces one launch later as a generic `"downloads are unavailable: …"` banner. This issue must
retro-spec and fix what shipped before it adds anything.

**2. librqbit accepts `socks5://` and nothing else — at both 8 and 9.**

```rust
// librqbit-8.1.1/src/stream_connect.rs:14-16  ==  librqbit-9.0.0/src/stream_connect.rs:72-75
let url = ::url::Url::parse(url).context("invalid proxy URL")?;
if url.scheme() != "socks5" {
    anyhow::bail!("proxy URL should have socks5 scheme");
}
```

So **SOCKS4, SOCKS4a, `socks5h` and HTTP proxies are not available for the swarm at any version**.
Auth is URL userinfo only, mapped to `Socks5Stream::connect_with_password`
(`stream_connect.rs:98-106` in 9.0.0).

**3. There is no per-subsystem scope inside librqbit — one URL covers both subsystems.**
`session.rs:693-736` (9.0.0) parses the single `proxy_url` into *both* the `StreamConnector`
(outgoing peer TCP) and the session's `reqwest::Client` via `reqwest::Proxy::all`. That client is
what serves HTTP/HTTPS tracker announces (`session.rs:1578`), `torrent_from_url`
(`session.rs:1124`) and the blocklist fetch. **"Peers" and "trackers" cannot be scoped
separately**; the only real scope boundary is harbour's own HTTP client (item 6).

**4. A set proxy has no direct fallback — which is the good news.**

```rust
// librqbit-9.0.0/src/stream_connect.rs:228-238
if let Some(proxy) = self.proxy_config.as_ref() {
    let (r, w) = self.with_stat(ConnectionKind::Socks, addr.is_ipv6(), proxy.connect(addr)).await?;
    return Ok((ConnectionKind::Socks, ...));   // early return: no TCP, no uTP
}
```

A dead proxy means **zero peers**, never a silent direct connection. That is the behavior harbour
wants and the FR must lock in so a future "fallback to direct" convenience patch is rejected on
sight.

**5. The leaks are real and upstream already tracks them.** `ikatson/rqbit` issue **#493
"Various SOCKS proxy leaks"** (open, created 2025-10-06, updated 2025-10-16, read via
`gh api repos/ikatson/rqbit/issues/493` on 2026-08-16) lists, independently of this reading:
UDP tracker announces still fire; reqwest does **local DNS resolution** for `socks5://` (proxied
resolution needs `socks5h`/`socks4a`, which librqbit rejects), same for blocklist and
torrent-file URLs; the DHT stays on; and HTTP3 could bypass the proxy. Confirmed in-tree: the
UDP tracker client (`session.rs:761`) and the DHT (`session.rs:606-631`) are built with no
reference to the proxy config at all. Add the listener: `listen`/UPnP still accept inbound
connections on the real IP. **Any "proxy-only" claim harbour makes must enumerate these.**

**6. harbour's own HTTP is a second, more capable lane.** `reqwest 0.13.4` supports
`socks4 | socks4a | socks5 | socks5h` (`src/proxy.rs:130`, `:723-745`) plus `http`/`https`,
gated on the feature `socks = []` (`Cargo.toml:178`) — an **empty feature list**: reqwest 0.13
implements SOCKS itself (`src/connect.rs`, `#[cfg(feature = "socks")]` throughout).
**Enabling it adds zero crates.** harbour builds three such clients today:
`HttpSource::new` (`src/sources.rs:148`), the fresh streaming client (`src/sources.rs:308`), and
the indexer health/media clients (`src/ensure_indexer.rs:71,187`, `src/app/watch.rs:312`).
`resolve_indexer_url` (`src/sources.rs:113-140`) resolves to `http://127.0.0.1:<port>` unless
`HARBOUR_INDEXER_URL` points elsewhere — **so the search lane is loopback by default and must
never be proxied**, which is exactly why search scope differs, as the issue suspected.

**7. librqbit 9 supplies the kill-switch parts 8.1.1 lacks.** `SessionOptions`
(`session.rs:418-481`): `dht: Option<DhtSessionConfig>` (None = off), `listen:
Option<ListenerOptions>` (None = no inbound listener and no UPnP), `disable_local_service_
discovery: bool` (LSD multicast announces your LAN IP), and `Session::stats_snapshot()`
(`session_stats/mod.rs:74`) whose `peers.live_socks` counter (`snapshot.rs:91`) is the only
observable proof that peer traffic is really going through the proxy.

## SPEC / FR reference

**Missing from SPEC — add first, then implement.** Proposed **FR-113 … FR-118**, in the
"Connection & protocol" section that `docs/plans/protocol-toggles.md` (#53) /
`port-forwarding-and-binding.md` (#55) create (SPEC's current §4.5 is *Seeding*; whichever plan
lands first creates the section).

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then. **The settings-row indices claimed below are
> provisional for the same reason**: the parallel batch (`speed-limits`, `share-limits`,
> `protocol-toggles`, `encryption-mode`, and especially `categorized-settings`, which may
> restructure the rows entirely) also adds rows.

- **FR-113 (swarm proxy).** harbour can route the BitTorrent swarm through one SOCKS5 proxy,
  configured as `socks5://[user:pass@]host:port`. It is boot-time: changing it persists and
  applies at the next launch, which the row says. When set it covers **outgoing peer connections
  and HTTP/HTTPS tracker announces together — they are one switch, not two** — and there is **no
  fallback to a direct connection**: an unreachable proxy means no peers, loudly, never
  unproxied traffic.
- **FR-114 (unsupported swarm schemes).** SOCKS4, SOCKS4a, `socks5h` and HTTP/HTTPS proxies are
  **not supported for the swarm**. The engine hard-rejects any scheme but `socks5`
  (librqbit 8.1.1 and 9.0.0, verified 2026-08-16), and harbour does not implement its own peer
  transport. The settings row rejects them at entry with that reason rather than saving a value
  that breaks the next launch. Re-evaluated on each librqbit upgrade; tracked upstream as
  ikatson/rqbit#493.
- **FR-115 (app-HTTP proxy scope).** harbour's own HTTP requests — search and magnet resolution
  against the indexer — have a **separate** on/off proxy setting, because the indexer is a
  local service by default. When on, they use the same configured proxy URL and, on this lane,
  `socks4`/`socks4a`/`socks5`/`socks5h`/`http`/`https` are all accepted. **Loopback destinations
  are never proxied**, whatever the setting: `127.0.0.1`, `::1` and `localhost` bypass, so
  turning the toggle on can never break a local indexer. Default off.
- **FR-116 (auth).** Proxy credentials are carried as userinfo in the URL. They are stored in
  `config.toml` in plain text, like every other value in that file; the settings row masks the
  password when rendering so a screen-share or screenshot does not leak it.
- **FR-117 (proxy-only mode).** With a proxy set, proxy-only mode disables the paths harbour
  can prove would bypass it: the DHT, LAN service discovery, the inbound listener (and with it
  UPnP), and `udp://` trackers, which are stripped from magnets before adding because UDP cannot
  traverse a TCP SOCKS proxy. It **overrides** the DHT and UPnP settings for that launch and
  says so in the UI rather than silently contradicting them. It is **leak reduction, not a
  guarantee**: DNS for HTTP tracker hostnames is still resolved locally (the engine rejects
  `socks5h`), `.torrent` files added from disk keep their `udp://` trackers, and harbour does not
  and cannot enforce OS-level routing. FR text names these; the UI row says "reduces leaks" and
  the detail text lists them. See ikatson/rqbit#493.
- **FR-118 (validation).** A proxy URL is validated when the settings row is committed: scheme
  from the allowed set, host present, port present. A rejected value keeps the edit open with a
  banner naming what was wrong and **leaves the saved config untouched** — a proxy setting that
  fails one launch later is indistinguishable from a broken network.

## Workstream

- **Steps 1, 2 (SPEC, entry validation)** — **Terminal UI (Ishan)** for the row/commit arm,
  with the SPEC text reviewed by Sarthak.
- **Steps 3 (app-HTTP lane)**, **5 (proxy-only)**, **6 (live-socks accessor)** — **Engine &
  Foundation (Sarthak)**: config schema, the shared HTTP client constructor and the librqbit
  mapping are load-bearing contracts. Coordinate with **Dhruv** only in that a *remote* indexer
  deployment is the sole case where FR-115 does anything; no `harbour-indexer` change is needed.
- **Step 4 (librqbit 9 upgrade)** — **Engine (Sarthak)**, specified once as step 0 of
  `docs/plans/protocol-toggles.md`. Blocks steps 5–6 only; steps 1–3 ship on 8.1.1 today.

**Shared types:** one additive method on the frozen `Engine` trait in `src/core/types.rs` —
`fn proxied_peers(&self) -> Option<u32> { None }` — following the `set_speed_limits`
(`src/core/types.rs:694`) precedent: a default body so `FakeEngine` and every other implementor
keep compiling. Sarthak owns it; the UI builds against it once it lands.

**Row-table prerequisite** (stated identically in this batch's sibling plans): the settings
row-table refactor from step 1 of `docs/plans/speed-limits.md` (#43) /
`categorized-settings.md` (#63) lands first, so rows are identified by value, not by the index
literals currently duplicated across `src/ui/settings.rs:100-155` and
`src/app/settings.rs:36-59`.

## Approach

**Step 1 — SPEC FR-113…FR-118 (docs only, ~55 lines).** Includes retro-speccing the SOCKS5 row
that already ships. No code.

**Step 2 — validate the URL at commit (UI, ~60 lines, ships on librqbit 8.1.1).**

A pure helper next to `parse_opt_number` (`src/app/settings.rs:78-87`), unit-testable with no
engine:

```rust
/// Allowed schemes differ per lane: the engine hard-rejects anything but
/// socks5 (librqbit stream_connect.rs:72), while harbour's own reqwest client
/// speaks socks4/4a/5/5h and http(s). Validating against the *swarm* set here
/// is deliberate: the same URL feeds both lanes, and the stricter one wins.
fn parse_proxy_url(text: &str) -> Result<Option<String>, String>;
```

Empty → `Ok(None)`. `socks5://host:1080` → `Ok(Some(..))`. `socks5h://…`, `http://…`,
`socks5://host` (no port), `host:1080` (no scheme), `banana` → `Err` with the reason, and the
commit arm does `app.warn(msg); return;` — edit stays open, config untouched (the established
never-guess contract, `src/app/settings.rs:148-152`). The row value renders the password as
`***` (FR-116).

**Step 3 — the app-HTTP lane (engine + sources, ~90 lines, ships on 8.1.1).**

`Cargo.toml`: add `"socks"` to the existing `reqwest` feature list. Zero new crates — the feature
is `socks = []` and reqwest implements it in-tree (finding 6).

One constructor in `src/sources.rs`, used by **both** existing build sites (`:148` and `:308`),
so the streaming client can never diverge from the search client:

```rust
/// The shared indexer HTTP client. Loopback is excluded unconditionally: the
/// indexer resolves to 127.0.0.1 by default (`resolve_indexer_url`), and
/// sending a local request through a remote proxy would break search for every
/// user who merely turned the toggle on.
fn indexer_client(proxy: Option<&str>) -> reqwest::Client;
```

`reqwest::Proxy::all(url)?.no_proxy(NoProxy::from_string("localhost,127.0.0.1,::1"))`
(`reqwest-0.13.4/src/proxy.rs:361`, `:506`). A proxy URL that reqwest rejects is a **loud
warning and an unproxied client is not built** — `HttpSource::new`'s existing
`unwrap_or_else(|err| … reqwest::Client::new())` fallback (`src/sources.rs:152-155`) must not be
reached with a proxy configured; that path is exactly the silent fallback the project forbids.

`Config` gains `proxy_search: bool` (default `false`). `src/ensure_indexer.rs` and
`src/app/watch.rs` stay **unproxied by design** — both talk to loopback only (indexer health,
the local stream server); a comment at each site says so, so nobody "completes" the feature later.

**Step 4 — librqbit 9 upgrade.** Owned by `docs/plans/protocol-toggles.md` step 0; not restated
here. `socks_proxy_url` moves from `SessionOptions` into
`SessionOptions.connect = Some(ConnectionOptions { proxy_url, .. })`
(`librqbit-9.0.0/src/stream_connect.rs:34-41`) — a one-line move in
`src/engine/rqbit.rs:246`.

**Step 5 — proxy-only mode (engine, ~110 lines, needs step 4).**

`Config` gains `proxy_only: bool` (default `false`), threaded through `EngineLaunchOptions`.
In `RqbitEngine::new`, when a proxy is set **and** `proxy_only`:

```rust
// Everything below is UDP or inbound, and a TCP SOCKS proxy carries neither
// (rqbit#493). Turning them off is the only way "proxy-only" can be true;
// leaving them on would make the setting a lie. DHT/UPnP settings are
// overridden for this launch — the settings rows say so.
dht: None,
listen: None,                            // no inbound listener, therefore no UPnP
disable_local_service_discovery: true,
```

`proxy_only` with **no** proxy configured is a no-op plus a startup warning — never a silent
"safe" mode that just breaks downloading.

`udp://` trackers: one pure helper in `src/core/magnet.rs`,

```rust
/// Removes `tr=udp://…` from a magnet. Called only in proxy-only mode: a UDP
/// announce sends the real IP to the tracker, and UDP cannot traverse a TCP
/// SOCKS proxy, so stripping costs nothing that worked. HTTP(S) trackers stay —
/// they go through the session's proxied client and are the only peer source
/// left once DHT is off.
pub fn strip_udp_trackers(magnet: &str) -> String;
```

applied at the single choke point `RqbitEngine::add` (`src/engine/rqbit.rs:423-435`), where
`req.magnet` is handed to `AddTorrent::from_url`, plus a filter on `SessionOptions.trackers`
built from `config.trackers` (FR-51's custom tracker list). **Known ceiling, stated in FR-117:**
`add_bytes` (`.torrent` files) is *not* filtered — that needs bencode rewriting, which is out of
scope for this issue. Future `.torrent` add paths (`add-torrents.md`, `watch-folders.md`) either
call the same helper on the magnet form or inherit this limitation; the helper's rustdoc says so.

**Step 6 — prove it (engine + UI, ~60 lines, needs step 4).**

`Engine::proxied_peers()` → `session.stats_snapshot().peers.live_socks`
(`librqbit-9.0.0/src/session_stats/mod.rs:74`, `snapshot.rs:91`), `None` in `FakeEngine`. A
read-only `Info` row renders:

- `3 peers via proxy` when the proxy is carrying traffic
- `0 — proxy configured, no peers yet` when set but nothing has connected
- `not in use` when no proxy is configured

**This row is the feature.** Everything above it is plumbing the user cannot check; this is the
line that distinguishes "my proxy works" from "my proxy is silently dead", which with FR-113's
no-fallback rule look identical in the downloads view. It reuses the `RowKind::Info` variant
introduced by `docs/plans/encryption-mode.md` (#54) / `port-forwarding-and-binding.md` (#55) —
whichever lands first adds the variant, the rest just add rows.

## Files to create / modify

- `SPEC.md` — FR-113…FR-118 in the Connection & protocol section.
- `Cargo.toml` — `"socks"` added to `reqwest`'s feature list (no new dependency).
- `src/persist.rs` — `proxy_search: bool` (default false), `proxy_only: bool` (default false);
  round-trip + old-config-loads tests alongside the existing `socks_proxy_url` test (`:574`).
- `src/app/settings.rs` — `parse_proxy_url` + its unit tests; the `TextField::SocksProxy` commit
  arm (`:217-223`) validates instead of accepting anything; two new toggle arms.
- `src/ui/settings.rs` — row 14 relabelled `SOCKS5 Proxy (peers + trackers)`; new rows
  `Proxy Search Requests`, `Proxy-Only Mode`, and the `Peers via Proxy` info row; password
  masking in the value renderer (`:366-367`).
- `src/sources.rs` — `indexer_client(proxy)`; both client build sites (`:148`, `:308`) use it.
- `src/ensure_indexer.rs`, `src/app/watch.rs` — a why-comment stating these stay unproxied
  (loopback only). No behavior change.
- `src/core/magnet.rs` — `strip_udp_trackers` + tests.
- `src/core/types.rs` — `Engine::proxied_peers()` with an additive `None` default (Sarthak).
- `src/engine/rqbit.rs` — `EngineLaunchOptions.{proxy_search unused, proxy_only}`; the
  proxy-only `SessionOptions` arm and its why-comment; `strip_udp_trackers` at the add choke
  point; `proxied_peers()`.
- `src/engine/fake.rs` — `proxied_peers()` returns `None`.
- `src/ui/tests.rs` — buffer snapshots for the masked URL, the three info-row states, and the
  proxy-only row.

## Key APIs / libraries

Verified 2026-08-16 by reading vendored/extracted crate sources and the upstream tracker:

- Scheme rejection — `librqbit-8.1.1/src/stream_connect.rs:14-16`,
  `librqbit-9.0.0/src/stream_connect.rs:72-75`.
- One URL → peers **and** HTTP client — `librqbit-9.0.0/src/session.rs:693-736`; tracker use at
  `:1578`; `torrent_from_url` at `:1124`.
- No direct fallback when proxied — `librqbit-9.0.0/src/stream_connect.rs:228-238`.
- 9.0.0 `ConnectionOptions { proxy_url, enable_tcp, peer_opts }` — `stream_connect.rs:34-41`;
  `SessionOptions { dht, listen, connect, disable_local_service_discovery, trackers, .. }` —
  `session.rs:418-481`.
- Proof counter — `Session::stats_snapshot()` `session_stats/mod.rs:74`;
  `peers.live_socks` `session_stats/snapshot.rs:91`.
- reqwest scheme set and feature — `reqwest-0.13.4/src/proxy.rs:130`, `:723-745`;
  `Cargo.toml:178` (`socks = []`); `Proxy::no_proxy` `proxy.rs:361`, `NoProxy::from_string`
  `proxy.rs:506`.
- Leak inventory — [ikatson/rqbit#493 "Various SOCKS proxy leaks"](
  https://github.com/ikatson/rqbit/issues/493), open, read 2026-08-16.
- librqbit 9.0.0 is current stable — crates.io API `max_stable_version 9.0.0`,
  `updated_at 2026-08-15T09:05:58Z` (docs.rs and web search still show 8.1.1; stale index).
- **ratatui exposes nothing proxy-related** — it is a terminal renderer; every proxy control here
  is ordinary settings-row state rendered by the existing `draw` (`src/ui/settings.rs`). Checked
  so the question is not re-asked.

**New crates: none.** URL parsing is `reqwest`'s and `url`'s, already present; the SOCKS
implementations are in reqwest and librqbit, both already in the tree.

## Risks / edge cases

- **Rejected: harbour-side SOCKS4/HTTP peer transport.** Making the swarm speak other proxy
  protocols means owning the peer socket, i.e. re-implementing what librqbit's `StreamConnector`
  does. AGENTS rule 8 and a maintenance burden forever. FR-114 documents the limit and points at
  rqbit#493; a user who needs SOCKS4/HTTP uses an OS-level tunnel.
- **Rejected: fallback to direct when the proxy is down.** It is the most requested "fix" and it
  is a privacy hole: the user who set a proxy would be deanonymised precisely when the proxy
  fails. FR-113 forbids it in writing.
- **The DNS leak survives this issue.** HTTP tracker hostnames resolve locally because the engine
  rejects `socks5h`. Named in FR-117 and in the proxy-only row's detail text. The honest framing
  is "your IP is not in the tracker announce; your resolver still saw the hostname."
- **Proxy-only with no HTTP trackers finds nothing.** DHT off + `udp://` stripped can leave a
  magnet with zero usable trackers, and the download sits at 0 peers. Expected, and the reason
  the FR says leak *reduction*: the alternative (leave UDP on) is the false promise. The
  `Peers via Proxy` info row plus the existing peer count make the state visible instead of
  mysterious.
- **`.torrent` files keep their udp:// trackers in proxy-only mode.** Stated in FR-117 and in
  the helper's rustdoc so it is a known ceiling, not a surprise. Upgrade path: bencode rewrite in
  the `add_bytes` path, when someone actually needs it.
- **A loopback indexer plus a naive proxy toggle would break search.** The unconditional
  `NoProxy` list is the guard; the test in step 3 asserts it, because this is the failure a user
  would report as "harbour's search died when I turned on my VPN proxy".
- **Credentials in `config.toml` in plain text.** Same as every other value there; masking is
  display-only. Called out in FR-116 rather than implied. A keyring dependency is out of scope
  and would be the same argument the debrid plan already settled.
- **`proxy_only` overriding DHT/UPnP could look like a bug.** The rows for DHT and UPnP must
  render `overridden by proxy-only` rather than their stored value — the same honesty rule
  `port-forwarding-and-binding.md` applies to the random-port row.
- **Do not fold `proxy_search` into `EngineLaunchOptions` semantics.** It configures harbour's
  reqwest client, not the engine; putting it in the engine options would imply librqbit reads it.

## Test strategy

- **Unit, `src/app/settings.rs`** — `parse_proxy_url`: empty ⇒ `None`; `socks5://h:1080` and
  `socks5://u:p@h:1080` ⇒ accepted; `socks5h://h:1080`, `http://h:8080`, `socks4://h:1080`,
  `socks5://h`, `h:1080`, `banana` ⇒ `Err`, each with a distinct reason. Committing a rejected
  value leaves `config.socks_proxy_url` unchanged, keeps `editing == true`, and warns.
- **Unit, `src/core/magnet.rs`** — `strip_udp_trackers` drops every `tr=udp://…` (including
  percent-encoded and repeated params), keeps `tr=http://` / `tr=https://`, keeps `xt`/`dn`
  byte-identical, and is a no-op on a magnet with no trackers.
- **Unit, `src/persist.rs`** — `proxy_search`/`proxy_only` round-trip; a config written before
  this change loads with both `false`.
- **Unit, `src/sources.rs`** — `indexer_client(Some("socks5://127.0.0.1:9"))` builds, and a
  request to `http://127.0.0.1:<port>/health` still succeeds against a local stub server,
  proving the loopback bypass (the proxy port is deliberately dead, so any attempt to use it
  fails the test).
- **Unit, `src/engine/rqbit.rs`** — a pure `fn proxy_only_overrides(proxy: Option<&str>,
  proxy_only: bool) -> Overrides` so the DHT/listen/LSD decisions are asserted without building
  a `Session`: proxy + on ⇒ all three disabled; no proxy + on ⇒ nothing disabled (plus the
  warning); proxy + off ⇒ nothing disabled.
- **Buffer snapshot, `src/ui/tests.rs`** — the proxy row masks `socks5://u:p@h:1080` as
  `socks5://u:***@h:1080`; the info row renders all three states; DHT/UPnP rows read
  `overridden by proxy-only` when that mode is on.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — with a local SOCKS5 server
  (`ssh -D` or any test proxy; skipped when `HARBOUR_TEST_SOCKS` is unset), a real tiny magnet
  reaches `proxied_peers() > 0`; with the proxy pointed at a closed port, peers stay `0` and the
  download makes no progress — the no-fallback assertion.

## Verification

1. `cargo run` → `shift+S` → the proxy row reads `SOCKS5 Proxy (peers + trackers)`, and typing
   `http://localhost:8080` + Enter shows a banner naming the scheme, keeps the edit open, and
   leaves `~/.harbour/config.toml` unchanged.
2. Run a real SOCKS5 proxy (e.g. `ssh -D 1080 …`), set `socks5://127.0.0.1:1080`, relaunch,
   start a well-seeded magnet: downloads progress and the **`Peers via Proxy` row shows a
   non-zero count**. That number is the proof — it comes from librqbit's `live_socks` counter,
   which only increments on connections that completed a SOCKS handshake.
3. Stop the proxy, relaunch: the same magnet gets **0 peers and 0 B/s**, with no traffic on the
   direct interface (`netstat -ano | findstr <peer port>` shows nothing). No silent fallback.
4. Turn on `Proxy Search Requests` with the default local indexer: **search still works** —
   proof the loopback bypass is in place. Point `HARBOUR_INDEXER_URL` at a remote indexer with
   the toggle on and off and confirm from the proxy's log which case it sees.
5. Turn on `Proxy-Only Mode`, relaunch: DHT and UPnP rows read `overridden by proxy-only`, and a
   magnet containing `tr=udp://…` is added with those trackers gone (visible in the engine's
   persisted session JSON under `~/.harbour/engine/`); no UDP traffic leaves the machine for
   that torrent (`netstat`/Wireshark on the tracker port).
6. Set `Proxy-Only Mode` with no proxy URL: a startup warning says it does nothing, and
   downloading still works normally — the mode never half-breaks the client.
7. `grep -n -i proxy SPEC.md` returns FR-113…FR-118, including the retro-specced row that
   shipped before this issue, and FR-114/FR-117 name the unsupported schemes and the residual
   leaks with the upstream link.
