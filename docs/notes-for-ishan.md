# Notes for Ishan — from the engine track

> From Sarthak. This is the short version of [`plan-engine.md`](plan-engine.md) —
> only the parts that need **your** decision or touch **your** track. You should not
> have to read the full plan to action this.
>
> Nothing here is a change I have made to your work. Everything is either a request
> for sign-off on a shared normative document, or a contract detail you need before
> Phase 2 renders anything.

---

## A. Three things that block me starting

These are the ones I'd like resolved first, in a single pass, because they all touch
the same two documents.

### A1. Who defines the shared types, and in which phase?

`AGENTS.md:33` says the shared-types freeze is mine and **lands in phase 1**.
`docs/roadmap.md:57` defines those same types as **the first task of Phase 2** —
your phase — and `roadmap.md:123` then makes my Phase 4 depend on Phase 2.

So as written, I wait on your phase to define my engine's own event enum, and two
documents disagree about who owns the contract that `AGENTS.md` rule 4 exists to
protect.

**Ask:** move the types task to phase 1, owned by me. If you agree, three edits have
to land **together** or we end up with two owners of a "frozen" contract:

- delete the types task at `roadmap.md:57`
- drop "shared types are stable" from the Phase 2 DoD at `roadmap.md:70`
- fix "which phase 2 pins down first" at `roadmap.md:247`

### A2. Can we split Phase 1?

`roadmap.md:242` says "**Phase 1 is serial** — everything hangs off it. One
workstream, do not split." But seven of its eight tasks are your track's work by
`AGENTS.md:10`'s own scope definition — theme schema, theme loader, live reload,
titanium, the 30fps loop, DEC 2026 sync, easing primitives.

What the engine actually needs out of Phase 1 is narrow: a compiling crate, the
tokio runtime, CLI parsing, config-dir resolution, an error type, and the
*signatures* of the crash-log and bootguard hooks.

**Ask:** split it.

| | **Phase 1A — Foundation** (me, ~2 days) | **Phase 1B — Terminal & Theme** (you) |
| --- | --- | --- |
| crate/module layout, shared types freeze, paths + config dir, error type, CLI parse, lifecycle hook signatures, CI matrix | theme schema, loader, live reload, titanium, animation loop, DEC 2026 sync, terminal lifecycle impl, easing |

"Do not split" then applies to 1B, which is where it was really aimed. Your Phase 2
and Dhruv's Phase 3 both start after 1A — two days, not a whole UI phase.

### A3. `paused` is missing from the status vocabulary

`AGENTS.md:48` lists five statuses: `queued`, `downloading`, `failed`, `seeding`,
`missing`. But `FR-43` (`p` pauses a seed), `FR-47` (bootguard restores seeds
paused) and `FR-53` (safe mode pauses everything) all need a sixth. librqbit has
`Paused` as a first-class state natively, so this isn't us inventing one.

Two follow-ons, both easy to miss:

- `FR-30`'s transition chain (`SPEC.md:147`) and `FR-48`'s ledger enum
  (`SPEC.md:195`) also list the five. If only `AGENTS.md` changes, the ledger
  legally cannot persist `paused`.
- **A question for you, because you render it:** is a paused *download* the same
  status as a paused *seed*, or two? I'd keep one status and disambiguate by whether
  the item ever reached `finished` — it keeps the enum small and matches how a user
  thinks about the `p` key. But it puts a `finished` flag on `QueueItem`, which lands
  in your rendering, so it's your call.

---

## B. Contract details you need before Phase 2 renders downloads

I verified these against `librqbit 8.1.1` on this machine (evidence:
[`engine-spike-librqbit.md`](engine-spike-librqbit.md)). They change what the frozen
types can promise you.

### B1. `peers` is sometimes genuinely unknown — please don't render it as `0`

`FR-32` and `FR-44` both show peers. But librqbit's `TorrentStats` has **no peer
field**: peers live at `stats.live?.snapshot.peer_stats`, and `live` is `None`
whenever a torrent is paused, initializing, or errored.

If I hand you `peers: u32`, a paused seed renders "0 peers" and you cannot tell it
apart from a live seed nobody is connected to. So the frozen type will say
`peers: Option<u32>`.

**Ask:** render the `None` case as `—`, not `0`. I've proposed a clause on FR-32 and
FR-44 saying exactly that (`plan-engine.md` A-5), but you own how it looks.

### B2. ETA comes from the engine — you don't need to compute it

`LiveStats.time_remaining` is provided. The frozen type will carry
`eta: Option<Duration>`. Same `None` rule as peers.

Related: engine speeds are `MiB/s` as `f64`, not bytes/sec. I'll convert once in the
engine adapter and hand you one documented unit, so it never gets done twice or not
at all — just flagging it so you don't build a second converter.

### B3. The 500ms poll will not be flat

`FR-32`/`FR-44` fix a 500ms stats poll. I'm proposing it stays 500ms while anything
is actively downloading and drops to ~5s when everything is a settled seed, with
completion driven by librqbit's awaitable `wait_until_completed()` rather than
discovered on a tick.

Why it matters to you: nothing about the user-visible guarantee changes. On a
seedbox with 200 idle seeds the flat version is 400 stat reads a second to learn
nothing, against `NFR-04`'s ≤2% idle CPU budget.

### B4. Restored seeds pass through an `Initializing` state

After a restart a **complete** seed is `Initializing` with `finished == true` before
it goes `Live`. My status projection maps that to `seeding (verifying)`, not
`downloading` — otherwise every restored seed shows as an active download the moment
the app opens, which would look like a bug and would contradict `FR-47`. Mentioning
it because it's a visible state you may want to style.

---

## C. Two spec numbers I think are wrong

### C1. `NFR-03` sets startup at ≤500ms

That's a budget a Node TUI can plausibly hit — and the reference product *is* a Node
TUI. If "lighter and faster" is why harbour exists, we could ship, pass `NFR-03`,
and be no quicker to start than the thing we're replacing, with nothing in the spec
catching it.

**Ask:** tighten to ≤100ms p95 to first paint (excluding first-run config creation).
I'd rather we set a number we have to work for. If you'd rather not commit to it,
that's a fair call — but then we should stop putting "faster" in the README.

### C2. There is no memory or binary-size requirement at all

Those are the two places "lighter" is actually observable to a user. I've proposed
NFR-13 (idle RSS with 20 seeds) and NFR-14 (release binary size), with the actual
numbers filled in from my E1 measurements rather than guessed now.

---

## D. Small stuff, batch it

- **Crate name.** `harbour-tui` in `AGENTS.md:51`, `roadmap.md:32` and
  `design.md:3`; `harbour` in `SPEC.md:3` and `Cargo.toml:2`. `Cargo.toml` is the
  one that compiles, so I'd standardise on `harbour` and fix the other three.
- **`docs/architecture.md:7`** names `C:/tmp/harbour-context.md` as "the single
  source of truth". That path doesn't exist on my machine and can't exist on
  Dhruv's. `roadmap.md:5` and `SPEC.md:8` both cite it too. Can we commit it as
  `docs/context.md`?
- **OQ-1 is answerable now.** librqbit has `pause`/`unpause` *and*
  `delete(id, delete_files)`, so both behaviours are cheap. I'd suggest `p` =
  pause-only, deletion behind an explicit confirm. If you agree, `FR-43`'s "or the
  item is removed per config" clause has to go at the same time, or you'll wire a
  keybind to a behaviour we just decided against.
- **`HARBOUR_STATE_DIR`.** I'm adding one env var in 1A that relocates all state to
  a temp dir, so all three of us can test persistence without touching real user
  data. Needs adding to the normative env-var list at `AGENTS.md:49`.
- **CI is now a three-OS matrix** on my branch. `NFR-08` makes Windows Terminal the
  primary target and `FR-06`/`FR-55` are path and atomic-rename requirements, which
  is exactly what breaks per-OS. It'll make your PRs run on three runners instead of
  one — worth it, but you should know it's coming.

---

## E. One design question, not a defect

`FR-25` groups results by source with no cross-source re-sorting, so the same film
from TPB, 1337x and BitTorrented appears three times in three blocks. The reference
product instead merges everything into one list, deduplicated by info_hash, keeping
whichever copy reports more seeders.

Reading `FR-25` and `FR-26` together, the block layout looks deliberate — it's what
makes the staggered source tags mean anything. **So this is a "confirm, don't fix"**:
if grouped-with-duplicates is the intended UX, nothing needs to change and I'll stop
raising it. If duplicates across sources weren't considered, it's much cheaper to
decide now than after the results list is built.

---

## Decision checklist

| # | Decision | Blocks |
| --- | --- | --- |
| 1 | Types move to phase 1, with the three atomic roadmap edits (A1) | My E0 |
| 2 | Phase 1 splits 1A / 1B (A2) | My E0, your 1B, Dhruv's Phase 3 |
| 3 | `paused` added to all three status lists; one status or two (A3) | The types freeze |
| 4 | `peers`/`eta` render as `—` when unknown (B1, B2) | Your Phase 2 |
| 5 | `NFR-03` → 100ms, and add the two footprint NFRs (C1, C2) | Nothing — but decide before we claim it |
| 6 | Crate name, context file, OQ-1, `HARBOUR_STATE_DIR` (D) | Nothing, just tidy |
| 7 | `FR-25` grouped-with-duplicates: intended? (E) | Your results list |

---
---

# Round 2 — from Sarthak, after the freeze decisions

> Thanks for the reply — you agreed to everything and unblocked both tracks by
> writing a working `types.rs`, which was the right call. The freeze is now written
> up in [`plan-engine.md`](plan-engine.md) §3. This is what changes for the UI, plus
> one genuine open question and two places where I'm departing from your working copy
> on purpose.

## 1. Decision taken: the engine owns fan-out and dedupe

Your #7 put dedupe "at the aggregation layer (UI-track wiring, app state)", and
`docs/architecture.md` §3(a) draws it in the engine. Rather than send that back and
forth, **I've decided it: the engine owns both** (plan §5 E3, recorded as D1).

The reasoning: the cache, the per-source deadlines, the negative-TTL marker, the
sticky-host hint and cancellation all have to live where the fan-out lives. Putting
the merge somewhere else spreads one invariant across two layers, and the merge would
be reading state it doesn't own.

**What you get:** one already-merged, already-deduped, seeder-sorted list, plus
`SourceAnswered` / `SourceFailed` events. Your staggered source tags and the 3s
pending dot still work exactly as you described — they're driven by those events, not
by owning the merge. It's less state in the UI, not less control over presentation.

**If you'd rather have it:** it's about half a day to reverse. The engine emits
per-source batches alongside the merged view and app state merges instead. Say so on
the PR and I'll do it — I just didn't want to block E0 on a round trip.

## 2. Two decisions that depart from your `types.rs`

Taken, not asked (D2 and D3 in the plan's decision record). Both are one match arm
to reverse, so they're cheap to overturn — but here's why I think they should stand.

**`EngineEvent::Error` maps to `failed`, never `missing`.** Your comment at
`src/types.rs:206` says "item → Failed; seed → Missing". I think that's wrong and
slightly dangerous: a transient tracker or network error on a seed would then mark
the user's files missing. `missing` should be reachable *only* from the file-gone
detector, because FR-45's whole purpose is that we never guess wrong in the
direction of "your data is gone" — or worse, re-download 50 GB.

**`Initializing` splits on `finished`.** After a restart a *complete* seed passes
through librqbit's `Initializing` state with `finished == true`. Mapping that state
unconditionally to `downloading` would show every restored seed as an active
download the moment the app opens, which contradicts FR-47. So
`Initializing && finished → seeding (verifying)`. You may want a distinct style for
that verifying moment.

## 3. What changes in the views, and how it lands

**The stats split (plan §3 T7/T8/T9).** `QueueItem` becomes durable-only; volatile
stats move to `EngineStats`; the views render an `ItemView` (`QueueItem` +
`Option<EngineStats>`). This is F-7/A-6 — right now the ledger persists `progress`,
`speed_mib`, `peers` and `eta_secs` to `downloads.json`, which is stale between
status changes and useless because FR-50 says resume comes from librqbit anyway.

It touches `src/ui/downloads.rs` at `:233-234`, `:237`, `:240`, `:307`. **It ships as
one PR touching both files, reviewed with you — `main` is never red.** If that PR
isn't ready when the freeze lands, `QueueItem` keeps the fields as deprecated
pass-throughs for a cycle. I'm not landing a break and leaving you to fix it.

**Two smaller ones in the same PR:**

- `eta_secs: Option<u64>` → `eta: Option<Duration>`. Your reply said `Option<Duration>`
  was implemented, but the file has `eta_secs: Option<u64>` on `QueueItem` and
  `time_remaining: Option<Duration>` on `EngineStats` — so "one unit, converted once"
  was already broken inside the freeze candidate. One unit wins.
- `SourceStatus` gains `Unknown` and `Checking`. **Your 3s pending dot is currently
  unimplementable** — `Online | Empty | Offline` has no way to say "hasn't answered
  yet", so `search.rs:155` renders every unprobed source as `·` forever, and FR-18's
  `checking` state has nowhere to live.

**`HistoryItem`.** FR-49's `history.json` is *search queries*, cap 500; your
`HistoryItem` models completed downloads and cites FR-53, which is bootguard. I'm
making recently-downloaded derive from the ledger (`finished == true`) so there's no
second file, and `history.json` stays search queries. `DownloadsState.history` gets
retyped in the same PR.

## 4. The tidy batch — two items didn't actually land

Not a gotcha, just so you're not surprised when I touch them: `notes-reply-ishan.md`
§6 reports the crate name and the context citations as fixed, but `harbour-tui` is
still at `AGENTS.md:51`, `docs/roadmap.md:32` and `docs/design.md:3`, and
`docs/roadmap.md:5` and `docs/design.md:5` still cite the uncommitted
`harbour-context.md`. `HARBOUR_STATE_DIR` and `docs/context.md` did land.

**I'm taking all of it.** The amendment list is up to 18 (FR-25 dedupe, FR-14
optional magnet, FR-18 `checking`, FR-49 history semantics are new) and I'll land
them in E0 rather than leaving them in your queue — including the FR-43 clause
removal you asked me for.

## 5. Nothing here needs an answer

Everything from your reply is agreed and scheduled, and the open items above are
decided rather than pending — E0 starts now. Every decision on this track, with its
reasoning and its reversal cost, is in [`plan-engine.md`](plan-engine.md) §10; that's
the place to look if something in the engine surprises you later.

The only thing that will land in your files is the stats-split PR in §3, and it comes
with the `downloads.rs` changes already made and reviewed with you.

---
---

# Round 3 — the engine landed, and the app is wired end to end

> From Sarthak. Sarthak asked for the whole product working, so I built through
> the plan rather than stopping at the freeze. Everything below is done and
> pushed; none of it needs a reply. Where I touched your files I say so, and why.

## The E1 gate passed against the live network

Before building anything on librqbit I ran the behavioural spike the plan gated
it behind. Real evidence, not a guess:

```
metadata: name=Some("Sintel") total=129302391 state=Initializing
restored 1 torrent(s) from persistence
```

Metadata fetched from a live swarm, `.torrent` bytes captured for the re-seed
cache (`FR-37`), and **fastresume verified** — a restarted session restores its
torrents without a rehash. That was the plan's designated no-go risk and it is
retired. `pause` also reported peers as `None` rather than `0`, which confirms
the B1 contract against the real engine rather than against my assumption.

`librqbit = "8.1.1"` is pinned.

## What I changed in your files, and why

**The stats split shipped, as promised — in one PR, with `main` never red.**
`downloads.rs` now renders `ItemView` (durable `QueueItem` + optional
`EngineStats`) through accessors: `item.progress()`, `item.peers()`,
`item.eta()`, `item.speed_mib()`. The em-dash-not-zero behaviour you implemented
is preserved and now has a real reason to exist, because the engine genuinely
returns `None` while paused.

**`AppState`, `SearchState`, `DownloadsState` and `Screen` moved to `src/ui`.**
They are UI state, not shared contract, and the freeze is deliberately limited to
what all three tracks share. Your views are otherwise untouched.

**The sidebar table is now typed** (`SourceId` rather than `&str`), so a
source-id typo is a compile error instead of a dot that never lights up.

**`SourceStatus` gained `Unknown` and `Checking`, and `search.rs` renders them.**
Your 3-second pending dot is implementable now — it was not before, because
`Online | Empty | Offline` had no way to say "still waiting". A source that has
not answered yet renders the live glyph muted; never probed stays neutral.

**I found and fixed a real rendering bug in the status area** — mine, not yours.
`ui::status::draw` computes its own banner height and splits the area it is
given into `[Min(0), banner, status]`. `app.rs` was reserving a different height,
so the banner was squeezed out entirely and **the safe-mode warning never
appeared**. I only caught it by running the binary and grepping the rendered
output. `app.rs` now mirrors your formula exactly, with a test pinning it. Worth
knowing about that view's contract if you change it: the caller must reserve
`banner_height + 1`.

## What is new

- `src/input.rs` — the keymap as a pure `(key, screen) -> Action` function, so
  every binding is unit-tested without a terminal. One behaviour worth knowing:
  on the search screen every printable key goes to the text field, so typing
  "dune" cannot fire a download on the `d`. The letter bindings take over only
  when the query is empty; `shift+D` downloads regardless.
- `src/ui/help.rs` — the `?` overlay, with a test asserting every implemented
  binding is documented (`UR-10`).
- `src/app.rs` — the real loop. Boot loads config/ledger/history, arms the crash
  breaker, constructs the engine and restores the queue. Input runs on its own OS
  thread so a keypress never blocks the async runtime. The engine poll is
  adaptive — 500 ms while transferring, 5 s once everything is a settled seed,
  with completion arriving as an event rather than being polled for. Quit flushes
  the ledger and only then clears the marker.

## Verified by running it, not only by testing it

- CLI enqueue → engine → ledger works end to end; `downloads.json` contains
  exactly the durable fields, no progress or speed.
- Killed the process, relaunched: the crash breaker fired and rendered
  *"harbour did not shut down cleanly last time, so everything is paused."*
- The render loop emits DEC 2026 sync sequences around every frame with the
  spinner advancing — your zero-flicker work, running under the real loop.

## Two things I decided rather than ask

- **`.torrent`-on-launch is not wired.** `harbour foo.torrent` parses, finds the
  file, and tells the user plainly that pasting the magnet works instead. Reading
  a `.torrent` means bencode plus hashing its info dict to get the id, and I would
  rather ship an honest message than a half-path. It is a small follow-up.
- **`NFR-03` (100 ms to first paint) is not measured yet.** The startup path now
  constructs a torrent session before the first frame, which is the thing most
  likely to blow that budget. Measuring it — and moving engine construction after
  first paint if it does — is the obvious next task, and I would rather flag it
  than quietly let the number rot.
