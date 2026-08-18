# RSS feeds + auto-download rules
Ref: #59

## Goal
Give harbour a client-side RSS feed manager and rule engine: feeds the user adds are polled,
matched against user rules, and matching items are enqueued into the existing queue with a
per-rule save folder — with every add auditable and nothing ever added twice.

## The findings that shape this plan

Read on **2026-08-16** from harbour's source and `Cargo.lock`.

**1. This is a client feature, not an indexer feature.** A rule's job is to *enqueue*, and the
queue (`src/queue.rs`), the ledger, and `Config` all live in the client. The indexer is a
stateless search proxy (`src/sources.rs:1-21`) with no notion of the user's queue.

**2. `SourceId::ShowRss` is a different thing and reviewers will conflate the two.**
`SourceId::ShowRss` (`src/core/types.rs:74-75`) is one of the ten *sites the indexer scrapes*.
This issue is about **user-supplied feed URLs polled by the client**. Say so in the module docs
or the two will be merged by mistake within a month.

**3. `quick-xml` is already in the build graph.** `Cargo.lock:2665` — `quick-xml 0.37.5`, pulled
by `librqbit-upnp 1.0.0` (`Cargo.lock:2032`), which is live because
`enable_upnp_port_forwarding` is wired in `src/engine/rqbit.rs:245`. Declaring
`quick-xml = "0.37"` as a direct dependency therefore adds **zero crates** to the build.

**4. `chrono 0.4.45` is transitively present (via `librqbit-dht`) but must not become a direct
dependency.** `src/core/types.rs:203-205` has the decision-site comment: *"Unix seconds rather
than a `DateTime` keeps `chrono` out of the tree for what is one integer."* `TorrentResult.added`
is `Option<i64>` unix seconds. Feed dates follow the same shape.

**5. The enqueue seam already exists and already dedupes.** `Queue::add(AddInput, now_ms)`
(used by `enqueue_magnet`, `src/app/actions.rs:473-496`) is the one entry point; FR-56 duplicate
detection lives behind it. A rule engine that calls it inherits dedupe, the concurrency cap, the
ledger, and crash-safe persistence for free.

**6. `EngineEvent` is a frozen shared type** (`AGENTS.md` rule 4, `src/core/types.rs:725-781`).
RSS refresh results must **not** add variants to it. The app loop already has a periodic tick
(`src/app/mod.rs:511-524`, `POLL_ACTIVE`/`POLL_IDLE`); RSS refresh hangs off that, or off its own
`mpsc` channel — not off the engine's enum.

**7. harbour has no "category" or "tag" concept, and `QueueItem` is persistence-frozen**
(`src/core/types.rs:387-416`). But `QueueItem.dir: PathBuf` is already per-item and already
respected by the engine. **Category maps cleanly onto a per-rule save directory. Tags do not map
onto anything and are deferred in writing.**

## SPEC / FR reference

**Exists today.** §4.4 FR-29…FR-41 (downloads), §4.6 FR-48…FR-56 (persistence, including FR-56
duplicate detection and FR-55 atomic writes). **Nothing in SPEC mentions RSS, feeds, rules, or
any automatic add.** An unattended process that adds torrents without a keypress is a
significant behavioural change and must be specified before it is built.

> **FR numbers here are proposed, not reserved.** Several plans in `docs/plans/` were drafted
> in parallel against the same free block (FR-86+), so numbers collide across files. Allocate
> final numbers when the SPEC edit lands (first merged wins, renumber the rest). The
> requirement *text* is the deliverable; the number is bookkeeping.

**Missing from SPEC — add first, then implement.** Proposed new section
**§4.9 RSS & auto-download (FR-91 … FR-97)**:

- **FR-91 (feeds are user state).** Feeds live in `<state>/feeds.json`, written with the same
  temp-file + atomic-rename discipline as the ledger (FR-55). A corrupt file is quarantined and
  the app starts with zero feeds plus a banner, never a crash (FR-54's rule, applied here).
- **FR-92 (feed manager).** A feeds screen lists every feed with URL, last refresh, item count,
  and last error. `a` adds a feed by URL, `x` removes one, `r` refreshes one now. Adding a feed
  fetches it once immediately so a typo is reported at the moment it is typed, not an hour later.
- **FR-93 (rules).** A rule is: a name, an optional feed filter (one feed or all), a
  **case-insensitive substring** `must contain` list, a `must not contain` list, a target save
  directory, and an enabled flag. All conditions are ANDed. Rules are stored in
  `<state>/rules.json` under the same durability rules as FR-91.
- **FR-94 (auto-add).** A feed item matching an enabled rule is enqueued through the normal
  queue path, inheriting FR-56 duplicate detection, the concurrency cap, and the ledger. The
  rule's directory becomes `QueueItem.dir`; an empty rule directory means the config default.
- **FR-95 (never twice).** Every auto-add is recorded by infohash in the rule's match history,
  so a feed that re-lists an item does not re-add it even after the item has been removed from
  the queue. The history is bounded and persisted.
- **FR-96 (auto-add is off until it is asked for).** No feed exists by default, and a newly
  added rule starts disabled. harbour never starts downloading something the user did not
  explicitly arrange.
- **FR-97 (auto-add is visible).** Every auto-add raises a banner naming the rule and the item.
  A background process that silently consumes disk and bandwidth is exactly the behaviour the
  project rules forbid.

Also add `f` (feeds screen) to SPEC's keybind table.

## Workstream

- **Engine & Foundation (Sarthak)** — `src/rss.rs` (fetch + parse), `src/rules.rs` (matching +
  match history), the `feeds.json`/`rules.json` store methods, the refresh scheduler.
- **Terminal UI (Ishan)** — `src/ui/feeds.rs`, the `Screen::Feeds` variant, the key dispatch.
- **Sources / indexer (Dhruv)** — none. This does not touch the indexer.

**Shared-type dependency:** none added. `EngineEvent`, `TorrentResult`, `Source`, `QueueItem`,
and `QueueStatus` are all read-only here. The new `FeedItem` / `Feed` / `Rule` types are
client-local and live in `src/core/feed.rs`, deliberately **not** in the frozen `core::types`.

## Approach

Each step compiles and is testable on its own.

**Step 1 — SPEC (docs only).** §4.9 FR-91…FR-97 plus the keybind row. ~50 lines.

**Step 2 — the parser (engine track, no I/O).** `src/rss.rs`: `parse_feed(xml: &str) ->
Result<Vec<FeedItem>, FeedError>` over `quick-xml`'s `Reader::read_event`. Fields taken:
`<title>`, `<link>`, `<enclosure url=…>`, `<guid>`, `<pubDate>`. The magnet is whichever of
`link`/`enclosure`/`guid` starts with `magnet:` — torrent RSS feeds disagree about which element
carries it, so try all three in that order and skip an item with none. `pubDate` (RFC 2822) is
converted to unix seconds by a ~30-line hand-rolled parser with a month table, matching
`TorrentResult.added`'s shape and keeping `chrono` out of the direct tree. One malformed
`<item>` is skipped, not fatal — the same rule as `parse_results` (FR-14). **Pure function, no
network, fixture-tested.** ~250 lines including fixtures. This is the highest-value step and it
is reviewable with zero moving parts.

**Step 3 — persistence (engine track).** `src/core/feed.rs` with `Feed`, `Rule`, `FeedItem`,
`MatchHistory`; `Store::load_feeds`/`save_feeds`/`load_rules`/`save_rules` in `src/persist.rs`
reusing the existing `atomic_write` and `Loaded<T>` recovery. ~150 lines, unit-tested against a
temp `HARBOUR_STATE_DIR`.

**Step 4 — matching (engine track, no I/O).** `src/rules.rs`:
`fn matches(rule: &Rule, item: &FeedItem) -> bool` — lowercase substring containment for every
`must contain`, none of `must not contain`, feed filter respected. Plus
`fn to_add_input(rule, item) -> Option<AddInput>` which derives the infohash via the existing
`crate::core::magnet::info_hash_from_magnet`. Pure, ~120 lines, heavily unit-tested.

**Step 5 — the fetch + refresh loop (engine track).** `FeedFetcher` holding a `reqwest::Client`
with a hard deadline; a refresh returns `Vec<FeedItem>` or an error stored on the feed. Driven
from the existing app-loop tick with a `last_refresh` per feed. Refresh runs on
`tokio::spawn` and reports back over its own `mpsc::UnboundedSender<FeedEvent>` — **a new,
RSS-local enum, not `EngineEvent`**. The app loop selects on it alongside `events_rx`.

**Step 6 — auto-add wiring (engine track).** For each new item × each enabled rule: match →
skip if the infohash is in the rule's match history → `Queue::add` with the rule's dir →
record in match history → persist → `app.warn` naming rule and item (FR-97).

**Step 7 — the feeds screen (UI track).** `src/ui/feeds.rs` as a pure `draw`, two panes (feeds
above, rules below), the `row_kind`/`row_label` shared-layout pattern from `src/ui/settings.rs`
so the view and `src/input.rs` dispatch agree on what row N is. `Screen::Feeds` + `f` from the
downloads screen. Add/remove/edit reuse the existing inline-edit-buffer idiom
(`FolderPrompt`/`SettingsState`), not a new overlay framework.

## Files to create / modify

**Create**
- `src/rss.rs` — feed XML parsing, the RFC 2822 date parser, `FeedError`.
- `src/rules.rs` — `matches`, `to_add_input`, match-history bookkeeping.
- `src/core/feed.rs` — `Feed`, `Rule`, `FeedItem`, `MatchHistory`, `FeedEvent`.
- `src/ui/feeds.rs` — the feeds/rules view (pure paint + `row_kind`/`row_label`).
- `tests/fixtures/rss/` — captured feeds: a Nyaa-style RSS with `<link>` magnets, an EZTV-style
  one with `<enclosure url="magnet:…">`, a ShowRSS-style one, one with a malformed item, one
  with CDATA titles, and one with no magnet anywhere.

**Modify**
- `SPEC.md` — §4.9 (FR-91…FR-97) and the keybind table.
- `Cargo.toml` — `quick-xml = "0.37"`.
- `src/core/mod.rs` — `pub mod feed;`.
- `src/main.rs` — `mod rss; mod rules;`.
- `src/core/paths.rs` — `feeds_file(root)`, `rules_file(root)`, next to `ledger_file`.
- `src/persist.rs` — the four load/save methods.
- `src/ui/mod.rs` — `Screen::Feeds`, `FeedsState`, `AppState.feeds`.
- `src/app/mod.rs` — the `FeedEvent` channel in the `tokio::select!`, the refresh scheduler
  hook on the existing tick, `Screen::Feeds` in `draw`.
- `src/app/actions.rs` — `auto_add(app, rule, item)`.
- `src/input.rs` — the `Screen::Feeds` key table and `f` from downloads.
- `src/ui/help.rs` — the new keys.

## Key APIs / libraries

- **`quick-xml = "0.37"`** — *the one new direct dependency, and it costs nothing.* Verified
  2026-08-16: `quick-xml 0.37.5` is already resolved in `Cargo.lock:2665` via
  `librqbit-upnp 1.0.0`. Pinning `"0.37"` unifies with that copy; pinning `"0.39"` (the current
  release) would compile a **second** copy. Use `Reader::read_event` in the event-streaming
  style, `Reader::config_mut().trim_text(true)`, and handle `Event::CData` as well as
  `Event::Text` — torrent feeds put titles in CDATA constantly.
  **Risk to record:** the pin now tracks librqbit's transitive version; revisit on every
  librqbit bump.
- **Rejected: `feed-rs`** (current release 2.4.0, checked on
  [crates.io](https://crates.io/crates/feed-rs) 2026-08-16). It is the better *general* feed
  parser — RSS + Atom + JSON Feed with one data model — but it brings its own subtree and would
  very likely pull a second `quick-xml`. harbour needs five fields from RSS 2.0. Under
  "lean dependency tree, justify every crate", one already-compiled crate beats a new subtree.
  Recorded here so it is rejected once, with a reason, rather than re-proposed.
- **Rejected: `regex`.** Substring matching plus the `SxxEyy` parser from #60 is deterministic
  and testable; a regex field would be a user-facing footgun and a new crate. Note it as a
  possible later opt-in, not a v1 field.
- **`reqwest`** — already a dependency. Reuse the `connect_timeout(5s)` + outer
  `tokio::time::timeout` pattern from `src/sources.rs:146-187`.
- **`crate::core::magnet::info_hash_from_magnet`** — already exists (`src/core/magnet.rs`), and
  is what makes FR-95's dedupe key free.

## Risks / edge cases

- **An unattended adder is the scariest thing in this issue.** A rule matching `""` or a
  too-loose `must contain` will enqueue an entire feed. Mitigations, all normative: FR-96 (new
  rules start disabled), FR-97 (every add banners), a rejected-empty-`must contain` validation,
  and a per-refresh add cap (e.g. 20) that stops and banners rather than draining a feed.
- **Feeds that re-list items forever.** FR-95's per-rule match history is the answer; bound it
  (e.g. 500 hashes, FIFO) so `rules.json` cannot grow without limit.
- **`.torrent`-URL feeds, not magnet feeds.** Many private trackers publish `<enclosure>` links
  to `.torrent` files. v1 handles magnets only and **skips** those with a visible "N items
  skipped: no magnet" count on the feed row. Wiring the URL path means fetching bytes and
  calling `Engine::add_bytes` — real work, and it overlaps `docs/plans/add-torrents.md`'s FR-72
  URL-add. Defer to that plan rather than building a second downloader.
- **Timezone junk in `pubDate`.** Feeds emit `GMT`, `UTC`, `+0000`, and named US zones. Parse
  numeric offsets and `GMT`/`UT`/`Z`; treat anything else as UTC and note it. `pubDate` is
  display-only here — it must never gate whether an item is added, or a bad clock silently stops
  downloads.
- **Feed poll on a laptop that sleeps.** Schedule on elapsed wall time, not on tick counts, so a
  resumed laptop refreshes once rather than replaying missed intervals.
- **Do not touch `EngineEvent`.** It is frozen. Using a separate `FeedEvent` channel is not
  stylistic — a variant added here would need Sarthak's sign-off and would ripple into every
  exhaustive match in `src/app/actions.rs`.
- **Tags are out of scope, in writing.** `QueueItem` is persistence-frozen and nothing in the UI
  renders a tag. "Download to category" ships as the per-rule save directory (FR-94); "tags"
  does not ship. Close the issue on that basis rather than adding a field nothing reads.

## Test strategy

- **Unit, `src/rss.rs`** — one test per fixture: magnet in `<link>`, magnet in `<enclosure>`,
  magnet in `<guid>`, CDATA titles, a malformed `<item>` between two good ones (both good ones
  survive), an item with no magnet (skipped, counted), and a completely non-XML body (hard
  error). Plus a date table: `Tue, 12 Aug 2026 09:15:00 +0000`, `… GMT`, a bad string → `None`.
- **Unit, `src/rules.rs`** — must-contain AND semantics, must-not-contain veto, case
  insensitivity, feed scoping, a disabled rule never matching, and the match-history bound
  evicting FIFO.
- **Unit, `src/persist.rs`** — `feeds.json`/`rules.json` round-trip; a corrupt file yields
  `Loaded::Recovered` with a warning and an empty list, never a panic.
- **Integration (local, no network), `src/rss.rs` tests** — the `spawn_indexer`-style
  `TcpListener` stub from `src/sources.rs:633-671` serving a fixture feed proves fetch → parse →
  match → `AddInput` end to end against `FakeEngine`. No `HARBOUR_TEST_NET` needed; nothing here
  touches a real swarm.
- **Buffer snapshot, `src/ui/tests.rs`** — the feeds screen with zero feeds (empty state), with
  one healthy feed, and with one errored feed (the error is visible on the row).

## Verification

1. `SPEC.md` §4.9 contains FR-91…FR-97 and the keybind table lists `f`.
2. `cargo run` → `f` opens the feeds screen showing the empty state. Add a public magnet RSS
   feed (e.g. a SubsPlease/Nyaa RSS URL). **The row immediately shows an item count**, or a
   readable error if the URL is wrong — verified at type time, per FR-92.
3. Add a rule with `must contain = ["1080p"]` and a save folder, enable it, press `r` to refresh.
   A banner names the rule and the item, the downloads screen shows the new item, and the item's
   files land in the rule's folder — not the default one.
4. Press `r` again. **Nothing is added a second time** (FR-95) and no banner fires.
5. Restart harbour, press `r` again. Still nothing added — the match history survived the
   restart, which is the whole point of persisting it.
6. `grep -n "EngineEvent" src/rss.rs src/rules.rs src/core/feed.rs` returns nothing — the frozen
   enum was not widened.
7. `cargo tree -d | grep quick-xml` shows a single `quick-xml` — the pin did what it claimed.
