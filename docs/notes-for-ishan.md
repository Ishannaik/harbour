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
from VaultIndex, ReelIndex and TorrentHub appears three times in three blocks. The reference
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

## 1. One question I can't answer for you: who owns dedupe?

Your #7 says dedupe happens "at the aggregation layer (UI-track wiring, app state)".
But `docs/architecture.md` §3(a) draws the search flow as `AppState → Engine →
Sources → Cache`, with the engine doing the fan-out and the UI consuming a merged
stream — and the cache, the per-source deadlines, the negative-TTL marker and the
sticky-host hint all have to live wherever the fan-out lives.

My plan puts fan-out **and** dedupe in the engine (E3), because splitting them means
the merge happens in one crate and the thing it merges is produced in another. But
this is your call as much as mine, and it's a week of work either way.

**Please pick one:**

- **Engine owns fan-out + dedupe** (my assumption). UI receives an already-merged,
  already-deduped, already-sorted list plus per-source status events. Less UI state.
- **UI owns dedupe.** Engine streams per-source results and the app state merges. You
  keep more control over ordering and the stagger; I keep only the cache and
  deadlines.

## 2. Two places I'm deliberately departing from your `types.rs`

Both are arguments, not overwrites — tell me if you disagree.

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

## 5. Nothing else needs you

Everything else from your reply is agreed and scheduled. If you're happy with §1 and
don't object to §2, I have no blockers on your side.
