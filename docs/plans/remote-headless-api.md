# Remote / headless control API
Ref: #67

## Goal
A small, harbour-shaped HTTP API for automation and remote control — token-authenticated,
loopback by default, off by default — and deliberately **not** qBittorrent WebUI parity.

## The two findings that anchor this plan

Read on **2026-08-16** from the harbour source and the exact librqbit harbour compiles against,
`~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/`.

### Finding 1 — harbour already serves an unauthenticated, fully state-modifying HTTP API

```rust
// src/engine/rqbit.rs:82-92
let api = Api::new(self.session.clone(), None, None);
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
tokio::spawn(async move {
    let http = HttpApi::new(api, None);          // <-- opts = None
    let _ = http.make_http_api_and_run(listener, None).await;
});
```

That `None` is the finding. In librqbit 8.1.1:

```rust
// src/http_api/mod.rs:35-37
pub struct HttpApiOptions {
    pub read_only: bool,
    pub basic_auth: Option<(String, String)>,
}
// :75  pub fn new(api: Api, opts: Option<HttpApiOptions>) -> Self
// :83  /// If read_only is passed, no state-modifying methods will be exposed.
```

`None` means `read_only = false` and no auth. And the gated route set is real
(`src/http_api/handlers/mod.rs:66-120`): the **always-on** block includes
`GET /torrents/{id}/stream/{file_id}` — the one route harbour actually wants — while the
`if !state.opts.read_only` block adds `POST /torrents` (add by magnet **or `http://` URL**, from
the route's own doc string at `:55`), `/torrents/limits`, and per-torrent
`pause`/`start`/`forget`/**`delete`**.

So today, the first time a user presses `w` to watch anything, harbour opens a loopback port on
which **any local process can delete their torrents and their files**, and can make harbour fetch
an arbitrary URL. It also enables a CORS layer allowing `http://localhost:3031`,
`http://localhost:1420` and `tauri://localhost` origins (`http_api/mod.rs:104-118`), which widens
that to a web page the user visits, and honours a `CORS_ALLOW_REGEXP` environment variable.

**Step 0 of this issue is therefore a security fix that must ship first and can ship alone.**
Pass `Some(HttpApiOptions { read_only: true, basic_auth: None })`. Streaming is unaffected — its
route is registered above the gate. This is a ~5-line change and it is the highest-value work in
#67. The implementer must re-read those two files at implementation time to confirm the line
numbers still hold for whatever librqbit version is pinned then.

### Finding 2 — the API is a SPEC violation as SPEC is currently written

> **NFR-10 (Security/Privacy)** Files stay on disk: no uploads, no central server, no telemetry,
> **no network calls beyond source fetching, tracker/DHT traffic for the user's own transfers,
> and the loopback stream endpoint (FR-61)**.

A remote-control listener is not in that list. SPEC is the referee, so **the SPEC amendment is
mandatory and lands first** — not as paperwork, but because writing the constraint down is what
keeps the API from growing into a WebUI.

## SPEC / FR reference

**Exists today.** `NFR-10` (network surface, above), `NFR-11` (path safety: cache/ledger paths
derive from info-hashes, never from untrusted names — the API must not introduce a second path
source), `FR-61` (the loopback stream endpoint), `NFR-15` (no subsystem failure escalates).

**Missing from SPEC — add first, then implement.** Proposed new **§4.9 Remote control
(FR-84 … FR-89)** and an amendment to NFR-10.

> **FR numbers here are provisional.** Several plans in `docs/plans/` independently propose
> FR-8x/FR-9x (see `speed-limits.md`, `debrid-support.md`, `queue-management.md`); allocate the
> real numbers against `SPEC.md` when the SPEC commit is written, in merge order. The *content*
> of FR-84…FR-89 below is what matters and does not overlap any sibling plan.

- **FR-84 (opt-in).** The remote API is **disabled by default**. Enabling it requires an explicit
  config change. A default install never opens a control port.
- **FR-85 (bind).** It binds `127.0.0.1` by default. A non-loopback bind is permitted only when a
  token exists, and harbour prints a one-line warning naming the bind address at startup.
- **FR-86 (auth).** Every request outside `GET /v1/health` carries `Authorization: Bearer
  <token>`. The token is 32 random bytes, hex-encoded, generated on first enable and stored at
  `<state>/api-token` with owner-only permissions. Comparison is constant-time. There is no
  username/password, no cookie, no session.
- **FR-87 (auth limits).** Failed authentications are rate-limited per source address; the
  listener caps concurrent connections. A wrong token yields `401` with no detail about why.
- **FR-88 (no server-side fetch — the SSRF rule).** The API accepts torrents **only** as a
  `magnet:` URI or as raw `.torrent` bytes in the request body. It never accepts a URL for
  harbour to fetch. A request containing an `http(s)://` torrent source is rejected `400` naming
  the reason. This is the SSRF mitigation *and* the scope boundary, in one rule.
- **FR-89 (not qBittorrent).** The API is versioned under `/v1`, harbour-shaped, and explicitly
  does not implement `/api/v2/*` or ship a web UI. Compatibility with qBittorrent clients is a
  non-goal; a request for it is a request for a different product.
- **NFR-10 (amended).** …and, when explicitly enabled (FR-84), the loopback remote-control
  listener. Additionally: harbour's embedded librqbit HTTP API is started in read-only mode
  (`HttpApiOptions::read_only`), so the streaming endpoint never exposes state-modifying routes.

The NFR-10 amendment's second sentence is Finding 1 turned into a contract with a test.

## Workstream

**Engine & Foundation (Sarthak).** All of it. The command enum borders the frozen shared-types
contract (`TorrentResult`, `QueueStatus`, the engine event enum) and the auth/bind decisions are
load-bearing security invariants — this is exactly the category AGENTS.md assigns to the expert.

**This is a separate track from the TUI.** No `src/ui/*` file is touched by steps 0–5. The only
TUI-adjacent work is step 6's status indicator, and it is optional.

Shared-type dependencies: **read-only consumers of `ItemView`, `QueueItem`, `QueueStatus`,
`TorrentResult`, `EngineEvent`.** The API must **serialize projections of** those types, not
re-export them — a JSON wire format that is literally `#[derive(Serialize)]` on a shared type
freezes the shared type against every future refactor. Define `src/remote/wire.rs` DTOs.

## Approach

**Step 0 — harden the existing listener (ships alone, ~10 lines).** `HttpApiOptions { read_only:
true, basic_auth: None }` in `src/engine/rqbit.rs::stream_server`, with a why-comment citing
`librqbit-8.1.1/src/http_api/handlers/mod.rs:92` and the route list. Plus a test that the stream
URL still resolves. **Merge this before anything else in the issue**; it is a fix, not a feature.

**Step 1 — SPEC (docs only).** §4.9 and the NFR-10 amendment.

**Step 2 — the command channel (~150 lines). The architectural decision of this issue.**

The API must **not** share `Queue` behind a mutex. `Queue` is mutated exclusively by the app loop
(`app/mod.rs:491-524`), and every view is documented as pure paint with "the loop owns mutations"
(`ui/mod.rs:5`). A second writer would break the one invariant the whole architecture rests on,
and would deadlock against the `tokio::select!` that already holds `&mut app`.

Instead, API requests become messages, exactly like terminal input:

```rust
// src/remote/command.rs
pub enum RemoteCommand {
    Status(oneshot::Sender<StatusDto>),
    ListDownloads(oneshot::Sender<Vec<DownloadDto>>),
    AddMagnet { magnet: String, reply: oneshot::Sender<Result<String, String>> },
    AddTorrentBytes { bytes: Vec<u8>, reply: oneshot::Sender<Result<String, String>> },
    Pause  { id: String, reply: oneshot::Sender<Result<(), String>> },
    Resume { id: String, reply: oneshot::Sender<Result<(), String>> },
    Remove { id: String, delete_files: bool, reply: oneshot::Sender<Result<(), String>> },
}
```

A **bounded** `mpsc::Receiver<RemoteCommand>` (capacity 64) becomes a **fourth arm** of the
existing `tokio::select!` in `run()`, next to `input.recv()` and `events_rx.recv()`, with the
same drain-the-rest loop. Bounded, unlike the input channel: the input channel's `unbounded` is
safe because a human types at human speed, and an HTTP client does not — a full channel returns
`503` rather than growing without limit (see Risks).
Handlers call the `Queue` methods that already exist and are already used by
the TUI: `add`, `pause`, `resume`, `remove`, `views`, `active_count`
(`src/queue.rs:208,342,362,386,172,190`). **No new engine capability is built** — the API is a
second front end onto the same command set the keyboard drives, which is the only way its
behaviour can be guaranteed to match the TUI's.

**Step 3 — auth + the listener (~220 lines).**

`src/remote/auth.rs`:
- Token generation: 32 bytes from `rand` (0.9.5, `Cargo.lock:2780`, via librqbit), hex-encoded.
- Storage: `<state>/api-token`, written through `Store`'s existing atomic temp-then-rename;
  `0600` on unix via `std::os::unix::fs::PermissionsExt`, `#[cfg]`-gated.
- Verification: `subtle::ConstantTimeEq` (`Cargo.lock:3588`). Not a hand-rolled loop — a
  short-circuiting `==` on a secret is a timing oracle, and hand-rolling it is precisely the
  hand-patch-where-deterministic-code-fits pattern the project forbids.
- Rate limiting: `governor` (0.10.4, `Cargo.lock:1175`, via librqbit) keyed by peer IP —
  5 failed auths/minute, then `429`. FR-87.

`src/remote/mod.rs`: an axum `Router` with a `from_fn` auth middleware, `TcpListener` bound per
config, a concurrency cap via `tower::limit`, and a body-size cap (`DefaultBodyLimit`, 8 MiB —
`.torrent` files are kilobytes; anything larger is an attack or a mistake).

`Config` gains, all `#[serde(default)]`:
```rust
pub remote_api_enabled: bool,      // default false — FR-84
pub remote_api_bind: String,       // default "127.0.0.1:8766" — FR-85
```

**Step 4 — the routes (~200 lines).** Lean, harbour-shaped, `/v1`:

| Route | Auth | Notes |
| --- | --- | --- |
| `GET /v1/health` | none | `{"ok":true,"version":"…"}`. No state, so no token — this is what a supervisor polls. |
| `GET /v1/status` | yes | engine up, queue counts, aggregate speeds, alt-rate state |
| `GET /v1/downloads` | yes | `Vec<DownloadDto>` from `Queue::views()` |
| `POST /v1/downloads` | yes | `{"magnet":"magnet:?xt=…"}` **or** `Content-Type: application/x-bittorrent` raw bytes |
| `POST /v1/downloads/{id}/pause` | yes | |
| `POST /v1/downloads/{id}/resume` | yes | |
| `DELETE /v1/downloads/{id}?delete_files=bool` | yes | |
| `GET /v1/search?q=…` | yes | proxies the user's own indexer — a call harbour already makes |

`{id}` is validated with the **existing** `core::magnet::is_info_hash` before it reaches any
path-forming code — that is `NFR-11` (no path traversal from untrusted input) enforced at the new
boundary, reusing the existing check rather than writing a second one.

**Explicitly not built:** `/api/v2/*` anything, a web UI, a file browser, a "download from URL"
endpoint, per-file selection, tracker editing, RSS. FR-89.

**Step 5 — event stream (optional, ~120 lines).** `GET /v1/events` as SSE, fed by a
`tokio::sync::broadcast` of `EngineEvent` projections. Only build it if a concrete automation
need appears; polling `/v1/downloads` at 1 Hz is adequate and has no lifecycle to get wrong.

**Step 6 — headless mode (optional, separate PR, ~100 lines).** `harbour --daemon`: run the
engine, queue, persistence and API without entering the terminal (skip `TerminalGuard::enter`
and the draw loop). This is where "remote control" pays off, and it is a clean split because
`run()` already separates setup from the loop. It needs its own SPEC line (FR-90) and its own
issue if it grows.

## Files to create / modify

- `SPEC.md` — §4.9 (FR-84…FR-89) + the NFR-10 amendment. **First commit after step 0.**
- `src/engine/rqbit.rs` — **step 0**: `HttpApiOptions { read_only: true, .. }` + why-comment.
- `Cargo.toml` — `axum`, `subtle`, `rand`, `governor`, `tower` / `tower-http` as direct deps.
- `src/remote/mod.rs` — **new**: router, listener, middleware, shutdown.
- `src/remote/auth.rs` — **new**: token generate/load/verify, rate limiter.
- `src/remote/command.rs` — **new**: `RemoteCommand`.
- `src/remote/wire.rs` — **new**: the DTOs (never shared types on the wire).
- `src/remote/routes.rs` — **new**: handlers.
- `src/main.rs` — `mod remote;`.
- `src/app/mod.rs` — the fourth `select!` arm + drain; spawn the listener when enabled; pass the
  sender in.
- `src/persist.rs` — `remote_api_enabled`, `remote_api_bind`; the token file next to the existing
  markers.
- `src/core/paths.rs` — `api_token_path()`.
- `src/cli.rs` — `--api` / `--no-api` overrides; `--print-api-token`.
- `tests/remote_api.rs` — **new**: the integration suite.
- `SECURITY.md` — a paragraph on the token, the bind default, and the read-only stream API.
- `README.md` — a short "automation" section with two `curl` examples.

## Key APIs / libraries

Every crate below is **already in `Cargo.lock`** (verified 2026-08-16), so promoting them to
direct dependencies adds **zero new crates** to the tree — the lean-dependency justification, in
one line each:

| Crate | Lock line | Already pulled by | Why |
| --- | --- | --- | --- |
| `axum 0.8.9` | 173 | `librqbit` (`http-api` feature, which harbour enables in `Cargo.toml:12`) | the HTTP router harbour's own engine already runs |
| `tower` / `tower-http 0.6.11` | 3972 / 3988 | `librqbit`, `reqwest` | concurrency limit, body limit, tracing |
| `subtle` | 3588 | `rustls` (verified by reverse-dep scan of `Cargo.lock`, 2026-08-16) | constant-time token compare (FR-86) |
| `rand 0.9.5` | 2780 | `librqbit`, `librqbit-core`, `librqbit-dht`, `librqbit-tracker-comms`, `governor` | 32 bytes of token entropy |
| `governor 0.10.4` | 1175 | `librqbit` | per-IP auth rate limiting (FR-87) |

License check: all are MIT/Apache-2.0/BSD, already inside `deny.toml`'s allow list — no
`deny.toml` change is needed. Confirm with `cargo deny check` before merge rather than assuming.

**librqbit 8.1.1, read at `~/.cargo/registry/src/index.crates.io-*/librqbit-8.1.1/` on
2026-08-16:**
- `src/http_api/mod.rs:35-37` — `HttpApiOptions { read_only, basic_auth }`
- `src/http_api/mod.rs:75,83` — `HttpApi::new(api, opts)`; "*If read_only is passed, no
  state-modifying methods will be exposed*"
- `src/http_api/handlers/mod.rs:66-120` — the route table and the `if !state.opts.read_only` gate;
  `/torrents/{id}/stream/{file_id}` sits **above** the gate, so read-only keeps streaming
- `src/http_api/mod.rs:104-129` — the CORS allow-list and `CORS_ALLOW_REGEXP` env override

**Deliberately not used:** librqbit's `HttpApi` for harbour's own API. It is rqbit-shaped, it
serves a web UI, and adopting it would make FR-89 unenforceable — the qBittorrent-parity drift
starts by inheriting someone else's route table.

## Risks / edge cases

- **SSRF — solved by not having the feature (FR-88).** Every SSRF story in a torrent client
  starts with "add torrent from URL": the server fetches an attacker-chosen URL and becomes a
  probe for `169.254.169.254`, `127.0.0.1:*`, or an internal service. harbour accepts magnets and
  raw bytes only, so there is no fetch to redirect. **Rejected: adding URL support with an
  IP-range denylist.** Denylists lose — DNS rebinding, redirect chains, IPv6-mapped IPv4,
  decimal-encoded IPs. If URL add is ever wanted it needs an explicit allowlist and its own
  issue; a denylist is the band-aid this project forbids.
- **The existing unauthenticated librqbit API is a live issue on `main` today.** Step 0 fixes it
  and must not be bundled into the big API PR where it would be invisible in review.
- **Rejected: `Arc<Mutex<Queue>>`.** It breaks "the loop owns mutations" (`ui/mod.rs:5`), it
  deadlocks against `select!`, and it makes API and TUI behaviour diverge silently. The channel
  costs ~40 lines more and keeps one writer.
- **An unbounded command channel is a memory DoS.** Use `mpsc::channel(64)` with backpressure —
  the input channel's `unbounded` is safe because a human types at human speed; an HTTP client
  does not. Return `503` when full.
- **Token leakage via process listing / shell history.** `--print-api-token` reads the file; the
  token is never a CLI argument and never an environment variable in harbour's own docs.
- **Non-loopback bind.** FR-85 permits it (people run seedboxes) but harbour must warn loudly at
  startup and refuse without a token. **Rejected: shipping TLS.** A cert story in a terminal app
  is a large surface; the documented answer is an SSH tunnel or a reverse proxy, stated in
  `SECURITY.md`.
- **CORS on the *stream* API stays permissive** even after step 0 (it is librqbit's layer, not
  harbour's). Read-only means a malicious page can enumerate and read the user's torrents from
  loopback. Note it in `SECURITY.md`; the real fix is upstream or a harbour-owned stream proxy,
  and it is out of scope here. **Say this out loud rather than implying step 0 closes everything.**
- **`NFR-15`** — an API listener that fails to bind (port in use) must warn and leave the TUI
  fully working. Never abort `run()`.
- **Clean shutdown.** The listener must stop when the app loop exits, before
  `store.flush_and_disarm`, or a request can mutate the queue after the final flush and lose
  state. Use `axum::serve(...).with_graceful_shutdown(...)` driven by the quit path.
- **Wire/type coupling.** DTOs, not shared types. Stated twice on purpose.
- **Rejected: qBittorrent WebUI parity.** FR-89. Named here so it is rejected once, in writing —
  it is the single most likely piece of scope creep in this issue, and every route added "for
  Sonarr compatibility" is a route harbour maintains forever.

## Test strategy

- **Unit, `src/remote/auth.rs`** — a generated token is 64 hex chars; `verify` accepts the exact
  token and rejects a one-byte-different token, a prefix, an empty string, and a longer string;
  the file is created `0600` on unix (`#[cfg(unix)]`); the rate limiter permits 5 failures then
  denies within the window and recovers after it.
- **Unit, `src/remote/routes.rs`** — an id failing `core::magnet::is_info_hash` (`../../etc`,
  `..%2f`, a 41-char hex string) is rejected `400` **before** any queue lookup; FR-88 rejects a
  body whose magnet field is `http://…` or `https://…` with a message naming the reason.
- **Integration, `tests/remote_api.rs`** (no network — a `FakeEngine` app on an ephemeral port,
  so this runs in normal CI, *not* behind `HARBOUR_TEST_NET=1`):
  - `GET /v1/health` without a token → `200`
  - `GET /v1/status` without a token → `401`; with a wrong token → `401`; with the right one →
    `200`
  - `POST /v1/downloads` with a magnet → the item appears in `GET /v1/downloads` **and** in
    `Queue::views()` — proof the command reached the loop's single writer
  - pause → resume → delete round-trips, and `QueueStatus` matches what the TUI path produces
    for the same operations
  - a burst of 6 bad tokens → the 6th is `429`
  - a 20 MiB body → `413`
- **Integration, step 0** — `HARBOUR_TEST_NET=1 cargo test --test engine_net`: a stream URL still
  resolves and serves bytes with `read_only: true`, and `POST /torrents` against the stream
  server returns `404`/`405`. **That second assertion is the regression test for Finding 1** and
  is the one that must never be deleted.
- **Unit, `src/persist.rs`** — a pre-#67 `config.toml` loads with `remote_api_enabled == false`.
  A default install never opens a port (FR-84), asserted directly.
- **No UI tests.** Nothing in `src/ui/*` changes in steps 0–5, which is itself the evidence that
  this is a separate track.

## Verification

1. **Step 0, before anything else.** `cargo run`, press `w` to start a watch, note the loopback
   port, then:
   `curl -X POST http://127.0.0.1:<port>/torrents -d 'magnet:?xt=urn:btih:…'`
   On `main` today this **adds a torrent**. After step 0 it returns `404`/`405`, while
   `curl -I http://127.0.0.1:<port>/torrents/<id>/stream/0` still returns `200` and the player
   still plays. That pair — one thing stops working, the other keeps working — is the whole
   verification of the security fix.
2. Default install: `cargo run`, then `ss -ltnp | grep 8766` (or `netstat -ano`) → **nothing**.
   FR-84 proven by absence.
3. Enable it (`remote_api_enabled = true`), relaunch, then:
   ```
   curl -s localhost:8766/v1/health                                   # 200, no token
   curl -s -o /dev/null -w '%{http_code}' localhost:8766/v1/status     # 401
   TOKEN=$(cat ~/.harbour/api-token)
   curl -s -H "Authorization: Bearer $TOKEN" localhost:8766/v1/status  # 200
   curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        -d '{"magnet":"magnet:?xt=urn:btih:…"}' localhost:8766/v1/downloads
   ```
   The last one is the money shot: **the new download appears in the running TUI's downloads
   list, live, without a restart.** That is remote control demonstrated, and it also proves the
   command channel reached the loop's single writer.
4. `curl -H "Authorization: Bearer $TOKEN" -d '{"magnet":"http://evil.test/x.torrent"}'
   localhost:8766/v1/downloads` → `400` naming the reason. FR-88 demonstrated.
5. Six requests with a bad token → the sixth is `429`. FR-87 demonstrated.
6. `grep -rn "api/v2" src/` → nothing. FR-89 held.
7. `cargo deny check` clean — no new licenses, no new advisories, and (per the table above) no
   new crates in the tree.
