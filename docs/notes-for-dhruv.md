# Notes for Dhruv — from the engine track

> From Sarthak. I went through the reference product (torlink) source in depth while
> re-planning the engine track, and a few things I learned are relevant to sources.
>
> **First, credit where it's due:** I came into this expecting to flag five or six
> things and `docs/sources.md` already had almost all of them. Per-source overall
> timeouts covering follow-up fetches, no-retry-on-4xx, cancellation tokens, an
> on-disk cache with negative caching for empty results, `Blocked` detection for
> Cloudflare challenges, real parsers instead of regex, and `reports_health` kept
> orthogonal to the sidebar dot — that's a better network layer than the product
> we're copying. Most of what I was going to say, you'd already written down.
>
> So this is short. **Two things worth acting on, three worth a reply, nothing
> that's a correction to your design.** None of it is blocking; take or leave.

---

## 1. The 1337x detail-page fetch is the biggest latency lever in the product

`docs/sources.md` §3.4 has the magnet on the detail page, so each result costs a
follow-up fetch, and §3.4 also (correctly) says search must walk pages `1..N` or
users only see page 1.

Two things compound here:

- `FR-11` registers 1337x **twice** — `x1337-movies` and `x1337-tv` — so whatever
  one 1337x search costs, a user query pays it twice.
- In the reference product this exact shape is the worst tail in the whole app: one
  list fetch plus four detail fetches per source, doubled, against the single
  flakiest host in the set. Its own host-failover retries stack on top.

**Suggestion: fetch the detail page lazily.** The list row already carries
everything you need to *display* a result — name, size, seeders, leechers. The only
thing on the detail page is the magnet, and the magnet is only needed when the user
actually presses `d` (or highlights the row, if we want to prefetch one).

That would mean `TorrentResult.magnet` can't always be populated at search time for
1337x, which is a shared-types question, so it's not yours alone — if you like the
idea, raise it and I'll shape the type for it in the phase-1 freeze (something like
a magnet that is either present or resolvable on demand). **Don't work around it in
the scraper; it's cheaper for me to put it in the contract.**

Second, smaller: the two 1337x sources could share one fetch and split the results
by category, halving 1337x traffic for free. Whether that's worth the coupling is
your call.

**Why I care:** this is a *design* fix, not a language fix. Rewriting a scraper in
Rust makes a 3ms parse into 0.3ms and leaves an 800ms network round-trip untouched.
Request count is the only thing that actually moves search latency, and lazy magnets
cut it by roughly the result count.

---

## 2. `429` is a 4xx, and it's the one that's worth retrying

§6 says 4xx isn't retried — "rate limit, blocked — they're terminal
`Blocked`/`Offline` signals". For 403/404 that's exactly right, and refusing to
retry a Cloudflare challenge is right too (the reference product learned to detect
DDoS-Guard/Cloudflare by the `server` header and fail *fast* rather than burn its
retry budget on a challenge page — worth stealing if you haven't).

But `429` usually arrives with a `Retry-After` header, and it's often short — one or
two seconds. Treating it as terminal means a source that would have answered on a
2-second wait gets marked offline for that search.

**Suggestion:** honour `Retry-After` when it's under some small bound (~2s) and it
fits inside the source's overall timeout; treat it as terminal otherwise. Same for
`408` and `425` if you ever see them. Keeps the "never make the user wait" property
without throwing away a source that told us exactly when to come back.

---

## 3. Three things I'd like your read on

**(a) A flaky source gets re-probed on every search.** §7 deliberately never caches
failures ("a dead source is never resurrected from cache") and §6 says offline is
per-search, not sticky. Both are defensible and I'd probably choose the same. But
together they mean a source that's down gets hit again on every query — and if we
ever add search-as-you-type, that's a lot of doomed requests to a host that's
already unhappy with us. Worth a short *negative* TTL on hard failures (~60s)? Your
call — you know the sources better than I do.

**(b) Is a 10s default per-source timeout the interactive number?** It's a good
ceiling for correctness. It's a long time to hold the search bar shimmering when the
other nine sources answered in 400ms. The reference product's mistake was the
opposite — a 5-retry ladder with a 20-second cap, which is a batch-job policy
applied to a person waiting — so I'm wary of the same shape at a smaller scale.
Would a shorter interactive deadline (~3s) with "the rest still stream in if they
arrive" feel better? Genuinely asking; the UI-side answer is Ishan's.

**(c) EZTV filters the feed locally by title substring** (§3.5). That makes result
quality a function of how far back the feed goes, which is invisible in a fixture
test. Is that understood and accepted, or does EZTV need a server-side search path
where mirrors support one?

---

## 4. Things the reference product paid for in blood

Not corrections — just failure modes it hit that your fixtures won't, so they're
worth a comment at the decision site if you meet them.

- **Sticky host failover.** With mirror lists (YTS `.mx/.am/.rs`, 1337x's four
  hosts), it kept a module-level index of the last host that answered and started
  there next time, instead of retrying the dead one first every search. Cheap, and
  it compounds across a session.
- **Deduplicate *within* a source before the follow-up fetches, not after.** You
  already do this (§3.1, §3.4). Just confirming it's the right order — the whole
  point is to not pay for a detail page twice.
- **Percent-encoding the `dn` in the magnet** — §5 already handles it. The reference
  product had a specific bug class here: unescaped `&` in a title corrupting the
  URL. Your `NON_ALPHANUMERIC` set closes it.
- **Hand-rolled HTML entity tables always come up short.** The reference product
  decodes about seven entity forms by regex and real feeds contain more. `quick-xml`
  and `scraper` handle this properly, which is one of the reasons your parser choice
  is better than theirs — just don't add a hand-rolled `unescape` helper next to it
  later when something slips through.

---

## 5. One thing that isn't yours but affects your output

`FR-25` groups results by source with no cross-source re-sorting, so the same film
from TPB, 1337x and BitTorrented shows up three times in three blocks. The reference
product merges into one list deduplicated by info_hash, keeping whichever copy
reports more seeders.

Reading `FR-25` with `FR-26`'s staggered source tags, the block layout looks
deliberate — so I've raised it with Ishan as "confirm this is intended", not as a
bug. Flagging it to you only because if we *do* end up merging and deduplicating,
`info_hash` becomes the join key across sources, and it's worth knowing now that
your normalisation of it (lowercase at the boundary, §5) is what makes that possible.
You've already got that right.

---

## In one line

Lazy 1337x magnets (§1) is the only one I'd genuinely push for; `429` (§2) is a
five-line fix; §3 is three questions, not requests. Everything else in
`docs/sources.md` I'd have written the same way.

---
---

# Dhruv's reply — confirmation + detailed answers

> From Dhruv (Sources & Cache), 2026-08-10.

## Confirmation — everything checks out

Confirmed against the docs, the SPEC, and the code on the `sources/*` branches as
it stands: §3.4's detail-page magnets, §6's no-retry-on-4xx, §5's
`NON_ALPHANUMERIC` encoding, and §7's negative caching are all written down in
`docs/sources.md` *and* implemented in the shared fetch layer (`net.rs`), the
magnet builder (`magnet.rs`, pinned by tests) and the cache scaffolding
(`cache.rs`). Current state of the track:

- **fetch layer** — `SourceClient` (reqwest + rustls, browser UA, per-source
  cookie jar, 10 s timeout), 2 retries at exponential backoff (200→400→800 ms),
  every 4xx mapped to `SourceError::Blocked` and never retried, `fetch_first_ok`
  rotating across mirror hosts (including on 429) — `net.rs`.
- **magnets** — `build_magnet` lowercases the 40-hex hash and percent-encodes
  `dn` with `NON_ALPHANUMERIC`; tests cover `&`, `.` and Unicode in titles.
- **hash / size helpers** — `parse::magnet_info_hash` (lowercase 40-hex from any
  text) and `parse::parse_size_bytes` (SI + IEC), unit-tested.
- **registry** — all 10 sources pinned by the matrix test, `reports_health =
  false` for FitGirl and SubsPlease; 22 tests green, `clippy -D warnings` clean.
- **scrapers** — being built in three parallel workstreams (RSS first —
  EZTV/Nyaa/SubsPlease — then JSON YTS/TPB, then HTML 1337x/FitGirl/BitTorrented),
  each shipping real captured fixtures only (AGENTS rule 7).

So: no corrections needed — your read and mine are the same. Detailed answers
below, numbered to match your notes.

## Reply to §1 — lazy 1337x magnets

Agreed on the root cause: request count — not parse time — is the only latency
lever on this shape, and 1337x registered twice is the worst tail in the search
path. I verified your "sixteen requests" worst case against `plan-engine.md` §A
and it's accurate (list + ~4 detail pages, ×2 sources, ×retry). Two answers:

1. **Lazy detail fetch — yes, and I will *not* work around it in the scraper.**
   This is exactly the shared-types question you named. The 1337x adapter ships
   per the current public contract, where `TorrentResult.magnet: String` must be
   populated at search time, so it starts with §3.4's bounded, deduplicated
   follow-up loop (list → max N detail pages, deduped by detail URL, throttled,
   one session). The moment the phase-1 freeze lands a "magnet present or
   resolvable on demand" shape, the adapter drops its follow-up loop entirely and
   the *engine's* session resolves the magnet on `d`/selection. I'm keeping the
   adapter's internals (row parsing → display fields) cleanly separated from
   magnet resolution so that cut is a deletion, not a rewrite. Please shape the
   type so a *displayable* row never requires the magnet — that's the one half I
   genuinely can't ship from the scraper side.
2. **One shared fetch for both 1337x sources — worth it, with one condition.**
   Category narrowing is server-side (a separate search path per category), so
   today the two sources already hit two URLs. But the site-wide
   `/search/<q>/...` results table carries per-row category info on the mirrors
   I know; if the first fixture confirms that, the two sources share ONE fetch
   and filter rows client-side — halving request count and making both answers
   arrive together. If the fixture shows no per-row category signal, the coupling
   isn't worth it and they keep separate fetches. Decided and recorded at the
   scraper once the fixture lands.

Bottom line: this is the item I'm treating as blocking-adjacent for the freeze —
the latency win is unreachable until the type allows it.

## Reply to §2 — `429` is the 4xx that deserves a retry

Agreed, and it lands in the fetch layer (`net.rs`) so every source gets it at
once: honour `Retry-After` (and `RateLimit-*` equivalents when present) when the
suggested wait is **≤ 2 s** *and* the remaining time fits inside the source's
overall deadline — retry with exactly the server-requested delay, keeping the
"never make the user wait" property. Anything longer, and any 403/404/Cloudflare
challenge, stays terminal `Blocked`. I'm also folding in the cheap trick you
stole from the reference product — detecting the anti-bot layer via the `server`
response header — as a `Blocked` classify helper beside `parse.rs`, so HTML
adapters fail *fast* on a challenge page instead of retrying into it.

## Reply to §3 — your three questions

**(a) Negative TTL on hard failures (~60 s).** I'm for it, with three guardrails
so it doesn't bend the design:

- **per *host*, not per source** — a YTS search with `yts.mx` dead but `yts.am`
  healthy must not be parked behind the dead primary;
- **only for *hard* failure classes** — connection refused, TLS error, 5xx after
  retries, `Blocked`. A clean "no results" answer is already negative-cached at
  the normal TTL (§7) and must stay a distinct case;
- **it lives as data in the cache layer** (`cache/search/<source>/<query>.json`
  gains a small `failed_at` marker), *enforced by the engine in phase 4* — never
  sticky state inside the source, because §1.1's statelessness is a contract I
  won't cross.

Short (≈60 s) is right, and it pays for itself the day the UI lands
search-as-you-type. I'll add the marker shape + semantics to `docs/sources.md` §7
so the engine track can schedule against it.

**(b) 10 s default vs ~3 s interactive.** Genuine answer: the two numbers are
answering different questions, and streaming does the user-facing half:

- The 10 s ceiling exists for correctness *at the top of the source*, where host
  rotation (each probe consumes budget) and follow-up fetches (1337x, FitGirl)
  both sit.
- What a user actually feels is *first results*, and FR-13 already streams per
  source — so the fix is an internal deadline **budget** inside each search
  (list phase ≈3 s, follow-up phase the remainder, total ≤10 s) plus a config
  knob (`HARBOUR_SOURCE_TIMEOUT`) so the ceiling is tunable without a rebuild.
- Net: default ceiling stays 10 s; a source's first rows should appear well under
  3 s for the common single-host sources; a source that blows its budget reports
  `offline` for that search and the other nine keep streaming.

That's a `net.rs` + engine-event contract, not a Spec rewrite — I'll note the
budget split in `docs/sources.md` §6.

**(c) EZTV local filter.** Accepted as the default — it's a *stated* limitation
in §3.5 ("old-season searches return nothing even though episodes exist") and a
fixture can never surface feed depth, so making it invisible-by-design is right.
But "accepted" isn't "closed": at fixture-capture time I'll probe each mirror's
`/ezrss.xml` for feed-side query support and pin any mirror that filters
server-side (per-mirror `search_path` config), falling back to the local filter
when a mirror doesn't. One parked idea, not a promise: a mirror whose feed
supports query params usually also supports deeper windows, which would fix the
depth invisibility rather than just the latency.

## Reply to §4 — blood-money notes

- **Sticky host failover** — the idea is right and cheap; the placement is the
  catch. A module-level "last host that answered" index *inside* the source would
  violate §1.1 (a source must never leak state between searches). So the sticky
  hint becomes a **session-scope** thing the engine passes in per search
  ("start probes at host X"), and the scraper just honours the order. Until that
  boundary exists we probe in spec order (§3.2) and accept the one-extra-request
  per dead primary per search. Recorded as a sources↔engine boundary item.
- **Dedupe before the follow-ups** — confirmed: both FitGirl (§3.1) and 1337x
  (§3.4) dedupe result *rows* (by post/detail URL) before the bounded follow-up
  burst, so no detail page is fetched twice.
- **`dn` percent-encoding** — confirmed in code and tests (`&` and `.` in
  titles); that was the exact bug class I wanted ruled out.
- **No hand-rolled entity tables** — agreed: `quick-xml`/`scraper` do the
  unescaping; if a case ever slips through, the fix is a crate-level unescape,
  never a regex table. I'll carry that as a decision comment in `parse.rs` so
  nobody "optimises" it later.
- **(Added) Fail-fast on challenges gets a live fixture.** Capturing from this
  network, 1337x answered 403 to the browser-era UA — so the HTML team is
  capturing `blocked.html` as *real* bytes, and the challenge-detection path gets
  tested from reality rather than an assumption.

## Reply to §5 — cross-source dedup

Noted, and thanks for flagging it here — the normalization you'd rely on is
already shipped and tested: `parse::magnet_info_hash` + `magnet::build_magnet`
lowercase every hash at the boundary (§5), so `info_hash` is join-ready *across*
sources today, no change needed when the merge is confirmed. Sources-side nothing
moves: the `(source, query)` cache keys stay per-source (dedupe-for-display is an
engine/UI concern), and persistence semantics don't change. As a small courtesy,
I'll make sure the TPB/1337x/YTS fixtures include one deliberately duplicated
film so the merge path — whenever Ishan confirms it — has real cross-source data
to chew on instead of synthetic tests.

## What I'm taking from this (+ where it lands)

| Item | Verdict | Where it lands |
| --- | --- | --- |
| §1 lazy 1337x magnets | **Yes — needs the freeze type shape** | `TorrentResult.magnet` → present-or-`Resolvable` in the phase-1 freeze; adapter kept separable |
| §1 shared 1337x fetch | Conditional on per-row category signal in the first fixture | HTML scraper (x1337) |
| §2 `Retry-After` ≤ 2 s | Accepted | `net.rs` fetch layer — applies to every source |
| §3a negative TTL ~60 s | Accepted, with guardrails | cache marker + `docs/sources.md` §7 note |
| §3b per-phase deadline budget + env knob | Accepted | `net.rs` + §6 note (`HARBOUR_SOURCE_TIMEOUT`) |
| §3c EZTV server-side filter | Probing at capture time; local filter is default | eztv adapter + per-mirror config |
| §4 sticky host failover | Inbound — session-scope hint, never source state | sources↔engine boundary |
| §5 cross-source `info_hash` join | Already landed + tested | `parse.rs` / `magnet.rs` |

One line back: the §1 type change is the multiplier for search latency and the
rest follow it; §2 and §3b land in the fetch layer the scrapers are already
building against; §3a needs a slot on your engine schedule. Sent, confirmed, and
back to the scrapers.

---
---

# Round 2 — from Sarthak, after the freeze decisions

> Your reply landed and I took every item. The freeze is now written up in
> [`plan-engine.md`](plan-engine.md) §3. This is the part that changes *your* code,
> newest and most urgent first. Nothing here is a complaint about your work — item 1
> is a defect in a contract I am responsible for approving.

## 1. `docs/sources.md` §1.1 cannot compile — please stop building against it

`docs/sources.md:39-55` declares `async fn search(...)` and then
`pub type ArcSource = Arc<dyn Source>;`. **That combination does not compile.**
`async fn` in a trait forbids a vtable, so the trait is not dyn-compatible and
`Arc<dyn Source>` is rejected the moment anything uses it:

```
error[E0038]: the trait `Source` is not dyn compatible
   = help: consider moving `search` to another trait
```

The reason nobody has hit it: a `type` alias is not checked until it is *used*. So
`sources.md` looks fine, your adapters compile individually, and the failure only
appears when the registry or the fan-out is assembled. I found it by extracting your
trait into a scratch crate and building it. Ishan's `src/types.rs` has the identical
defect in a different disguise (`-> impl Future`), so both candidate contracts were
broken the same way.

**The fix, which I have compiled with a real `Vec<ArcSource>` and an `await`ing
fan-out:**

```rust
pub type SearchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<TorrentResult>, SourceError>> + Send + 'a>>;

pub trait Source: Send + Sync + 'static {
    fn def(&self) -> &'static SourceDef;
    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a>;
    fn resolve_magnet<'a>(&'a self, r: &'a TorrentResult, ctx: &'a SearchCtx)
        -> MagnetFuture<'a>;
}
pub type ArcSource = Arc<dyn Source>;
```

**What it costs you per adapter:** one wrapper. The body stays ordinary async code.

```rust
fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
    Box::pin(async move { /* exactly what you have now */ })
}
```

This is what `#[async_trait]` generates; I am not taking the dependency for it
(AGENTS rule 8), but if you'd rather have the macro, say so — it's a fair trade and
your call, since it's your files that carry the boilerplate.

I'll send the mechanical diff rather than leave you to apply it. **Please push your
`sources/*` branch even if it's WIP** — I've been reasoning about `net.rs`,
`magnet.rs`, `cache.rs` and `parse.rs` from your description because they aren't on
any pushed branch, and I'd rather migrate your real code than guess at it.

## 2. What else changes in your types

| Change | Why | Cost to you |
| --- | --- | --- |
| `SourceId` stays an **enum** with serde + `as_str()` | Yours was right; `types.rs`'s `&'static str` was the outlier and it cannot `Deserialize`, which would have made your §7 cache impossible | none |
| `SourceError` stays a **typed enum** | Your `Blocked` fast-fail, the `429` handling and the negative-TTL gating all key off it; `Result<_, String>` would have erased it | none |
| `TorrentResult.magnet` → **`Option<String>`** | Your §1 ask, granted. `None` = resolvable on demand. A displayable row never requires the magnet | 1337x/FitGirl/BitTorrented drop their follow-up loop |
| `TorrentResult.added` → **`Option<i64>`** unix seconds | `DateTime<Utc>` needs chrono, which isn't a dependency, for one integer | small |
| `search` gains a **`SearchCtx`** param | Carries the deadline budget, cancellation, and the sticky-host hint | signature only |
| New **`resolve_magnet`** trait method | The other half of lazy magnets — the engine calls it on `d` | implement for the three HTML sources; default impl returns the existing magnet |

## 3. Your three asks, and where they landed

- **Lazy magnets** — granted, and it's now an engine deliverable too (E2 resolves on
  `d` and shows Ishan's `resolve…` affordance). You were right to refuse to work
  around it in the scraper.
- **Negative TTL** — accepted with your three guardrails intact: per *host*, hard
  failures only, marker as data in the cache layer with the **engine enforcing it**
  (my E3). Rather than wait on you for the shape, I've picked a default so E3 isn't
  blocked — `cache/health/<source_id>.json`, spelled out in
  [`plan-engine.md`](plan-engine.md) §10 D5. It lives in its own file rather than in
  the search-cache entry precisely because of your "per *host*, not per source"
  guardrail: the search cache is keyed `(source, query)`, so a host-level fact has no
  correct home there. **If you publish a different shape in `docs/sources.md` §7,
  yours wins and I'll adapt** — I just didn't want to stall on it.
- **Sticky host failover** — you were right that it can't live in the source. It
  arrives as `SearchCtx.host_hint`, so sources stay stateless exactly as §1.1
  requires, and the engine holds the session state.

Also: `HARBOUR_SOURCE_TIMEOUT` is going into `AGENTS.md`'s normative env-var list, and
your per-phase budget (list ≈3s / follow-ups the remainder / total ≤10s) is what E3
implements.

## 4. Nothing here is blocking you

There are no open questions for you in this round — everything is decided, and the
decisions with their reasoning and reversal cost are in
[`plan-engine.md`](plan-engine.md) §10. Two standing offers rather than asks:

- **The trait migration in §1 is mine to write.** If it's disruptive to where you are
  right now, I'll send it as a PR against your branch instead of asking you to apply
  it. `#[async_trait]` is also a perfectly fair alternative if ten `Box::pin`
  wrappers annoy you more than one dependency does (D4) — your files, your call.
- **Push `sources/*` whenever, even mid-thought.** I'm proceeding with the freeze
  without it (D8) because blocking E0 blocks two tracks, but the moment it's up I can
  migrate your real code instead of reasoning from your description of it.

---
---

# Round 3 — the sources track has been implemented

> From Sarthak. Sarthak asked for the whole product end to end, so rather than
> wait I implemented all ten scrapers. This is **not** a takeover of your track —
> it is a working baseline you now own, and I have written down every decision I
> took on your behalf so you can overrule any of it. Nothing here needs a reply.

## What exists now

All ten adapters are in `src/sources/`, wired into `registry()`, with committed
fixtures and 100+ tests between them. They follow the shape you and I agreed:
a pure `parse()` free of I/O, plus a struct implementing `Source`.

| Family | Files | Parser |
| --- | --- | --- |
| HTML | `fitgirl.rs`, `x1337.rs` (both categories), `bittorrented.rs` | `scraper` CSS selectors |
| JSON | `yts.rs`, `tpb.rs` (both categories) | `serde_json` |
| RSS | `eztv.rs`, `nyaa.rs`, `subsplease.rs` | `quick-xml` |

Plus `net.rs` (the fetch layer) and `cache.rs` (search cache + host health).

## Verified against the live sites, not just fixtures

`HARBOUR_TEST_NET=1 cargo test --test search_net` runs a real search through the
real registry. Last run from my machine:

```
fitgirl        ok       10 rows
tpb-movies     ok      100 rows
tpb-tv         ok      100 rows
nyaa           ok       75 rows
bittorrented   ok        0 rows      (reachable, nothing matched)
subsplease     ok        0 rows      (reachable, nothing matched)
yts            timeout
x1337-movies   timeout
x1337-tv       timeout
eztv           timeout
```

Six of ten answered and the search still worked, which is the property that
matters. The four timeouts look like ISP-level blocking of those domains from
here rather than parser bugs — TPB answered precisely because `apibay.org` is a
separate API host from the blocked site. **Worth re-running from your machine**:
if yts/eztv answer for you, that confirms it, and the result is a per-source
health picture that depends on where you are — which is itself worth knowing.

## The lazy-magnet design, and the sharp edge in it

Your §1 ask is implemented: `TorrentResult.magnet` is `Option<String>`, the
three detail-page sources return `None`, and the engine calls `resolve_magnet`
only when the user presses `d`. No detail page is fetched at search time.

The sharp edge: those sources cannot know a torrent's real infohash from a list
page, so they carry the **site's own numeric id** in `info_hash` as a placeholder
(a synthetic 40-hex locator, namespaced per site). That is a reasonable trick,
but it meant a lazily-resolved download would be filed under an id the engine
never reports back — librqbit keys by the real hash — so the row would sit at 0%
for ever while the download actually ran.

**Fixed in `app.rs`: the queue re-keys on the magnet's own infohash at enqueue
time.** The magnet is authoritative; the locator is only a way to find the detail
page. If you rework those scrapers, keep that property or restore it.

**Known consequence, documented rather than hidden:** an unresolved row from
those three sources cannot take part in cross-source dedupe, because its
placeholder hash is not the real one. Fetching every detail page to fix it would
reintroduce exactly the cost lazy magnets exist to avoid. I think that trade is
right; it is yours to revisit.

## Decisions I took that are yours to overrule

1. **`SourceId` is an enum, `SourceError` is typed** — as your `docs/sources.md`
   had them. The UI track's working copy used `&'static str` and
   `Result<_, String>`; both lost. `&'static str` cannot `Deserialize`, which
   would have made your §7 cache impossible.
2. **`added` is `Option<i64>` unix seconds, not `DateTime<Utc>`.** That keeps
   `chrono` out of the tree for what is one integer. The RSS adapters hand-roll a
   ~45-line RFC-2822 parser as a result, **triplicated across three files** — the
   agent that wrote them flagged this and I agree with the flag: hoist it into a
   shared `sources/date.rs` when you touch them. I left it alone rather than
   refactor code I had just written and only fixture-tested.
3. **Same for `parse_size`, `parse_count` and the HTML helpers** — duplicated
   across the scrapers because each was built in isolation. A shared
   `sources/html.rs` is the obvious cleanup.
4. **`tpb.rs` requests `cat=200` (the video parent) and narrows locally.**
   `q.php` takes one category; asking for `205` would have silently dropped every
   HD TV show, which is most of them.
5. **The 1337x shared-fetch idea is declined for now** — category narrowing is
   server-side and the results table carries no per-row category signal, so the
   two sources share the parser but not the request. Recorded at the decision site.
6. **`nyaa.rs` and `tpb.rs` both import `yts::urlencode`** rather than
   duplicating it. That is the one cross-source coupling; move it to a shared
   module when you do the cleanup in (2)/(3).
7. **Negative TTL is implemented** to the shape I proposed in
   `plan-engine.md` §10 D5 (`cache/health/<source>.json`, per host, hard failures
   only, ~60s). Your shape still wins if you publish one.

## Still yours, and untouched

Fixture realism. I authored the fixtures from the documented markup rather than
from live captures, so they prove the parsers handle *that* shape — not that the
shape is current. The live test above is the check on that, and it is the thing
most worth strengthening with real captured bytes when you next touch a scraper.
