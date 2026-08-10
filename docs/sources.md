# harbour — sources

How harbour finds torrents: the `Source` trait, the 10 sources, per-source scraping
strategy, parsing, the magnet builder, the network layer, the result cache, and
health reporting. Implements phase 3 ("Real scrapers + cache") of the roadmap.

Exact DOM selectors and endpoint shapes are captured in fixture tests
(`tests/fixtures/<source>/`) at implementation time; anything still open is called
out explicitly rather than assumed.

## 1. Data model

### 1.1 `Source` trait

```rust
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Group { Games, Movies, Tv, Anime }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SourceId {
    FitGirl, Yts, TpbMovies, X1337Movies, Eztv,
    TpbTv, X1337Tv, Nyaa, SubsPlease, Bittorrented,
}
// `Group::label()` → "Games" | "Movies" | "TV" | "Anime";
// `SourceId::as_str()` → the lowercase table ids ("fitgirl", "tpb-movies", …).

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("network: {0}")] Network(String),
    #[error("parse: {0}")] Parse(String),
    #[error("source rejected request (rate limit / bot check): {0}")] Blocked(String),
}

/// A torrent search backend. One instance per row of the source matrix.
/// Stateless on purpose: all state (cache, offline status) lives in the
/// engine, so a source can never leak state between searches.
pub trait Source: Send + Sync + 'static {
    fn id(&self) -> SourceId;
    fn label(&self) -> &'static str;
    fn groups(&self) -> &'static [Group];
    fn homepage(&self) -> &'static str;

    /// False when the source's results carry no trustworthy seeder counts
    /// (e.g. an RSS feed without seed fields): the UI renders a neutral dot
    /// and the engine never sorts by seeders for this source.
    fn reports_health(&self) -> bool;

    /// Search this source for `query`. Empty `query` = curated top list.
    /// Must be abort-safe (the engine cancels on quit/timeout). Errors are
    /// per-source: the engine marks the source offline and keeps searching.
    async fn search(&self, query: &str) -> Result<Vec<TorrentResult>, SourceError>;
}

pub type ArcSource = Arc<dyn Source>;
```

### 1.2 `TorrentResult`

```rust
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorrentResult {
    /// 40-char lowercase hex infohash — always normalized to lowercase by the
    /// magnet builder; no source may hand us uppercase.
    pub info_hash: String,
    pub name: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    /// Some sources don't expose a file count (most RSS feeds).
    pub num_files: Option<u32>,
    /// The source this result came from — drives the source tag in the UI.
    pub source: SourceId,
    /// Prebuilt via `build_magnet`, so every consumer gets identical URLs.
    pub magnet: String,
    /// Publication date when the source provides one (RSS pubDate, API
    /// date_uploaded). None when unavailable.
    pub added: Option<DateTime<Utc>>,
}
```

`TorrentResult` is `Serialize`/`Deserialize` because the search cache persists it
verbatim (section 7). `info_hash` stays a `String` (not `[u8; 20]`) — every
consumer treats it as text.

## 2. Source matrix

| id | label | groups | kind | hosts / endpoints | search hits |
| --- | --- | --- | --- | --- | --- |
| fitgirl | FitGirl | Games | HTML | fitgirl-repacks.site (`/?s=<q>` WordPress search) | site search, per-post magnet extraction |
| yts | YTS | Movies | JSON | `yts.mx` → `yts.am` → `yts.rs` fallback, `/api/v2/list_movies.json` | API search, `sort_by=seeds` |
| tpb-movies | TPB | Movies | JSON | `apibay.org` `/q.php?q=<q>&cat=200` | API search, Movies category |
| x1337-movies | 1337x | Movies | HTML | `1337x.to` `/search/<q>/<page>/` | HTML search results, category filter |
| eztv | EZTV | TV | RSS | eztv mirror chain, `/ezrss.xml` | full recent-episode feed, filtered locally |
| tpb-tv | TPB | TV | JSON | `apibay.org` `/q.php?q=<q>&cat=205` | API search, TV category |
| x1337-tv | 1337x | TV | HTML | `1337x.to` `/search/<q>/<page>/` | HTML search results, category filter |
| nyaa | Nyaa | Anime | RSS | `nyaa.si` `/?page=rss&q=<q>&c=1_0` | server-side RSS search, anime category |
| subsplease | SubsPlease | Anime | RSS | `subsplease.org` `/rss/` | full recent-release feed, filtered locally |
| bittorrented | BitTorrented | Movies | HTML | bittorrented homepage + search page | HTML results table with magnets |

Games is the only group with a single source: FitGirl alone (trusted repacker,
per design). Every source resolves its primary host at construction and rotates
through fallbacks in order.
## 3. Per-source strategies

### 3.1 fitgirl (Games, HTML)

- **URL**: `https://fitgirl-repacks.site/?s=<q>` (WordPress search). Empty query →
  the "All Posts" index (browse top-list path).
- **Parse**: results give post titles + links only; magnets live in each post
  body, so search issues **one follow-up request per matching post** to extract
  `magnet:` URIs and repack size — deduplicated, bounded to the first N posts.
- **Fallback hosts**: canonical domain + config mirror list, probed in order.
- **Failure modes**: WordPress search only matches site text — queries absent from
  titles/content return nothing even when a repack exists; post pages are megabytes
  of HTML; no seeders anywhere → `reports_health() = false`; follow-up bursts can
  trip rate limits (throttle, reuse one session).

### 3.2 yts (Movies, JSON)

- **URL**: `https://yts.mx/api/v2/list_movies.json?query_term=<q>&sort_by=seeds&order_by=desc&limit=50`.
- **Parse**: `serde_json` → `data.movies[]`; one result per `torrents[]` entry (a
  multi-quality movie yields multiple results, each with its own hash). Size from
  `size_bytes`, seeders from `seeds`, `added` from `date_uploaded_unix`.
- **Fallback hosts**: `yts.mx` → `yts.am` → `yts.rs`, same API path; rotate on
  connection failure or 429.
- **Failure modes**: aggressive rate limiting (429) — back off and mark offline,
  don't hammer; some entries carry a null torrent hash (missing files) — skip;
  the host chain rotates often, so fallbacks are config, not code.

### 3.3 tpb-movies / tpb-tv (Movies, TV; JSON)

- **URL**: `https://apibay.org/q.php?q=<q>&cat=200` (movies) / `cat=205` (TV).
  The two sources are **one struct with a different category constant** — a single
  apibay parser, two `SourceId`s.
- **Parse**: `serde_json` → flat array (`name`, `info_hash`, `size` as byte-string,
  `seeders`, `leechers`, `added`).
- **Failure modes**: category filtering is server-side only — dropping `cat` mixes
  Movies and TV, which is exactly why the two sources hard-code distinct categories
  instead of sharing a "tpb" source; apibay is an unofficial TPB API mirror and is
  flaky (downtimes, garbage entries) — treat empty/garbage payloads as offline,
  don't retry into a hole.

### 3.4 x1337-movies / x1337-tv (Movies, TV; HTML)

- **URL**: `https://1337x.to/search/<q>/<page>/` — page 1, 2, … appended. Category
  narrowing uses the site's category search path (Movies vs TV); exact path shape
  is fixture-pinned in phase 3.
- **Parse**: `scraper` over the results table — one `<tr>` per result; title
  anchor → detail page URL; size ("2.4 GB" text → bytes) and seeders/leechers from
  cells; the magnet lives on the detail page, so like fitgirl this is a bounded,
  deduplicated follow-up per result. `seeds/desc` sort links bias top results.
- **Failure modes**: pagination is the known trap — search must walk pages 1..N
  (or stop at a result cap) or users only ever see page 1; Cloudflare-style
  challenges intermittently block scrapes and parse as zero results, so the
  parser must detect "no table + challenge marker" and report `Blocked`, not an
  empty search; markup drift is the long-tail risk (fixtures catch it).

### 3.5 eztv (TV, RSS)

- **URL**: `https://<mirror>/ezrss.xml` — standard feed of recent episodes. Search
  filters the feed locally by title substring; mirrors that accept a server-side
  query param are detected and pinned per mirror at implementation time.
- **Parse**: `quick-xml` → `<item>`s; title, torrent link (enclosure or item
  link), seeders from the torrent namespace when present, `added` from pubDate;
  info_hash from the magnet/`.torrent` link in the description when present.
- **Fallback hosts**: mirrors churn constantly (`eztvx.to`, `eztv.re`, older dead
  domains) — the list is config, each search probes in order.
- **Failure modes**: a hard-coded host rots within months (config list + fixtures
  are the defense); the feed only covers recent episodes, so old-season searches
  return nothing even though episodes exist; namespace tags vary across mirrors —
  parse defensively, never fail an item on one missing field.

### 3.6 nyaa (Anime, RSS)

- **URL**: `https://nyaa.si/?page=rss&q=<q>&c=1_0` — server-side RSS search pinned
  to the anime category (`1_0`).
- **Parse**: `quick-xml` → `<item>`s; title, size from the description, seeders/
  leechers from the nyaa namespace, info_hash from the magnet in the description
  (regex `magnet:\?xt=urn:btih:[a-f0-9]{40}`).
- **Fallback hosts**: nyaa.si is canonical; mirror instances are unsanctioned and
  only added to config after manual verification — a dead nyaa.si reports offline
  rather than chasing mirrors.
- **Failure modes**: periodic DDoS downtime (short per-source timeout + offline
  reporting keeps the app usable); category must stay pinned or live-action and
  manga torrents leak into anime results.

### 3.7 subsplease (Anime, RSS)

- **URL**: `https://subsplease.org/rss/` — full feed of recent releases.
- **Parse**: `quick-xml` → `<item>`s; info_hash from the magnet in the description.
  No size/seeders in the feed → `reports_health() = false`, `size_bytes = 0`,
  `seeders = 0` (UI renders size as "—").
- **Failure modes**: the feed only carries the last ~50–100 releases — search is
  a local filter, so older releases return nothing (stated limitation, not a
  bug); no seeders means seed-sorted results are impossible for this source;
  feed fetch is throttled (small site).

### 3.8 bittorrented (Movies, HTML)

- **URL**: homepage + site search page; the site posts curated picks with magnets
  on the page itself.
- **Parse**: `scraper` over the results table; anchors matching
  `magnet:?xt=urn:btih:` are the primary signal, title/size from adjacent cells.
- **Failure modes**: page structure is unverified until phase 3 fixture work —
  selectors are pinned there, not assumed here; small curated catalog means sparse
  results by design; markup drift and domain availability are standing risks.
  **Open question**: whether the site exposes a stable search path at all — if
  not, this source ships browse-only (empty query) until proven otherwise.

## 4. Parsing

Three parsers, one per `kind`, each with fixture tests.

### 4.1 RSS — `quick-xml`

`quick_xml::Reader` streams the feed; a state machine walks `<item>`/`<entry>`
boundaries and collects child text. Namespaced tags (`torrent:seeders`, nyaa
fields) are matched by full `{namespace}local` name so mirror variants don't
break parsing. A malformed item is skipped, never fatal to the feed. info_hash
extraction is a single regex over the magnet/description text; items without a
hash are dropped (unusable for librqbit).

### 4.2 HTML — `scraper`

`scraper::Html::parse_document` + `Selector` for CSS-style queries (table rows,
anchors, size cells). Text sizes ("2.4 GB", "850 MB") parse to `size_bytes`
through one shared helper with fixture cases. Magnets match
`magnet:?xt=urn:btih:[a-f0-9]{40}` by attribute or inline regex. Detail-page
follow-ups are bounded and throttled (fitgirl, 1337x). Anti-bot: one persistent
client session per source with a browser-like `User-Agent` and cookie jar; a
challenge page must be detected (`Blocked` error), not parsed as zero results.

### 4.3 JSON — `serde_json`

One `#[derive(Deserialize)]` struct per API shape (`yts` nested `data.movies[]`,
apibay flat array), `#[serde(rename_all = "camelCase")]` where the API is
camelCase, unknown fields ignored so API additions don't break parsing. Missing
required fields → `Parse` error → source offline for that search.

## 5. Magnet builder

```rust
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

/// Single source of truth for magnet construction. Everything downstream —
/// download queue, history, cache — consumes this, never hand-rolled strings.
///
/// The infohash is normalized to lowercase here: sources may return mixed
/// case, but the magnet spec and librqbit both treat it as case-insensitive
/// hex, so we canonicalize once at the boundary.
pub fn build_magnet(info_hash: &str, name: &str) -> String {
    format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        info_hash.to_ascii_lowercase(),
        utf8_percent_encode(name, NON_ALPHANUMERIC),
    )
}
```

Produces exactly `magnet:?xt=urn:btih:<lowercase infohash>&dn=<urlencoded name>`.
`dn` is percent-encoded (`percent-encoding`, `NON_ALPHANUMERIC` set) so spaces,
unicode, and `&` in titles can't corrupt the URL. Trackers are deliberately
omitted — librqbit adds its own at add time, and the cached magnet must stay
stable for dedupe.

## 6. Network layer

One `reqwest::Client` **per source**, built at source construction:

- **Timeout**: per-source overall timeout (config, default 10 s) covering the whole
  search including follow-up fetches — a hung source can never stall the engine.
- **Retries**: connection failures and 5xx retried with exponential backoff
  (2 retries, 200 ms / 800 ms base). 4xx (rate limit, blocked) are **not**
  retried — they're terminal `Blocked`/`Offline` signals.
- **Abort**: a `tokio_util::sync::CancellationToken` per search; quit, timeout,
  or a new search for the same source cancels the in-flight future (sources must
  be abort-safe, section 1.1).
- **Isolation**: each source runs as its own tokio task with its own client; a
  panicking, hanging, or erroring source touches nothing else — the engine
  collects per-source outcomes and proceeds with whatever answered.
- **Offline reporting**: terminal failure emits `SourceStatus::Offline
  { source, reason }` on the engine event channel → the sidebar dot goes dim,
  the search continues. Offline is **per-search**, not sticky — every new search
  re-probes all sources.

`HARBOUR_MAX_DOWNLOADS` bounds the *download* concurrency; search is always
all-10-in-parallel.

## 7. Cache

Per-(source, query) JSON cache, 5-minute TTL, absorbing repeated searches
(arrow-key browsing, top-list pokes) without re-hitting flaky hosts.

**Layout** (config dir, see persistence doc):

```
~/.harbour/
  cache/
    search/<source_id>/<urlencoded query>.json
    torrents/<info_hash>.torrent      # engine metadata capture, not this module
    covers/                           # phase 7
```

`<source_id>` is the table id (`tpb-movies`, `x1337-tv`, …); the query filename
is percent-encoded so any query maps to a safe path.

**File shape**: `{ "fetched_at": <unix_ts>, "results": [TorrentResult...] }` —
`results` is the raw list from section 1.2, serialized verbatim.

**Semantics**:

- **TTL**: entries younger than 300 s serve without network I/O; stale entries
  are re-fetched and overwritten. No refresh-in-background — the UI streams per
  source anyway, so a stale cache just costs one re-fetch.
- **Negative caching**: an *empty* result list is cached for the same TTL when
  the source answered successfully with no matches, so top-list browsing doesn't
  hammer sources that legitimately return nothing.
- **No caching of failures**: network/parse errors never write cache entries —
  a dead source is never resurrected from cache.
- **Invalidation**: TTL expiry on read is the only mechanism; nothing is ever
  invalidated eagerly. Deleting `cache/` is always safe — it rebuilds on the
  next search. Entries from a changed schema fail deserialization and are
  discarded on read.

## 8. Health reporting (sidebar dots)

The search-view sidebar lists sources under their group; each row carries a
one-character health dot, driven by engine state (not the source itself):

| state | dot | meaning |
| --- | --- | --- |
| Online | green | last search answered with results |
| Empty | dim | last search answered with zero results (reachable, nothing found) |
| Offline | red/dim | last search failed or timed out; `SourceStatus::Offline` received |

`reports_health()` is orthogonal: it controls whether a source's *seeders* are
trusted (sorting, "health" coloring of result rows). SubsPlease and FitGirl
report no health data → neutral dot glyph and "—" seeders regardless of online
state. Unprobed sources render as unknown (dim) until the first search.

## 9. Future sources register

Two candidates are deferred to phase 7 spikes. Both are **unproven** — honest
status, not a roadmap promise.

### 9.1 cs.rin.ru (Steam Underground forum)

- **Why wanted**: scene-standard repacks and game content beyond FitGirl's
  catalog; would round out the Games group.
- **Why feasibility is unproven**: behind Cloudflare (bot challenge on first
  hit); a phpBB-style **forum**, not a catalog — search is forum search, results
  are threads, and the "torrent" is a magnet buried in an arbitrary post (or a
  `.torrent` attachment requiring login on some boards); no clean API.
- **Spike plan (phase 7)**: headless probes over a week — measure challenge
  rate, confirm thread-search → magnet extraction on a sample, check login
  requirements. Go/no-go: park it if >20% of probes hit a challenge or magnets
  are inconsistently reachable. Best case it's a lower-trust source than FitGirl
  and needs explicit user opt-in.

### 9.2 online-fix.me

- **Why wanted**: scene fixes/updates (crack-only updates for online games) —
  content no other source carries.
- **Why feasibility is unproven**: Cloudflare-protected; catalog pages are
  JS-rendered (title/size/magnet not in static HTML — the scraper would need a
  real browser or a reverse-engineered internal API); magnets live on per-release
  pages, so every search is N follow-up fetches against a fragile target.
- **Spike plan (phase 7)**: same probe pattern as cs.rin.ru, plus a
  browser-automation experiment to determine whether the internal data API can
  be called directly. Go/no-go on the same metrics; no API access → deferred.

### 9.3 Standing rule for new sources

A source ships only when it can be exercised by **fixture tests with real
captured HTML/JSON/RSS** (no mock-by-hand data), passes the live smoke test
against the current host, and is added to the matrix with an explicit
`reports_health` decision. Unverifiable sources stay in this register.
