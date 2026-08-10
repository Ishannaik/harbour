# Reply from Ishan (UI track) — to Sarthak's notes-for-ishan + Dhruv's questions

> Formal answers to the decision checklist in [`notes-for-ishan.md`](notes-for-ishan.md)
> and the two items in [`notes-for-dhruv.md`](notes-for-dhruv.md) that are addressed
> to the UI track. Sent 2026-08-11.

## On Sarthak's decision checklist

### 1. Types move to phase 1, owned by you — agreed

The UI and sources tracks need a compiling contract *now*, so I created a working
`src/types.rs` implementing **your** approved contract (one `Paused` status +
`finished` flag, `peers: Option<u32>`, `eta: Option<Duration>`, speeds as `f64`
MiB/s — no second converter). It's marked as yours: when your 1A freeze lands, keep
yours and I'll adapt the views. The UI views compile against the working copy
already (search/downloads/status views are written).

### 2. Phase 1 splits 1A/1B — agreed, 1B is effectively shipped

Theme schema + loader + titanium, animation loop, DEC 2026 sync, easing primitives,
terminal lifecycle, and the over-the-top splash are all merged to `main` via PR #1.
The only 1B item still open on my side is the theme live-reload watcher (`notify`,
dep already added) — it's in the current UI slice.

### 3. `paused`: one status + a `finished` flag — agreed, implemented

`QueueItem { status, finished }`: a paused download (`finished == false`) and a
paused seed (`finished == true`) share `QueueStatus::Paused`, disambiguated by the
flag. Rendering: `paused` chips in `warning`; the Seeding tab only lists
`finished == true` items. Matches how a user thinks about the `p` key.

### 4. `peers`/`eta` render `—` when unknown — agreed, implemented

The downloads view renders `—` for `None`, never `0` — a paused seed shows `— peers`
and `— ETA`, indistinguishable-in-principle from a live-but-quiet seed, per your B1.
(Both look the same to librqbit; the `—` is honest about that.)

### 5. `NFR-03` → 100ms p95 + footprint NFRs — agreed

A 100ms-to-first-paint budget is a number we have to work for, and idle RSS /
binary-size NFRs give "lighter" teeth. Update SPEC in your 1A pass; I'll keep the
README honest (no "faster" claim without the numbers) until then.

### 6. Tidy batch — done

- `docs/context.md` committed; all `C:/tmp/harbour-context.md` citations fixed
  (SPEC, roadmap, architecture, design, sources, theming, AGENTS, README).
- Crate name standardized on **`harbour`** (Cargo.toml already had it; fixed
  AGENTS/roadmap/design).
- `HARBOUR_STATE_DIR` added to the AGENTS.md env-var list.
- **OQ-1**: agreed — `p` is pause-only; deletion is behind an explicit confirm
  (later slice). Please remove the "or removed per config" clause from FR-43 when
  you touch SPEC, or I'll wire a keybind to a behaviour we just decided against.

### 7. FR-25 grouped-with-duplicates — NOT intended; dedupe wins

Confirm: results merge into **one list, deduplicated by `info_hash`, keeping the
copy with more seeders** — torlink parity. Staggered source tags still make sense:
each result keeps its source tag, rows pop in as their source answers, and the list
re-sorts by seeders desc after dedupe. Dedupe happens at the aggregation layer
(UI-track wiring, app state), never in the scrapers. Lowercase-at-the-boundary
infohash is the join key — already right in docs/sources.md.

## On Dhruv's doc — the two items addressed to the UI

### §3(b): interactive search deadline — yes, ~3s, rest streams in

UI answer to "would a ~3s interactive deadline with late arrivals streaming in feel
better": **yes, and it's what the search view implements.** The bar releases after
3s (the query stays), the header reads `searching… N results from M sources` and
keeps updating as late sources land; a source that hasn't answered by the deadline
shows a *pending* dot (muted), not offline. Sources keep their 10s per-request
ceiling (their contract) — the UI just stops holding the bar at 3s. A late source
that answers slots its results in and flips its dot online. No user-visible wait.

### §5: dedupe — same answer as #7 above.

## Open item for the freeze (not blocking)

**Lazy ReelIndex magnets** (Dhruv's doc §1): if the freeze gives `magnet: Option<String>`
plus on-demand resolution, the UI shows a `resolve…` affordance on `d` when the
magnet is `None`. Recommend taking Sarthak's suggestion — it's the single biggest
latency lever in the product, and it's cheaper in the contract than in the scraper.
