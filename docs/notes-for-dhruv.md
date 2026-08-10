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
