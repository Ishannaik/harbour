# Plugin multi-engine search
Ref: #58

## Goal
Make harbour's multi-engine search honest: document the aggregation that already exists, move
the Jackett-style plugin surface to where it can actually live (the indexer), and build the one
piece that is genuinely missing on the client — **search proxy enforcement**.

## The findings that shape this plan

Read on **2026-08-16** from harbour's own source and the vendored dependency tree.

**1. The client ships zero scrapers, by design.** `src/sources.rs:1-21` and
`docs/architecture.md`: all site adapters live in the separate `harbour-indexer` service and the
client's only `Source` is `HttpSource`, an HTTP proxy. `docs/indexer-guide.md` already documents
the wire contract (`/search`, `/magnet`, `/health`) and says in as many words that harbour is
"a protocol-neutral client (the Stremio / Jackett model)". **harbour is already the Jackett
consumer.** A second, client-side plugin runtime would be a parallel scraping stack in the one
binary that is deliberately scrape-free.

**2. Multi-engine aggregation is already built and tested.** `src/search.rs:43-63` —
`merge()` dedupes by infohash keeping the higher seeder count, then sorts seeders → newest →
name for a total order. `src/app/mod.rs:206-219` (`remerge`) folds per-source batches and drops
disabled sources before merging. `src/sources.rs:294-404` (`consume_stream`) consumes the
indexer's NDJSON `/search/stream` so sources land one at a time. Four unit tests in
`src/search.rs` already pin the merge semantics. **There is nothing to build here; there is
something to write down.**

**3. Result filtering is already built.** `SearchState::sort_results` / `toggle_sort`
(`src/ui/mod.rs:93-133`) plus the `0`–`4` keybinds (`src/input.rs:343-347`) and the sidebar
source toggles (`Config.disabled_sources` → `ctx.disabled` → the `exclude` param,
`src/sources.rs:487-500`).

**4. Dynamic plugin ids are structurally impossible against the frozen shared types.**
`SourceId` (`src/core/types.rs:44-76`) is `#[derive(Copy)]` with `as_str(self) -> &'static str`
and a fixed `ALL: [SourceId; 10]`. A `Plugin(String)` variant breaks `Copy`, breaks `'static`,
and breaks the `SourceId::ALL` iteration that the sidebar, the settings rows, and
`settle_unanswered` all depend on. `SourceId::parse` already returns `None` for an unknown id
and `record_sources` silently skips it (`src/sources.rs:217-219`) — so an indexer that today
reports a plugin id is *dropped on the floor*. Shared types are Sarthak's and frozen
(`AGENTS.md` rule 4), so this is a deliberate deferral, not an oversight.

**5. `socks_proxy_url` reaches only the swarm.** `src/persist.rs:82-83` documents it as "SOCKS5
proxy URL for the swarm"; `EngineLaunchOptions::from_config` (`src/engine/rqbit.rs:588-598`)
hands it to `SessionOptions.socks_proxy_url`. **Neither reqwest client in `src/sources.rs`
honours it** — not `HttpSource::new`'s client (line ~148) and not `consume_stream`'s fresh
pool-free client (line ~308). Search traffic therefore always goes direct. When the indexer is
remote (which the config explicitly allows) that is a real leak, and it is the concrete
client-side deliverable in this issue.

**6. `reqwest` needs a cargo feature for SOCKS.** Confirmed from the vendored manifest
`~/.cargo/registry/src/index.crates.io-*/reqwest-0.13.4/Cargo.toml:178` → `socks = []`. harbour
currently declares `reqwest = { version = "0.13.4", default-features = false, features =
["default-tls", "gzip", "brotli", "stream"] }` — no `socks`. Enabling it pulls
`tokio-socks`/`hyper-socks`-style support into the tree; that is the one new build surface this
plan adds and it is justified below.

## SPEC / FR reference

**Exists today.** §4.2 FR-11…FR-22 covers search and browse; FR-14 is the malformed-row rule,
FR-15/FR-18 the per-source health, FR-20 the cancel-on-new-query rule. §4.2 does **not** say
anywhere that results from several engines are merged into one deduplicated list, which is the
product's single most user-visible search behaviour.

> **FR numbers here are proposed, not reserved.** Several plans in `docs/plans/` were drafted
> in parallel against the same free block (FR-86+), so numbers collide across files. Allocate
> final numbers when the SPEC edit lands (first merged wins, renumber the rest). The
> requirement *text* is the deliverable; the number is bookkeeping.

**Missing from SPEC — add first, then implement.** Proposed **FR-86 … FR-90**, in §4.2:

- **FR-86 (aggregation, normative).** Results from every engine the indexer queries are merged
  into one list, deduplicated by infohash keeping the copy reporting the most seeders, and
  ordered seeders-descending → newest → name so the order is total and stable between renders.
  A source with `reports_health == false` reports `seeders: 0` meaning *unknown*; dedup must
  never let that displace a real count. (This is `search::merge`, written down.)
- **FR-87 (engine set is the indexer's, not the client's).** harbour queries exactly one
  `Source`: the configured indexer. Which engines/plugins that indexer runs is the indexer's
  concern. The client contributes only the user's disabled-site set, sent as `exclude`.
- **FR-88 (unknown engine ids are reported, never silently dropped).** A `sources[]` entry or a
  result row naming an id the client does not know is counted and surfaced as
  "N results from engines this build does not recognise" rather than vanishing. Today it is
  dropped silently — the exact class of silent fallback the project rules forbid.
- **FR-89 (search proxy enforcement).** When a search proxy is configured, **every** outbound
  client search request — `/search`, `/search/stream`, `/magnet`, and the indexer health probe
  — goes through it. If the proxy is configured but the client cannot build a proxied HTTP
  client, the search fails loudly with a banner; it must never fall back to a direct
  connection.
- **FR-90 (proxy scope is explicit).** The settings expose whether the SOCKS5 proxy applies to
  the swarm only, to search only, or to both. A proxy silently covering only half the traffic
  is a privacy bug, so the scope is a stated setting rather than an implied one.

## Workstream

- **Engine & Foundation (Sarthak)** — FR-89/FR-90 proxy plumbing in `src/sources.rs`,
  `src/persist.rs`, `src/engine/rqbit.rs`; the FR-88 unknown-id counter in `src/sources.rs`.
  Owns any future `SourceId` redesign (step 5).
- **Terminal UI (Ishan)** — the two settings rows and the unknown-engine banner copy.
- **Sources / indexer (Dhruv)** — steps 1 and 4 land in the **`harbour-indexer` repo**, not
  here: the plugin loader and the `/engines` capability endpoint.

**Shared-type dependency:** this plan is written to *not* touch `SourceId`, `Source`,
`TorrentResult`, or `EngineEvent`. Step 5 is the one that would, and it is deliberately blocked.

## Approach

**Step 1 — SPEC (docs only).** FR-86…FR-90 into §4.2. Nothing else in this plan is reviewable
until "what does multi-engine search promise" is written down. ~40 lines of SPEC.

**Step 2 — search proxy enforcement (engine track).** The real feature.
- `Config` gains `search_proxy_url: Option<String>` and `proxy_scope: ProxyScope`
  (`Swarm` / `Search` / `Both`, `#[serde(rename_all = "lowercase")]`, default `Swarm` so
  existing configs are unchanged).
- `HttpSource::new` takes the resolved proxy and applies it to **both** client builders. A
  single `fn build_client(proxy: Option<&str>) -> Result<reqwest::Client, SourceError>` used by
  `new()` *and* `consume_stream()` is the whole point — two builders is how the stream path
  leaks around a proxy.
- A configured-but-unbuildable proxy returns `Err` and the app banners it. **Delete the
  existing `unwrap_or_else(|err| … reqwest::Client::new())` fallback in `HttpSource::new`
  (line ~151)** — with a proxy configured, that fallback is a silent direct connection.
- ~120 lines. Independently testable: a stub SOCKS listener that accepts a connection proves
  the request went through it.

**Step 3 — unknown-engine visibility (engine track, small).** `record_sources` and
`parse_results` count rows whose `source` id does not parse instead of dropping them silently;
`HttpSource` exposes `unrecognised_count()` and the app renders "N results from engines this
build does not recognise" in the search footer. ~60 lines. This is the *forward-compatibility*
half of plugin support: an indexer that grows plugins stops being invisible, without any
shared-type change.

**Step 4 — the plugin loader, in `harbour-indexer` (indexer track, out of this repo).** The
Jackett-shaped design, recorded here so the client and indexer agree:
- A plugin is a declarative manifest (Torznab endpoint URL + api key + category map), or a
  qBittorrent-style script adapter. The indexer's `/search` fans out across plugins and
  returns the merged rows in the existing wire shape.
- The indexer adds `GET /engines` → `[{"id","label","kind":"builtin"|"plugin","enabled"}]`
  so the client can *list* engines it does not have enum variants for. This is additive: an
  indexer without it is tolerated exactly like the missing `sources[]` array is today
  (`src/sources.rs:201-203`).
- Torznab is the interop target: `?t=caps` for capabilities and `?t=search&q=&cat=` for
  queries, returning an RSS/XML result set with `torznab:attr` seeders/peers
  ([Jackett Torznab reference](https://deepwiki.com/Jackett/Jackett/3-torznab-api-reference),
  [qBittorrent's Torznab notes](https://github.com/qbittorrent/search-plugins/wiki/New-Torznab-search-engine),
  both checked 2026-08-16). The indexer speaks Torznab *outward* to plugins and harbour's own
  JSON *inward* to the client — the client never parses XML.

**Step 5 — dynamic engine ids (BLOCKED, shared-types).** Only after step 4 ships and a real
plugin exists is there a reason to widen `SourceId`. The shape, for whoever picks it up:
replace `SourceId` with a `Copy` interned handle (`struct SourceId(u16)` + a registry built at
boot from `/engines`) so `Copy`/`'static` survive, or add a separate `EngineId(Arc<str>)` used
only for display. Both are Sarthak-owned, both are >400 lines, and neither should be attempted
inside this issue. **Do not add a `SourceId::Plugin(String)` variant** — it breaks `Copy` and
cascades into every match in `src/ui/settings.rs`, `src/ui/search.rs`, and `src/sources.rs`.

## Files to create / modify

- `SPEC.md` — FR-86…FR-90 in §4.2.
- `Cargo.toml` — add `"socks"` to the `reqwest` feature list.
- `src/persist.rs` — `Config.search_proxy_url`, `Config.proxy_scope`, the `ProxyScope` enum,
  both `#[serde(default)]` so older configs load unchanged; update the `socks_proxy_url` doc
  comment which currently says "for the swarm" and would become a lie.
- `src/sources.rs` — `build_client()`; both call sites use it; delete the silent client
  fallback; `unrecognised_count()` and the counting in `parse_results`/`record_sources`.
- `src/app/mod.rs` — pass the proxy into `HttpSource::new`; banner the build failure; render
  the unrecognised count into `SearchState`.
- `src/ui/mod.rs` — `SearchState.unrecognised: usize`.
- `src/ui/search.rs` — the footer line for the unrecognised count.
- `src/ui/settings.rs` — two rows: "Search Proxy URL" (`TextField::SearchProxy`) and
  "Proxy Applies To" (a new `RowKind::Cycle`, or a third `Toggle` if a two-state
  swarm/both split is enough). Rows go through `row_kind`/`row_label`/`text_field` so the
  view and `src/app/settings.rs` dispatch stay in agreement — bump `APP_ROWS` (17 → 19) and
  update the index tables in `row_kind`, `text_field`, `row_label`, and the tests at
  `src/ui/settings.rs:434-459`.
- `src/app/settings.rs` — commit arms for both rows.
- `docs/indexer-guide.md` — document `GET /engines` as an optional fourth endpoint.
- `docs/architecture.md` — one paragraph: plugins are the indexer's, the client is the
  aggregator.

**Deliberately not created:** any `src/plugins/`, any WASM/script host, any `SourceId` variant.

## Key APIs / libraries

- **`reqwest::Proxy::all("socks5://host:port")` + `ClientBuilder::proxy`.** Requires the
  `socks` cargo feature — confirmed present as `socks = []` in the vendored
  `reqwest-0.13.4/Cargo.toml:178` (read 2026-08-16). Use `socks5h://` when DNS should also
  resolve through the proxy; a search proxy that leaks DNS is only half a proxy, so make
  `socks5h` the documented default in the settings hint.
- **librqbit `SessionOptions.socks_proxy_url`** — already wired; unchanged by this plan except
  that `ProxyScope::Search` must pass `None` to it.
- **Torznab** (indexer track only): `?t=caps`, `?t=search`, `torznab:attr` seeders/peers.
  Sources checked 2026-08-16: [Jackett API wiki](https://github.com/Jackett/Jackett/wiki/Jackett-API),
  [SearXNG's Torznab engine docs](https://docs.searxng.org/dev/engines/online/torznab.html).

**New crates: none.** One new *feature flag* on a crate already in the tree. Justification: it
is the only way reqwest can speak SOCKS5, and FR-89 is unbuildable without it. If review
rejects the feature, the fallback is an HTTP `CONNECT` proxy (`reqwest::Proxy` works without
any feature for http/https proxies) and FR-89 narrows to HTTP proxies only — state that
narrowing in SPEC rather than shipping a proxy setting that silently ignores `socks5://`.

## Risks / edge cases

- **Two client builders is the whole bug class.** `src/sources.rs` builds a reqwest client in
  `new()` and a second, pool-disabled one inside `consume_stream` (the comment explains why:
  dropping a stream mid-body poisons keep-alive). Any proxy work that patches one and not the
  other ships a proxy that leaks on the streaming path — which is the *default* path. The
  shared `build_client()` exists to make that mistake impossible.
- **A loopback indexer through a proxy is usually wrong.** The default indexer URL is
  `http://127.0.0.1:8765`. Routing loopback through SOCKS will often just fail. Add
  `reqwest::Proxy::no_proxy` for loopback, or document it — do not let the common case break.
- **Rejected: a client-side plugin runtime.** Loading Python/JS scrapers into the TUI binary
  re-introduces the exact scraping stack `docs/architecture.md` removed for legal neutrality,
  and would need a script host (a large new dependency). Rejected once, here, in writing.
- **Rejected: `SourceId::Plugin(String)`.** Breaks `Copy` and `as_str() -> &'static str`; see
  finding 4. It is not a small change, it is a shared-types redesign.
- **Silent drop is the current behaviour and it is a bug, not a feature.** `SourceId::parse`
  returning `None` means an indexer that adds one engine loses those rows with no message.
  FR-88/step 3 is the minimum fix and is worth shipping even if nothing else in this issue does.
- **Scope honesty for the issue.** #58 lists three bullets. "Multi-engine aggregation +
  result filtering" is **already shipped** — this plan documents and tests it (FR-86).
  "Search proxy enforcement" is **built here** (FR-89/FR-90). "Plugin-based search engines"
  belongs to `harbour-indexer` and is step 4 in that repo, with the client-side
  forward-compatibility landing as FR-88. Close #58 on that basis; do not fabricate a client
  plugin loader.

## Test strategy

- **Unit, `src/sources.rs`** — `build_client(None)` succeeds; `build_client(Some("garbage://"))`
  returns `Err` (never a default client). A `parse_results` fixture containing a row with
  `"source":"some-new-plugin"` asserts `unrecognised_count() == 1` and that the *valid* rows
  still land (FR-14 unchanged).
- **Integration, `src/sources.rs` tests** — extend the existing `spawn_indexer` stub harness:
  point `HttpSource` at a stub SOCKS5 listener that only completes the greeting and records the
  requested destination, then assert the search attempted to reach the indexer *through it*.
  Same TcpListener-on-an-ephemeral-port pattern the file already uses, no new test dep.
- **Unit, `src/search.rs`** — no new tests needed; the four existing `merge` tests already pin
  FR-86. Add one assertion referencing FR-86 by number so the link is explicit.
- **Unit, `src/ui/settings.rs`** — the row-table tests at lines 434-459 must be updated with
  the new `APP_ROWS`; that is the guard that keeps view and dispatch aligned.
- **Buffer snapshot, `src/ui/tests.rs`** — the unrecognised-engines footer renders when the
  count is non-zero and is absent at zero.
- **No engine integration test** — nothing about the swarm changes.

## Verification

1. `SPEC.md` §4.2 contains FR-86…FR-90, and `src/search.rs`'s `merge` doc links FR-86.
2. Set `search_proxy_url = "socks5h://127.0.0.1:1080"` and `proxy_scope = "both"`, run a local
   SOCKS5 server with logging (`ssh -D 1080 localhost` is enough), point `indexer_url` at a
   **non-loopback** indexer, `cargo run`, and search. **The proxy log shows the connection.**
   That is the user-visible proof — an unlogged connection means the stream path leaked.
3. Stop the SOCKS server and search again: harbour shows an error banner and zero results. It
   must **not** quietly return results over a direct connection.
4. Point at an indexer whose `/search` returns one row with `"source":"torznab-plugin"`. The
   search footer reads "1 result from an engine this build does not recognise" instead of
   silently showing nothing.
5. `grep -rn "reqwest::Client::new()" src/sources.rs` returns nothing — the silent fallback is
   gone.
6. `grep -rn "Plugin(" src/core/types.rs` returns nothing — the frozen types are untouched.
