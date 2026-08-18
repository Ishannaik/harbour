# Per-feed refresh + episode filters
Ref: #60

## Goal
Make the RSS layer controllable and season-aware: a refresh interval per feed, a smart episode
filter that understands `SxxEyy` and only downloads episodes it has not already taken, and a
readable item header with a short date and a key that opens the article link.

## Dependency on #59 — read this first

**This plan is unbuildable until `docs/plans/rss-feeds-auto-download.md` steps 2–4 have landed.**
It builds directly on types that plan creates and does not redefine any of them:

| Type | Defined by | Used here |
| --- | --- | --- |
| `Feed` | #59 step 3, `src/core/feed.rs` | gains `refresh_secs: Option<u64>` |
| `Rule` | #59 step 3, `src/core/feed.rs` | gains `episode_filter: Option<String>` |
| `FeedItem` | #59 step 3, `src/core/feed.rs` | read-only (`title`, `link`, `published`) |
| `MatchHistory` | #59 step 3 | joined by the new seen-episode set |
| `parse_feed` | #59 step 2, `src/rss.rs` | read-only |
| `matches()` | #59 step 4, `src/rules.rs` | gains the episode predicate |
| `FeedsState` | #59 step 7, `src/ui/mod.rs` | gains the item-detail header |

If #60 is picked up first, the correct move is to land #59 steps 2–4 as its own PR rather than
to duplicate the types here. Every field added below is `#[serde(default)]` so a `feeds.json` /
`rules.json` written by the #59-only build still loads.

## The findings that shape this plan

Read on **2026-08-16** from harbour's source.

**1. The refresh scheduler must be wall-clock, not tick-count.** `src/app/mod.rs:511-524` polls
on `Instant::now().duration_since(last_poll)` at `POLL_ACTIVE` (500ms) / `POLL_IDLE` (5s). A
per-feed interval hangs off the same comparison. Counting ticks would make a sleeping laptop
replay every missed refresh at once.

**2. The file/URL opener is already decided, in this repo, by a sibling plan.**
`docs/plans/open-finished-files.md` specifies `src/core/openers.rs` with `open_default(path)` and
`open_reveal(path)`, built on platform `Command`s — `cmd /C start "" <path>` on Windows, `open`
on macOS, `xdg-open` on Linux — and explicitly **no new dependency** (the `open` crate is
replaced). `openers::open_default` takes a path *or a URL*: all three commands accept a URL.
**Reuse it; do not re-decide it.**

**3. Dates are unix seconds throughout.** `TorrentResult.added: Option<i64>`
(`src/core/types.rs:203-205`) with the decision-site comment about keeping `chrono` out of the
tree. #59's `FeedItem.published` follows the same shape, so the "short date format" here is a
formatter over `i64`, not a date library.

**4. `SourceId::AnimeTosho`/`SubsPlease`/`Eztv`/`ShowRss` exist as indexer sites, and their real
feeds are the exact naming zoo this filter must survive.** Anime feeds are overwhelmingly
absolute-numbered (`Show - 12 [1080p]`), Western TV feeds are `S01E12`. A filter that only
understands one of the two is a filter that silently does nothing on half the user's feeds.

## SPEC / FR reference

**Exists after #59.** §4.9 FR-91…FR-97 (feeds, rules, auto-add, never-twice, off-by-default,
visible). FR-95 is per-*infohash* — a feed that re-publishes the same episode as a different
release (a proper, a repack, a second group) has a different infohash and would be added again.
That gap is exactly what the episode filter closes.

> **FR numbers here are proposed, not reserved.** Several plans in `docs/plans/` were drafted
> in parallel against the same free block (FR-86+), so numbers collide across files. Allocate
> final numbers when the SPEC edit lands (first merged wins, renumber the rest). The
> requirement *text* is the deliverable; the number is bookkeeping. This plan's numbers must
> stay contiguous with #59's §4.9 block, whatever that block ends up being.

**Missing from SPEC — add first, then implement.** Proposed **FR-98 … FR-102**, extending §4.9:

- **FR-98 (per-feed refresh interval).** Each feed carries its own refresh interval; unset means
  the global default. The minimum accepted interval is 60 seconds — a client that hammers a
  public feed gets the user rate-limited or banned, so the floor is enforced at the point of
  entry, not left to the user's discretion. Refresh is scheduled on elapsed wall time.
- **FR-99 (episode filter).** A rule may carry an episode filter expressed as a season/episode
  range (`S01E05-S01E12`, `S02E01-`, `1-24`). An item is auto-added only if an episode number
  can be parsed from its title **and** falls in range. If the filter is set and no episode can be
  parsed, the item is **skipped and counted** — never added on a guess.
- **FR-100 (smart episode filter / never the same episode twice).** With the smart filter on, a
  rule records the season/episode of everything it has taken and refuses a second release of the
  same episode, regardless of infohash. This is what stops a repack, a proper, and a second
  fansub group from all landing in the same folder.
- **FR-101 (item header).** The feeds view shows, for the selected item: title, short date
  (`12 Aug 09:15`, or `12 Aug 2025` when the item is not from the current year), the parsed
  episode (or `—`), and the article link. An item with no publication date renders `—`, never a
  fabricated or epoch-zero date.
- **FR-102 (open link).** `enter` on a selected feed item opens its article link in the system
  browser via the FR-8x opener, using the same platform commands as the open-finished-files
  path. `http`/`https` links only; a `magnet:` link is downloaded, not opened.

## Workstream

- **Engine & Foundation (Sarthak)** — `src/rss/episode.rs` (parsing + range matching), the
  seen-episode store, the per-feed scheduler in `src/app/mod.rs`.
- **Terminal UI (Ishan)** — the item-detail header in `src/ui/feeds.rs`, the short-date
  formatter, the settings/edit rows for interval and filter, the `enter` binding.
- **Sources / indexer (Dhruv)** — none.

**Shared-type dependency:** none added. This extends #59's client-local types only; `core::types`
is untouched.

## Approach

**Step 1 — SPEC (docs only).** FR-98…FR-102 into §4.9. ~35 lines.

**Step 2 — the episode parser (engine track, pure).** `src/rss/episode.rs`:

```rust
pub struct Episode { pub season: Option<u16>, pub number: u16 }
pub fn parse_episode(title: &str) -> Option<Episode>
```

Patterns, tried in this order (first match wins; order matters because `1x05` and `S01E05`
overlap with resolution and year tokens):

1. `S01E05` / `s1e5` — season + episode.
2. `1x05` — season + episode.
3. `Season 1 Episode 5` — season + episode.
4. `- 12 ` / `- 12v2` / `Ep 12` / `E12` — absolute number, no season. The anime convention.

**Guards that make this honest rather than clever:**
- Never read a resolution as an episode: `1080p`, `720p`, `2160p`, `x264`, `x265`, `H.264`,
  `10bit`, `5.1`, `AAC2.0` are masked out of the title before pattern 4 runs.
- Never read a year as an episode: a bare 4-digit `19xx`/`20xx` is masked out.
- Pattern 4 requires a delimiter on both sides, so `Show2` is not episode 2.

Hand-written scanning, **no `regex` crate** — the same call `src/sources.rs` made for its
`urlencode`. ~200 lines including a large table test. Pure, no I/O, reviewable alone.

**Step 3 — filter ranges (engine track, pure).**
`parse_filter("S01E05-S01E12") -> EpisodeFilter` and `EpisodeFilter::contains(&Episode) -> bool`.
Accepted forms: `SxxEyy-SxxEyy`, `SxxEyy-` (open ended), `SxxE*` (a whole season), `n-m` and
`n-` (absolute). An unparseable filter string is an **error the rule editor rejects on Enter**,
not a filter that silently matches everything. ~120 lines.

**Step 4 — seen episodes (engine track).** `Rule` gains
`seen_episodes: BTreeSet<(u16, u16)>` (`#[serde(default)]`, season 0 = absolute-numbered).
Recorded at auto-add time next to #59's infohash history, in the same `rules.json` atomic write.
`matches()` consults it when the smart filter is on. ~80 lines.

**Step 5 — per-feed refresh (engine track).** `Feed.refresh_secs: Option<u64>` and
`Feed.last_refresh: Option<i64>` (unix seconds, so it survives a restart —
`Instant` does not serialize). The scheduler picks feeds where
`now - last_refresh >= refresh_secs.unwrap_or(global_default)`, clamps the interval to a 60s
floor at load *and* at edit, and refreshes at most one feed per tick so ten feeds never fire one
burst. ~100 lines.

**Step 6 — the item header (UI track).** `src/ui/feeds.rs` gains a detail block under the item
list: title / short date / episode / link. The short-date formatter is
`fn short_date(unix: i64, now: i64) -> String` — a civil-date conversion from unix seconds
(days-since-epoch → y/m/d, ~40 lines, the standard algorithm) plus a month table. Same-year →
`12 Aug 09:15`; other year → `12 Aug 2025`; `None` → `—`. Pure and unit-testable, and it keeps
`chrono` out of the direct tree exactly as `core/types.rs` intends.

**Step 7 — open link (UI track, tiny).** `enter` on a selected item →
`Action::FeedOpenLink` → `crate::core::openers::open_default(link)`. Guard the scheme: only
`http`/`https` are opened; `magnet:` routes to the existing download action instead. A spawn
failure banners the reason (FR-102 + `openers`' own error handling). ~30 lines.

## Files to create / modify

**Create**
- `src/rss/episode.rs` — `Episode`, `parse_episode`, `EpisodeFilter`, `parse_filter`.
  (This turns #59's `src/rss.rs` into `src/rss/mod.rs` + `src/rss/episode.rs`, which also keeps
  both files under the FR-67 size norms.)

**Modify**
- `SPEC.md` — FR-98…FR-102 in §4.9; `enter` in the keybind table.
- `src/core/feed.rs` — `Feed.refresh_secs`, `Feed.last_refresh`, `Rule.episode_filter`,
  `Rule.smart_filter`, `Rule.seen_episodes`. All `#[serde(default)]`.
- `src/rules.rs` — the episode predicate inside `matches()`; record the episode on add.
- `src/app/mod.rs` — the per-feed scheduler on the existing tick.
- `src/ui/feeds.rs` — the detail header, `short_date`, the two new editable rows.
- `src/ui/mod.rs` — `FeedsState.selected_item`.
- `src/input.rs` — `enter` → `Action::FeedOpenLink` on the feeds screen.
- `src/app/actions.rs` — the `FeedOpenLink` arm.
- `src/core/openers.rs` — **no change**; `open_default` already takes what is needed. If
  #60 lands before `docs/plans/open-finished-files.md`, that module is this plan's one
  prerequisite and should be lifted from that plan verbatim rather than re-invented.
- `tests/fixtures/rss/` — add an absolute-numbered anime feed and a `1x05`-style feed.

## Key APIs / libraries

- **No new crates.** Everything here is arithmetic and string scanning.
- **Rejected: `regex`.** Four patterns with explicit masking is ~200 deterministic, table-tested
  lines; a regex set is a new crate plus patterns nobody can review by eye. This also matches
  the existing precedent (`src/sources.rs`'s hand-rolled `urlencode`).
- **Rejected: `chrono`/`time` for the short date.** `core/types.rs:203-205` states the policy at
  the decision site. Civil-date-from-unix-seconds is ~40 lines and is exactly testable.
- **`crate::core::openers::open_default`** — decided in `docs/plans/open-finished-files.md`
  (checked 2026-08-16): `cmd /C start "" <target>` on Windows (`start` is a cmd builtin, so it
  cannot be spawned directly), `open` on macOS, `xdg-open` on Linux. All three accept a URL.
- **`quick-xml = "0.37"`** — introduced by #59; unchanged here.

## Risks / edge cases

- **The episode parser is the whole risk.** A false positive downloads the wrong thing; a false
  negative silently downloads nothing. Mitigations: the resolution/year masking in step 2, a
  large table test built from **real** fixture titles, and FR-99's rule that an unparseable title
  under an active filter is **skipped and counted** — the count is what makes a silently-doing-
  nothing filter visible on the feeds screen.
- **`S01E05-S02E03` spanning seasons.** Compare on the `(season, number)` tuple, not on the
  episode number alone, or the range collapses. Absolute-numbered items (season `None`) are
  compared only against absolute-form filters; mixing the two is rejected at `parse_filter`.
- **Multi-episode releases** (`S01E01-E03`, batch packs). v1 parses the *first* episode and
  notes the limitation. A batch under a smart filter would otherwise mark one episode seen and
  leave two silently missing — worse than skipping. Count them as "batch, skipped".
- **`v2` re-releases.** `- 12v2` parses as episode 12, so the smart filter refuses it. That is
  the correct default (it is the same episode) but must be *documented* in FR-100, because some
  users want the v2. A per-rule "allow re-releases" toggle is the natural follow-up, not v1.
- **A 60-second floor is a real constraint, not a suggestion.** Public feeds rate-limit. Enforce
  it at load *and* at edit; a hand-edited `feeds.json` with `refresh_secs = 5` must be clamped
  with a banner, not obeyed.
- **`last_refresh` must be unix seconds, not `Instant`.** `Instant` is not serializable and is
  meaningless across a restart. Storing it wrong means every restart refreshes every feed.
- **Opening a link is spawning a process.** Only `http`/`https`; never pass an arbitrary feed
  string to a shell. `openers` uses `Command` with an argv array, so there is no shell to inject
  into — keep it that way and do not "simplify" it to a shell string.

## Test strategy

- **Unit, `src/rss/episode.rs`** — a table test over real titles:
  `[SubsPlease] Frieren - 12 (1080p) [ABCD1234]` → `(None, 12)`;
  `Show.Name.S01E05.1080p.WEB-DL` → `(1, 5)`; `Show 1x05` → `(1, 5)`;
  `Movie.2160p.2019.x265` → `None` (year and resolution both masked);
  `Show.Name.S01E01-E03` → `(1, 1)` plus the batch flag. Plus filter ranges: in-range,
  out-of-range, open-ended, season-spanning, and a rejected malformed filter string.
- **Unit, `src/ui/feeds.rs`** — `short_date`: same-year, other-year, epoch 0, negative,
  `None` → `—`. This is the formatter that would otherwise quietly print `1 Jan 1970`.
- **Unit, `src/rules.rs`** — smart filter refuses a second release of the same episode; the
  seen set round-trips through `rules.json`; a rule with a filter and an unparseable title skips
  and increments the counter.
- **Unit, scheduler** — with a mocked clock, a feed at `refresh_secs = 900` is not refreshed at
  899s and is at 901s; a `refresh_secs = 5` is clamped to 60; at most one feed refreshes per
  tick.
- **Buffer snapshot, `src/ui/tests.rs`** — the item header with a date, without a date, with a
  parsed episode, and with `—`.
- **No network tests.** Every step here is pure or clock-driven; the fetch path is #59's.

## Verification

1. `SPEC.md` §4.9 contains FR-98…FR-102 and the keybind table lists `enter` on the feeds screen.
2. `cargo run` → `f` → add a real anime RSS feed (SubsPlease/Nyaa). Set the feed's interval to
   60s. **The "last refresh" column updates roughly once a minute without any keypress** — the
   observable proof the per-feed scheduler runs.
3. Set a rule's episode filter to `1-3` on that feed and enable it. Only episodes 1–3 are
   enqueued; the feed row shows a non-zero "skipped" count for the rest. Setting the filter to
   `S99E01-` enqueues nothing and the skipped count covers every item — a filter that matches
   nothing is *visible*, not silent.
4. Turn on the smart filter, let episode 4 download, then hand-edit `rules.json` to drop the
   infohash from the match history and refresh. **Episode 4 is still not re-added** — that is
   FR-100 working where FR-95 alone would not have.
5. Select an item and press `enter`. The system browser opens the article page. A magnet-only
   item downloads instead of opening — no browser window with a `magnet:` URL in it.
6. The header of a today item reads `12 Aug 09:15`; an item from last year reads `12 Aug 2025`;
   an item with no `pubDate` reads `—`. No `1 Jan 1970` anywhere.
7. `grep -rn "chrono\|regex" Cargo.toml` returns nothing — both were rejected on purpose.
