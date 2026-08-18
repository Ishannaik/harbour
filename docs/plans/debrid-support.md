# Debrid support (Real-Debrid + others)
Ref: #48

## Goal
Resolve a selected result through a debrid service into a direct HTTP link and hand that link
to the external player — with cache status shown first where the provider still supports it,
behind one pluggable trait so Real-Debrid is not special-cased.

## The finding that shapes this whole plan

**Real-Debrid's `/torrents/instantAvailability` is dead.** It was disabled in **November 2024**
after a formal notice from the FNEF (a French anti-piracy body); the endpoint now answers
`disabled_endpoint` and every Stremio addon that depended on it broke at once. Checked
2026-08-16:

- [g0ldyy/comet#243 — "Real-Debrid responds with `disabled_endpoint` for API Call to
  InstantAvailability"](https://github.com/g0ldyy/comet/issues/243)
- [ElfHosted — "Stremio after RealDebrid" (2024-11-22)](https://store.elfhosted.com/blog/2024/11/22/stremio-after-realdebrid/)
- [rogerfar/rdt-client#545 — RD response changes for uncached torrents](https://github.com/rogerfar/rdt-client/issues/545)

Meanwhile the other three providers **do** still expose a cache check (checked 2026-08-16):

| Provider | Cache check | Status |
| --- | --- | --- |
| Real-Debrid | `/torrents/instantAvailability` | **Removed** — returns `disabled_endpoint` |
| TorBox | `GET /v1/api/torrents/checkcached?hash=…` | Works. ~100 hashes/request, 1-hour server-side cache, `Authorization: Bearer` ([TorBox SDK docs](https://github.com/TorBox-App/torbox-sdk-py/blob/main/documentation/services/TorrentsService.md)) |
| Premiumize | `POST /api/cache/check` | Works, explicitly **best-effort** — a hit is not a guarantee ([premiumize.me/api](https://www.premiumize.me/api)) |
| AllDebrid | `magnet/instant` | **Uncertain.** Wrappers still ship `GetMagnetInstant`, but AllDebrid's own site carried a "resolving the current situation about our magnet tool" notice. Not settled in two searches — see Risks. |

**Therefore `check_cached` must not be a required trait method.** The issue says
"cache-status-first (check instant availability before resolving)", but for the single most
popular provider that is no longer possible. Baking a mandatory instant-availability call into
the trait would design the whole feature around a removed endpoint. The trait returns
`CacheStatus::Unknown` by default and each provider opts in.

**The RD flow that replaces it** (still current, and what every surviving addon now does):
`POST /torrents/addMagnet` → `POST /torrents/selectFiles/{id}` → poll `GET /torrents/info/{id}`
until `status == "downloaded"` → `POST /unrestrict/link`. Rate limits are strict — **250
requests/minute, 30 concurrent** — so a naive per-row cache probe over a 200-row result list
would get harbour rate-limited even if the endpoint existed
([sooti/stremio-addon-debrid-search — Real-Debrid integration](https://deepwiki.com/sooti/stremio-addon-debrid-search/4.1-real-debrid-integration),
checked 2026-08-16).

## SPEC / FR reference

Exists today: **FR-57…FR-61** (watch mode) describe streaming from harbour's *own* loopback
endpoint. **Nothing in SPEC mentions debrid, third-party accounts, or API keys** — this is
entirely new surface, and one that sends a user's infohashes to a third party, so it must be
specified before it is built.

**Missing from SPEC — add first, then implement.** Proposed **FR-86 … FR-91**, new §4.9:

> **FR numbers here are provisional.** 13+ plans were drafted in parallel on 2026-08-16 and
> their ranges collide — five plans claim FR-86, and FR-112 is claimed twice. Final numbers are
> assigned when each SPEC PR merges; renumber then. **The settings-row indices claimed below
> are provisional for the same reason**: the parallel batch (`speed-limits`, `share-limits`,
> `protocol-toggles`, `encryption-mode`, and especially `categorized-settings`, which may
> restructure the rows entirely) also adds rows, so the row-collision note below is
> understated — treat it as "coordinate with the whole batch", not just #50/#51.

- **FR-86 (opt-in).** Debrid is off unless the user has saved an API key for a provider.
  With no key configured harbour never contacts a debrid host, and the debrid keybind is inert
  with an explanatory banner — not a silent no-op.
- **FR-87 (provider abstraction).** Every provider implements one `DebridProvider` trait.
  Adding a provider is one module plus one registry row; no provider is special-cased in the
  app loop or the UI.
- **FR-88 (cache status is optional and honest).** Cache status is one of `cached`,
  `not cached`, or `unknown`. A provider that cannot answer (Real-Debrid since Nov 2024)
  reports `unknown`, and the UI renders `unknown` — never an optimistic `cached`.
- **FR-89 (resolve → play).** Resolving a result yields a direct HTTPS URL that is handed to
  the external player through the existing watch path. Harbour does not download debrid links
  itself and they never enter the torrent queue or the ledger.
- **FR-90 (loud failure).** Every debrid failure — bad key, expired subscription, rate limit,
  uncached torrent that never converts — is an error banner naming the provider and the
  reason. No fallback to the swarm without telling the user.
- **FR-91 (key storage).** API keys live in `config.toml` at `~/.harbour/`, in plaintext, and
  the settings row says so. Keys are never logged, never in an error message, and never sent
  anywhere but their own provider's host.

## Workstream

**Engine & Foundation (Sarthak)** owns steps 1–4: `DebridProvider` is a new load-bearing
contract of exactly the kind AGENTS.md §4 reserves for the engine track, and it sits beside
`Source` and `Engine` in shape and in dyn-compatibility constraints.

**Terminal UI (Ishan)** owns steps 5–6 (settings rows, cache badge, keybind).

Shared-type dependencies: **the trait is new, not a change to a frozen type.** Do not add a
variant to `QueueStatus` and do not add a `EngineEvent` variant — a debrid stream never
becomes a queue item (FR-89), so neither frozen enum needs to grow. This is the single most
important constraint in the plan; see Risks.

## Approach

**Step 1 — SPEC (docs only).** FR-86…FR-91 as §4.9. No code.

**Step 2 — the trait and the types, no network (engine).** `src/debrid/mod.rs`:

```rust
pub enum CacheStatus { Cached, NotCached, Unknown }

pub struct DebridStream { pub url: String, pub filename: String, pub size_bytes: u64 }

pub trait DebridProvider: Send + Sync + 'static {
    fn def(&self) -> &'static DebridDef;
    /// Default `Unknown` — Real-Debrid removed its endpoint (Nov 2024), so a
    /// provider that cannot answer is the normal case, not an error.
    fn check_cached<'a>(&'a self, hashes: &'a [InfoHash], ctx: &'a DebridCtx)
        -> CacheFuture<'a> { /* all Unknown */ }
    fn resolve<'a>(&'a self, magnet: &'a str, ctx: &'a DebridCtx) -> ResolveFuture<'a>;
}
```

Boxed futures (`Pin<Box<dyn Future + Send>>`), exactly as `Source` and `Engine` do and for the
identical reason documented in `core/types.rs`'s module docs: `async fn` in a trait is
dyn-incompatible and the registry needs `Vec<Arc<dyn DebridProvider>>`. Ships with a
`FakeDebrid` (mirroring `src/engine/fake.rs`) so steps 5–6 build with no account.

**Step 3 — Real-Debrid (engine).** `src/debrid/realdebrid.rs`. `resolve` implements the four-
call flow from *The finding*. `check_cached` is **not** overridden — the default `Unknown` is
the truthful answer. Poll `torrents/info` with a bounded deadline (~20 s) and a fixed interval;
a torrent that never reaches `downloaded` is an uncached torrent and returns
`DebridError::NotCached`, which the UI reports verbatim.

**Step 4 — TorBox + Premiumize (engine, one PR each).** These override `check_cached`:
TorBox batches ≤100 hashes per request; Premiumize's answer is documented best-effort, so a
`Cached` that later fails to resolve is a normal path, not a bug — FR-90 covers it.
AllDebrid is deliberately **last and separate**, pending the Risks item.

**Step 5 — settings (UI).** Two rows: provider (cycles `off / real-debrid / torbox /
premiumize`) and API key (text, masked). See the row-collision note below.

**Step 6 — the keybind and the badge (UI).** On the search screen, a debrid key on the selected
row resolves and launches the player through the *existing* watch path. A cache badge renders
per row from whatever `check_cached` reported, with `unknown` rendered as a neutral dot —
the same "unknown is not failure" rule `SourceStatus` already encodes.

### Settings-row collision (cross-PR, read before merging)

`ui/settings.rs` hardcodes row indices in four parallel `match` blocks — `row_kind`,
`text_field`, `row_label`, `source_at` — against `const APP_ROWS: usize = 17`, and
`app/settings.rs::settings_toggle_row` matches the same integers again. **Issues #48, #50 and
#51 all add rows.** Each PR must append its rows immediately before the per-source block and
bump `APP_ROWS` by the number it adds; whichever merges second rebases rather than merging the
index changes textually. This plan claims rows **17–18** if it lands first. A follow-up worth
filing: replace the integer matches with a single `const ROWS: &[Row]` table so this class of
conflict stops recurring.

## Files to create / modify

Create:

- `src/debrid/mod.rs` — trait, `CacheStatus`, `DebridStream`, `DebridDef`, `DebridCtx`,
  `DebridError`, and the registry (`providers_from_config`).
- `src/debrid/realdebrid.rs`, `src/debrid/torbox.rs`, `src/debrid/premiumize.rs`
- `src/debrid/fake.rs` — offline provider for tests and UI work.

Modify:

- `src/main.rs` — `mod debrid;`
- `src/persist.rs` — `Config.debrid_provider: Option<String>`, `Config.debrid_api_key:
  Option<String>`, both `#[serde(default)]` so existing `config.toml` files keep loading (the
  struct already carries `#[serde(default)]` at the container level).
- `src/ui/settings.rs` — two rows; `APP_ROWS` 17 → 19; a new `TextField::DebridApiKey` and a
  `RowKind::Cycle`-style provider row (reuse the theme row's cycle shape).
- `src/app/settings.rs` — commit arms for both rows; the key row must **not** echo the key into
  a warning banner on a failed save (FR-91).
- `src/app/actions.rs` — `debrid_watch_selected`, next to `download_selected`.
- `src/input.rs` — the keybind; `src/ui/help.rs` — the help line.
- `src/ui/search.rs` — the cache badge column.
- `Cargo.toml` — see below.

## Key APIs / libraries

**New crates: none.** `reqwest`, `serde`, `serde_json` and `tokio` are already in the tree and
`src/sources.rs` already does exactly this shape of JSON-over-HTTP work — reuse its client
construction and timeout conventions rather than inventing a second style.

One **feature** change, not a new dependency: `reqwest` is currently
`default-features = false, features = ["default-tls", "gzip", "brotli", "stream"]` — **no
`json`**. Either add `"json"` (it pulls only `serde_json`, already a direct dependency) or
parse with `serde_json::from_slice(&resp.bytes().await?)`. **Prefer adding `"json"`**: it adds
no new crate to the lockfile and removes a hand-rolled deserialize step from four modules.
Justify it in the PR description per AGENTS.md §8.

Endpoints, all checked 2026-08-16 (see *The finding* for links):

- **Real-Debrid** `https://api.real-debrid.com/rest/1.0` — `POST /torrents/addMagnet`,
  `POST /torrents/selectFiles/{id}`, `GET /torrents/info/{id}`, `POST /unrestrict/link`.
  `Authorization: Bearer <key>`. 250 req/min, 30 concurrent.
- **TorBox** `https://api.torbox.app/v1/api` — `GET /torrents/checkcached`,
  `POST /torrents/createtorrent`, `GET /torrents/requestdl`. Bearer.
- **Premiumize** `https://www.premiumize.me/api` — `POST /cache/check`,
  `POST /transfer/directdl`. Key as `apikey` param or Bearer.

**Playback reuses what exists.** `WatchSession::launch_remote(&player, &url)`
(`src/watch.rs`) already launches an external player against an arbitrary URL, and
`app/watch.rs::enter_watch` already records a `NowPlaying` from one. A debrid resolve is
therefore a *new URL source for an existing pipe*, roughly 30 lines — not a new playback stack.

## Risks / edge cases

- **Rejected approach: a parallel HTTP download manager.** "Stream over HTTP instead of local
  download" reads like "add an HTTP downloader alongside librqbit". Reject it. It would need
  progress, pause/resume and a status for a non-torrent item, which means new `QueueStatus`
  variants and new `EngineEvent` variants — both **frozen** shared types owned by Sarthak,
  changed for a feature that does not need it. FR-89 scopes debrid to *resolve and hand to the
  player*, which the watch path already does. If "save this debrid link to disk" is genuinely
  wanted later, it is its own issue with its own SPEC change.
- **AllDebrid's cache endpoint is unresolved.** Two searches did not settle whether
  `magnet/instant` still answers (see the table). Do not implement AllDebrid on the assumption
  it works: implement `resolve` first with `check_cached` left at the `Unknown` default, and
  only override it after a live 200 against a real key. The trait's optional design means this
  costs nothing structurally — which is the point.
- **Rate limits are the likeliest first bug.** RD allows 250 req/min. Do **not** call
  `check_cached` per visible row on every keystroke or every search. Batch (TorBox's ≤100 cap
  is the natural batch size), cache per infohash for the session, and only probe rows the user
  can actually see. A 200-row result set naively probed one-per-row exceeds the minute budget
  on the second search.
- **Plaintext API keys.** `config.toml` is world-readable in the user's home directory. This is
  what qBittorrent and every Stremio addon do, and OS keychain integration would add a
  dependency per platform. Accept it, state it in FR-91 *and* in the settings row's label so
  the user is not surprised. `~/.harbour/` is already gitignored (AGENTS.md §9).
- **Debrid links expire** — typically hours, and they are IP-bound on several providers. Never
  persist a resolved URL to the ledger or reuse one across sessions; resolve fresh each time.
- **A `cached` that will not resolve.** Premiumize documents its cache as best-effort. Handle
  `Cached` → `resolve` failure as an ordinary FR-90 banner.
- **Privacy.** Debrid resolution tells a third party which infohash the user wants, tied to a
  paying account. FR-86's opt-in default is the mitigation; say it plainly in the settings row.
- **Uncached torrents take minutes, not seconds.** RD converts them by actually downloading
  them. The bounded poll (~20 s) then `NotCached` is honest; a spinner that hangs for four
  minutes is not.

## Test strategy

- **Unit, `src/debrid/*.rs`** — response parsing against **checked-in JSON fixtures** captured
  from each provider's docs, mirroring the fixture-test convention AGENTS.md §7 already
  mandates for scrapers. Cover: RD's `disabled_endpoint` body maps to `CacheStatus::Unknown`
  and **never** to an error banner; an uncached RD torrent that never reaches `downloaded` maps
  to `NotCached`; TorBox's `checkcached` object *and* list response shapes both parse; a 401
  maps to a "check your API key" error whose `Display` **does not contain the key**.
- **Unit, `src/debrid/mod.rs`** — the default `check_cached` returns all-`Unknown` for a
  provider that does not override it. This is the FR-88 guarantee; if someone later makes the
  method required, this test objects.
- **Buffer snapshot, `src/ui/tests.rs`** — the cache badge renders `cached` / `not cached` /
  neutral-dot-for-unknown; the settings key row renders masked (`••••`), never the key.
- **Integration, gated `HARBOUR_TEST_NET=1`, `tests/debrid_net.rs`** — additionally gated on a
  key env var (`HARBOUR_RD_KEY`); skips cleanly, exactly like `tests/engine_net.rs`. Never runs
  in CI (FR-64 forbids network tests there).
- **No queue tests** — `src/queue.rs` is untouched by design.

## Verification

1. `SPEC.md` §4.9 contains FR-86…FR-91.
2. With **no** key configured: `cargo run`, press the debrid key on a result → a banner says
   debrid is not configured and points at settings. Nothing is sent (confirm with a packet
   capture or by pointing the base URL at a local listener that logs).
3. With a real Real-Debrid key: settings → provider `real-debrid`, paste key, search a
   well-seeded title, press the debrid key → **mpv/VLC opens and plays from
   `https://*.real-debrid.com/d/…` within a few seconds**, and `~/.harbour/downloads.json` is
   **unchanged** — the ledger never learns about it (FR-89). That is the user-visible proof.
4. The now-playing view shows the debrid URL's host, so it is obvious the bytes are not coming
   from the swarm.
5. On a deliberately uncached obscure torrent: a banner naming the provider and "not cached"
   inside the poll deadline — not an indefinite spinner (FR-90).
6. `grep -rn "debrid_api_key" src/` shows the key is read in exactly two places (config load,
   provider construction) and appears in no `warn`/`log`/`format!` that reaches the UI.
