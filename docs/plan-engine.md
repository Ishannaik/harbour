# Engine & Foundation — revised track plan (Sarthak)

> **Scope of this document.** It revises the plan for **one track only** — Engine &
> Foundation, i.e. `AGENTS.md`'s "crate skeleton, shared-types freeze, librqbit
> integration, queue, persistence, bootguard, resume". It does **not** change the
> Terminal UI track (Ishan) or the Sources & Cache track (Dhruv). Where a change
> would touch a shared normative artifact (`SPEC.md`, `AGENTS.md`, `docs/roadmap.md`,
> `docs/architecture.md`) I have written the exact amendment in §4 rather than
> editing it unilaterally — per `AGENTS.md` rule 2, SPEC is the referee and changes
> to it go through review first.
>
> Evidence for every engine claim is in [`engine-spike-librqbit.md`](engine-spike-librqbit.md),
> re-derived on a Windows machine against `librqbit 8.1.1`.

---

## 1. Verdict on the existing plan

The existing plan is **good** — better than most repos have at commit 3. `SPEC.md`
is genuinely testable (61 FRs, 13 URs, 8 TRs, 12 NFRs, each with a verification
method), the phase dependency graph is explicit, and the decision to steal
torlink's hard-won behaviours (bootguard, stray detection, metadata caching,
oldest-first promote) rather than rediscover them is exactly right.

I am not proposing to replace it. I am proposing **thirteen specific corrections**.
Three are blocking for my track (F-1, F-2, F-3) and three must land before the
shared-types freeze or every track pays for it later (F-3, F-5, F-13).

The corrections cluster into three themes:

1. **The engine track is scheduled behind work it does not depend on**, and the
   contract it owns is defined inside someone else's phase. (F-1, F-2)
2. **The types freeze is not yet freezable** — it is missing a status the spec
   requires in three places, and it models engine facts that the engine does not
   actually expose in that shape. (F-3, F-5, F-13)
3. **The riskiest dependency is the only one without a spike**, and two spec
   requirements contradict each other in a way that costs runtime performance —
   on a project whose whole premise is "lighter and faster". (F-4, F-7, F-10)

---

## 2. Findings

Each finding: what it is, the evidence, why it matters, what I propose.

### F-1 — The shared-types contract is owned by one track and scheduled inside another **[blocking]**

- `AGENTS.md:11` — Sarthak's roadmap phases are "1 (types), 4, 5".
- `AGENTS.md:33` — "Shared-types freeze (`TorrentResult`, `Source` trait,
  `QueueStatus`, engine event enum) is Sarthak's, **lands in phase 1**."
- `docs/roadmap.md` **Phase 1 has no types task at all.** Its first task is
  "Scaffold crate", the remaining seven are theme/animation/terminal work.
- `docs/roadmap.md` **Phase 2, task 1** is "Define the shared core types once, in
  one module … the mpsc contract phase 4 implements against" — and Phase 2 is
  Ishan's per `AGENTS.md:10`.
- `docs/roadmap.md` Phase 4: "Dependencies: Phase 1 (runtime/lifecycle), **Phase 2
  (shared types)**."

**Why it matters:** the two governing documents disagree about who defines the
contract and when. As written, the engine owner waits on the UI track to define the
engine's own event enum. `AGENTS.md` rule 4 exists precisely to stop this.

**Proposal:** the types module moves into Phase 1, owned by me, as an explicit task.
Phase 2 and Phase 4 both consume it. See F-2 for the phase split that makes this cheap.

### F-2 — The engine track is gated behind UI-domain work **[blocking]**

- `docs/roadmap.md` §6: "**Phase 1 is serial** — everything hangs off it. One
  workstream, do not split."
- Phase 1's eight tasks: crate scaffold, omp theme schema port, theme loader with
  live reload, embedded titanium theme + colour-mode detection, 30fps animation
  subsystem with adaptive backpressure, DEC 2026 BSU/ESU, terminal lifecycle,
  loader/easing primitives. **Seven of the eight are Terminal-UI-track work** by
  `AGENTS.md:10`'s own scope definition.
- Phase 4 (mine) declares a dependency on all of it.

**Why it matters:** the highest-risk work in the project (an unproven engine
dependency) is scheduled to start only after theme JSON live-reload and eased
progress bars are production-grade. That is the wrong risk order, and it wastes the
parallelism the three-track split was created to buy.

The engine's *actual* prerequisites from Phase 1 are narrow: a compiling crate, the
tokio runtime, CLI parsing, config-dir resolution, an error type, and the signatures
of the crash-log and bootguard lifecycle hooks. None of that needs a theme.

**Proposal:** split Phase 1.

| | Phase 1A — Foundation | Phase 1B — Terminal & Theme |
| --- | --- | --- |
| Owner | **Sarthak** | Ishan |
| Contents | crate/module layout, shared types freeze, paths & config-dir, error type, CLI parse, lifecycle hook signatures, CI matrix | theme schema + loader + live reload, titanium, animation loop, DEC 2026 sync, terminal lifecycle impl, loader/easing primitives |
| Effort | ~2 days | as currently scoped |
| Blocks | everything | Phase 2 only |

Phase 4 then depends on **1A + the runtime/lifecycle part of 1B**, not on Phase 2.
1B, Phase 2 and Phase 3 run fully parallel to my E1–E3. The serial barrier shrinks
from a whole UI phase to two days.

### F-3 — `paused` is missing from the normative status vocabulary **[blocking, pre-freeze]**

- `AGENTS.md:48` — "Queue statuses: `queued`, `downloading`, `failed`, `seeding`, `missing`."
- `FR-43` — "`p` on a seeding item pauses/stops seeding; pressing it again resumes".
- `FR-47` — "on bootguard recovery all seeds start paused until resumed".
- `FR-53` — "every restored item is paused, engines start only on explicit user resume".
- And from the engine side: `TorrentStatsState::Paused` is a first-class librqbit
  state (spike V-6/V-7), with `Session::pause`/`unpause` and `is_paused()`.

**Why it matters:** three requirements and the engine itself need a state the shared
vocabulary does not have. Discovered after the freeze, this is a breaking change to
a type every track compiles against.

**Proposal:** six statuses — `queued`, `downloading`, `paused`, `failed`, `seeding`,
`missing` — plus an explicit engine→harbour projection table owned by the engine
track (§3, E0). One decision to make with Ishan: whether a paused download and a
paused seed are one status or two. **My recommendation: one status, disambiguated by
whether the item has ever reached `finished`** — it keeps the enum small and matches
how the user thinks about the `p` key.

### F-4 — The riskiest dependency is the only one without a spike

- `docs/roadmap.md` Phase 7 spikes cs.rin.ru, online-fix.me, cover art, and headless
  daemons — all deferred, none load-bearing.
- `docs/architecture.md` §1 commits librqbit to **three** load-bearing roles: engine,
  session resume, and the phase-6 streaming endpoint.
- `Cargo.toml` currently declares one dependency: `tokio`. librqbit is not pinned,
  and 8→9 is mid-flight (`9.0.0-rc.0` published, `8.1.1` latest stable).

**Why it matters:** committing three subsystems to an unpinned, unspiked, pre-1.0
crate is the largest single schedule risk in the project.

**Proposal:** phase E1, a time-boxed behavioural spike with a written go/no-go and a
recorded fallback. **Half of it is already done** — the static half is in
`engine-spike-librqbit.md`, and the news is good: librqbit 8.1.1 builds clean on
Windows/MSVC in 97 seconds with no native toolchain. Pin `8.1.1` exact; stay off the rc.

### F-5 — `peers` and ETA are not shaped the way the spec assumes **[pre-freeze]**

- `FR-32` (downloads) and `FR-44` (seeding) both render **peers**.
- Spike V-8: `TorrentStats` has **no peer field**. Peers are at
  `stats.live?.snapshot.peer_stats`, and `live` is `None` whenever the torrent is
  paused, initializing, or errored.
- Spike V-9: the engine already supplies `time_remaining`, and speeds are
  `Speed { mbps: f64 }` — MiB/s floats, not bytes/sec integers.

**Why it matters:** if the frozen type says `peers: u32`, a paused seed renders as
"0 peers", which is a lie the UI cannot distinguish from a real zero. And if we hand
Ishan bytes/sec when the engine gives MiB/s, the conversion gets done twice or not
at all.

**Proposal:** `peers: Option<u32>`, `eta: Option<Duration>`, and speeds carried in
one documented unit with the conversion done once, in the engine adapter. `FR-32`
and `FR-44` get a clause for the unknown case (§4).

### F-6 — The 500 ms poll is applied to items that cannot change

- `FR-32`/`FR-44` fix a 500 ms stats poll; `docs/architecture.md` §4 applies it
  "per active item".
- Spike V-10: `wait_until_completed()` is an awaitable future — completion does not
  need to be discovered by polling.

**Why it matters:** on a seedbox with 200 idle seeds, a flat 500 ms poll is 400
stat reads per second to learn nothing, against `NFR-04`'s ≤2% idle CPU. For a
project whose goal is "lighter", this is free.

**Proposal:** adaptive cadence — 500 ms while any item is `downloading`, 5 s when
every item is a settled seed, and completion driven by `wait_until_completed()`
rather than by observing `finished` on a tick. Keeps `FR-32`'s user-visible
guarantee, drops the idle cost.

### F-7 — The ledger's `progress` field is redundant with `FR-50` and stale by construction

- `FR-48` — `downloads.json` holds "status **and progress**", "written atomically
  (write-temp + rename) **on every status change**".
- `FR-50` — "**piece-level resume state comes from librqbit's session, not from
  `downloads.json`**."
- Spike V-12: librqbit already persists its own session state
  (`SessionPersistenceConfig::Json { folder }`).

**Why it matters:** this is a redundancy, not a contradiction — `FR-50` governs only
where *resume* state comes from. But it leaves `progress` in a bad spot either way.
Written on status change only (as `FR-48` specifies), it is stale between
transitions and nothing may trust it. Written often enough to be accurate, it turns
a whole-file atomic rewrite into a per-poll-tick treadmill — the reference product's
known weakness, which we are supposedly improving on. There is no cadence at which
the field is both correct and cheap, and `FR-50` means nothing needs it.

**Proposal:** the ledger stores **durable identity only** — info_hash, name, source,
magnet, output folder, status, timestamps. Volatile stats never touch disk. Writes
fire on status transitions, are debounced, and are flushed synchronously on exit.

### F-8 — Crash-marker and ledger-flush ordering is unspecified

- `docs/architecture.md` §5 step 1: marker written at boot. `FR-08`: cleared on clean exit.
- The roadmap puts bootguard in Phase 5; the marker's write/clear hook points are in
  Phase 1's terminal lifecycle.
- `UR-08`: exit is synchronous and never waits on engine sockets.

**Why it matters:** if the marker is cleared before the ledger is flushed, a crash
in that window leaves "clean marker + stale ledger" — bootguard stands down exactly
when it was needed. The reference product gets this right and the ordering is load-
bearing: `persistSync()` then `disarmBootMarker()`, in that order, in one function.

**Proposal:** normative ordering — **flush the ledger synchronously, then clear the
marker, then restore the terminal, then exit.** Phase 1A defines the lifecycle hook
signatures; E3 fills them in.

### F-9 — CI is single-OS while the spec names Windows as the primary target

- `.github/workflows/ci.yml` **as of commit `5ba92c8`**: one job, `runs-on:
  ubuntu-latest`. (Fixed on this branch — see the note at the end of this finding.
  The file in your working tree already has the matrix.)
- `NFR-08`: "**Primary target Windows Terminal (Windows 11)**; macOS and Linux
  terminals are supported for the same feature set."
- `FR-06`: `%USERPROFILE%\.harbour`. `FR-55`: atomic rename **on the same volume**.
  `UR-03`: DEC 2026 sync "observable on Windows Terminal".

**Why it matters:** path resolution, atomic rename semantics, and file locking are
where cross-platform actually breaks, and all three live in my files. The reference
product learned this and moved to a three-OS matrix. Doing it now, while the repo is
four lines of Rust, is free; doing it at Phase 5 means bisecting a Windows failure
through three tracks of merged work.

**Proposal:** three-OS matrix. **Applied on this branch** — it is foundation work,
in-track, and small enough to review at a glance.

### F-10 — The performance bar is set at the level of the thing we are replacing

- `NFR-03`: "TUI interactive (splash visible) **≤ 500ms** after process start."
- The reference product is a Node CLI that boots React + Ink + Yoga-WASM + a
  222-package engine graph before its first frame. I have **not** measured its
  startup and no in-repo evidence exists; the point does not depend on the exact
  number, only on the fact that 500 ms is a budget a Node TUI can plausibly hit and
  a static Rust binary should not need.

**Why it matters:** "lighter and faster" is the reason this project exists. A
statically linked Rust binary that renders a first frame should be an order of
magnitude under this. As written, harbour could ship, meet `NFR-03`, and be no
faster to start than the thing it replaces — and nothing in the spec would catch it.
There is also no requirement for memory or binary size at all, which are the two
places where "lighter" is actually observable.

**Proposal:** tighten `NFR-03` to **≤100 ms p95** to first paint (excluding
first-run config creation, measured on the reference machine), and add two NFRs for
idle RSS and release binary size (§4). Numbers get set from E1's measurements —
but the requirement should exist before the numbers do.

### F-11 — The declared source of truth is a machine-local path

`docs/architecture.md:7` — "Contract: `C:/tmp/harbour-context.md` is the single
source of truth. Every name, keybind, and decision below is normative."

That path does not exist on my machine and cannot exist for a contributor on macOS
or Linux. A normative document that no one else can read is not normative.

**Proposal:** fold it into `SPEC.md` or commit it as `docs/context.md`, and make
`architecture.md` point at the in-repo copy.

### F-12 — The missing-file detector's constants were derived for a different engine

- `FR-45`: a seed with `speed > 0 && progress < 1` for 2 consecutive polls after a
  **10 s grace** is flagged `missing`.
- Those constants come from the reference product, where the grace exists to cover
  **hash re-verification on re-seed** — during which `progress < 1` with
  `downloadSpeed > 0` is normal and indistinguishable from a genuinely missing file.
- librqbit has a distinct `Initializing` state (spike V-6) and its own resume state
  (V-12), so the condition the grace was protecting against may not arise in the
  same shape. And the inputs differ in shape: download speed is reachable only
  through `live: Option<LiveStats>` (so it is *absent*, not zero, whenever the
  torrent is not live) and is MiB/s as `f64`, not a flat byte count (V-9).

**Why it matters:** copying the constants without the reasoning gives us a detector
that is either dead code or a false-positive generator, and this detector's failure
mode is "silently re-download 50 GB". `FR-45`'s *intent* is right and must survive.

**Proposal:** keep the intent, re-derive the mechanism from observed librqbit
behaviour in E1 spike item 3, and amend `FR-45` to state the condition in engine
terms. The acceptance test is behavioural, not numeric: **delete the file under a
running seed → the item goes `missing` and no re-download starts.**

### F-13 — Two crate names across five documents

`harbour-tui` in `AGENTS.md:51`, `docs/roadmap.md:32` and `docs/design.md:3`;
`harbour` in `SPEC.md:3` and `Cargo.toml:2` — and `Cargo.toml` is the one that
actually compiles.

Trivial, but it is a shared-vocabulary item in the foundation I own, and it will
cost someone an afternoon of confused imports.

**Proposal:** `harbour`, matching `SPEC.md` and the committed `Cargo.toml`; correct
`AGENTS.md:51`. Single crate for now — a workspace split is justified by compile
times, and we don't have compile times yet (`AGENTS.md` rule 8).

---

## 3. The revised track plan

Replaces roadmap Phase 1 (types), Phase 4 and Phase 5 **for this track**, pending
review. Phase numbering is deliberately `E`-prefixed so it does not collide with the
shared roadmap's numbering until §4's amendments are accepted.

### E0 — Foundation (= proposed Phase 1A) · ~2 days · blocks everything

- Crate/module layout per `docs/architecture.md` §2; resolve the crate name (F-13).
- **`types.rs` — the freeze.** `TorrentResult`, `Source` trait signature,
  `QueueStatus` (six variants, F-3), `QueueItem`, `EngineEvent`. Documented as
  breaking-on-change per `AGENTS.md` rule 4.
  - `peers: Option<u32>`, `eta: Option<Duration>`, one documented speed unit (F-5).
  - The **engine→harbour status projection table**, written down once here:

    | librqbit state | harbour status |
    | --- | --- |
    | `Initializing && !finished` | `downloading` |
    | `Initializing && finished` | `seeding` (verifying) |
    | `Live && !finished` | `downloading` |
    | `Live && finished` | `seeding` |
    | `Paused` | `paused` |
    | `Error` | `failed` |
    | — | `missing` is harbour-derived only (F-12) |

    The `Initializing` split is load-bearing: after a restart a **complete** seed
    passes through `Initializing` with `finished == true`, and mapping that to
    `downloading` would show every restored seed as an active download, breaking
    `FR-47`. `queued` is harbour-side too — it exists before the item is handed to
    the engine at all.
- **An `Engine` trait plus a fake implementation.** This is the piece that unblocks
  everyone: the queue becomes unit-testable with no network, and Ishan's Phase 2
  gets a real downloads contract instead of inventing fake download data.
- `paths.rs`: config-dir resolution (`FR-06`) **plus a `HARBOUR_STATE_DIR` override**
  so all three tracks can test against a temp dir and never touch real user state.
  (Ported from the reference product's `TORLINK_STATE_DIR`; it is the reason their
  test suite can touch persistence at all.)
- Error type; crash-log hook and bootguard arm/disarm hook **signatures** (F-8) —
  1B implements the terminal side against them.
- CLI parse: `FR-02`–`FR-05`, infohash validation, magnet builder.
- **`FR-07`**: `HARBOUR_MAX_DOWNLOADS` parsed at startup, invalid → unlimited with a
  warning. (The cap *logic* is E2's; the env parse belongs with the other startup
  config so there is one place where the environment is read.)
- CI three-OS matrix (F-9).

**DoD:** `cargo build && cargo test && cargo clippy -- -D warnings && cargo fmt
--check` green on Linux, macOS **and Windows**; `harbour --version` / `--help`
per `FR-03`/`FR-04`; unit tests for infohash validation, magnet builder, and path
resolution under both `%USERPROFILE%` and `$HOME`; the types module has rustdoc on
every public item and is announced as frozen in the PR description.

### E1 — librqbit behavioural spike + decision gate · ~2–3 days

Static half is done (`engine-spike-librqbit.md`). Remaining, each a pass/fail gate:

1. Real tiny magnet: add → metadata → download → finish → seed.
2. `kill -9` mid-download → relaunch → resumes **with no rehash**.
3. Delete the file under a live seed → **record what `TorrentStats` actually
   reports**; that recording is the specification for F-12's detector.
4. `pause`/`unpause` round trip; `delete(id, delete_files: true)` with an open
   handle (Windows locking is the risk).
4b. **Re-add from a cached `.torrent` against files already on disk** and confirm it
   verifies locally without swarm traffic. This is `FR-37`'s load-bearing half and
   the whole reason we cache the metadata; if librqbit re-fetches anyway, the cache
   is dead weight and `FR-37` needs rewriting.
5. Measure `Session::new` cost, RSS at 1 and 20 torrents, release binary size →
   these become the numbers behind the new NFRs (F-10).
6. `default-tls` vs `rust-tls` (rustls avoids a system OpenSSL dependency — likely
   right for a portable binary).

**DoD:** written verdict appended to the spike doc, version pinned in `Cargo.toml`
with a why-comment, go/no-go recorded. **No-go fallback**, recorded now rather than
improvised later: drive the `rqbit` binary over its `http-api` feature as a sidecar.
That costs the single-binary property — a product decision for Ishan, not a dead end.

### E2 — Engine + queue (= roadmap Phase 4, engine half) · ~1.5–2 weeks

- `engine.rs`: `Session` wrapper, add from magnet/infohash/`.torrent`, handle
  registry keyed by info_hash, the projection table from E0.
- **Adaptive stats cadence** (F-6): 500 ms while anything is downloading, 5 s when
  everything is a settled seed, completion via `wait_until_completed()`.
- `queue.rs`: concurrency cap (`FR-31`), oldest-first `promote()`, duplicate
  detection by info_hash (`FR-56`) — all unit-tested against the fake `Engine`.
- Metadata capture → `cache/torrents/<info_hash>.torrent`, **and the re-seed path
  that consumes it** — `FR-37` is two halves and only the capture half is obvious.
- Seed-by-default with **trackers-override** support (`FR-42`, carried over from
  roadmap Phase 4 task 5 so it does not fall through the phase re-cut).
- Missing-file detector, re-derived from E1 item 3 (F-12).
- Error surfacing (`FR-36`) from both add-time failures and `stats.error` (V-13).

**DoD:** `HARBOUR_TEST_NET=1` integration test covering add → download → seed →
pause → resume → missing; a re-seed from cached metadata that performs no swarm
metadata fetch (`FR-37`); queue semantics fully covered by network-free unit tests
against the fake `Engine`; `NFR-11` path-safety tests (no cache or ledger path is
ever derived from a torrent name).

### E3 — Persistence + bootguard + resume (= roadmap Phase 5) · ~1 week

- Ledger with durable fields only (F-7); atomic temp+rename; debounced writes;
  synchronous flush on exit.
- `history.json` cap 500 (`FR-49`); `config.toml` (`FR-51`); corrupt-file quarantine
  (`FR-54`); **recently-downloaded list persisted and restored** (roadmap Phase 5
  task 7, carried over so it does not fall through the phase re-cut).
- Bootguard: arm at boot, **flush-then-disarm** on clean exit (F-8); safe mode
  restores everything paused with a banner.
- Reconcile the ledger against librqbit's session state on startup (`FR-50`).

**DoD:** `kill -9` mid-download → relaunch resumes with no rehash; simulated crash →
safe mode with zero engines started; corrupt ledger quarantined and startup survives;
verified on all three OSes.

### E4 — Integration · joins Ishan and Dhruv

Hand over the real `Engine` behind the E0 trait, replacing the fake. Own the M0
acceptance run for items 4–8 of `SPEC.md` §8.

**Track total: ~4–5 weeks**, of which only E0's two days are a barrier to the other
two tracks.

---

## 4. Amendment requests to shared documents

These need Ishan's review because they touch normative artifacts. Wording is
ready-to-merge.

| # | File | Change | From finding |
| --- | --- | --- | --- |
| A-1 | `AGENTS.md:48` **+ `SPEC.md` FR-30 (`:147`) + FR-48 (`:195`)** | Queue statuses become `queued`, `downloading`, `paused`, `failed`, `seeding`, `missing`. All three lists must change together — as it stands the ledger's own enum (`FR-48`) would make `paused` unpersistable. | F-3 |
| A-2 | `AGENTS.md:51`, `docs/roadmap.md:32`, `docs/design.md:3` | Crate is `harbour` (not `harbour-tui`), matching `SPEC.md:3` and `Cargo.toml:2`. | F-13 |
| A-3 | `docs/roadmap.md` §2 and §6 | Split Phase 1 into 1A (Foundation, Sarthak) and 1B (Terminal & Theme, Ishan); move the "define the shared core types" task from Phase 2 to 1A; §6's "do not split" applies to 1B. **Atomically**: delete the types task at `roadmap.md:57`, drop "shared types are stable" from the Phase 2 DoD (`:70`), and fix "which phase 2 pins down first" (`:247`). Leaving any of those makes two tracks owners of the frozen contract, which is the exact failure `AGENTS.md` rule 4 exists to prevent. | F-1, F-2 |
| A-4 | `docs/roadmap.md` Phase 4 | Dependencies become "Phase 1A (types, paths, runtime) + Phase 1B (terminal lifecycle)" — not Phase 2. | F-1, F-2 |
| A-5 | `SPEC.md` FR-32, FR-44 | Add: "peers and ETA are absent while the torrent is not live (paused/initializing/errored); the UI renders `—`, never `0`." | F-5 |
| A-6 | `SPEC.md` FR-48 | Drop "and progress" from the ledger; add "volatile statistics are never persisted; piece state is librqbit's per FR-50. Ledger writes fire on status transition, are debounced, and are flushed synchronously on exit." | F-7 |
| A-7 | `SPEC.md` FR-08 / UR-08 | Add the normative exit ordering: flush ledger → clear crash marker → restore terminal → exit. | F-8 |
| A-8 | `SPEC.md` FR-45 | Restate the detector in librqbit terms; replace the numeric constants with the condition recorded in E1 spike item 3. Acceptance stays behavioural: delete the file under a seed → `missing`, no re-download. | F-12 |
| A-9 | `SPEC.md` NFR-03 | Tighten to ≤100 ms p95 to first paint (excluding first-run config creation). | F-10 |
| A-10 | `SPEC.md` §7 | Add **NFR-13 (Footprint)**: idle RSS ≤ *N* MB with 20 seeds. Add **NFR-14 (Footprint)**: release binary ≤ *N* MB. *N* set from E1 measurements. | F-10 |
| A-11 | `docs/architecture.md:7`, `docs/roadmap.md:5`, `SPEC.md:8` | Replace every reference to the machine-local context file with an in-repo path. All three cite it as normative. | F-11 |
| A-12 | `SPEC.md` §9 OQ-1, **co-amending FR-43 (`:180`) and UR-10** | OQ-1 (`p` = pause-only vs pause-with-remove) is answerable now: librqbit has `pause`/`unpause` **and** `delete(id, delete_files)`, so both are cheap. Recommend pause-only on `p`, deletion behind an explicit confirm — which means FR-43's "or the item is removed per config" clause must go at the same time, or Ishan wires a keybind to a behaviour we just decided against. | spike V-11 |
| A-13 | `AGENTS.md:49` | Add `HARBOUR_STATE_DIR` to the normative env-var vocabulary alongside `HARBOUR_MAX_DOWNLOADS` and `HARBOUR_TEST_NET`. E0 introduces it and all three tracks will use it for tests. | E0 |

**Applied on this branch** (in-track foundation, small enough to review at a glance):

- `.github/workflows/ci.yml` → three-OS matrix (F-9).

---

## 5. What I deliberately did not change

Out of my track. Flagged here so they are not lost, for Ishan and Dhruv to accept or
reject on their own terms.

- **Sources track — the 1337x critical path.** The reference product fetches up to
  four detail pages per 1337x search purely to extract magnets, and registers 1337x
  **twice** (movies + TV), so one user search can cost up to sixteen requests to the
  single flakiest source. `FR-11` reproduces both registrations. Suggestion for
  Dhruv: fetch the detail page **lazily**, only when a row is selected or
  downloaded — list rows already carry name, size and seeders — and let one fetch
  serve both category tabs. This is the largest perceived-latency item in the
  reference product and it is a design fix, not a language fix.
- **Sources track — per-source deadlines.** `FR-16` mandates a per-request timeout
  and retry policy but no wall-clock deadline for the whole source. A hard budget
  with partial rendering is what makes a search *feel* fast when one source is sick.
- **Phase 7 — headless daemons.** librqbit ships `http-api`, `webui` and `watch` as
  upstream features (spike V-4). Most of that spike is "enable a feature and write a
  compatibility shim", not "build four daemons". Worth re-scoping when it comes up.
- **All of `docs/theming.md`, UR/TR requirements, and Phase 2/3/6 scope** — Ishan's
  and Dhruv's, untouched.

---

## 6. Risk register — this track

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| librqbit fastresume or the missing-file signal doesn't behave as needed | Medium | Project-shaping | E1 gate with a recorded sidecar fallback |
| 8→9 major lands mid-build and 8.1.1 goes unmaintained | Medium | Medium | Pin exact; `Engine` trait keeps the blast radius to one module |
| Types freeze breaks after other tracks build against it | **Medium-high** | High — recompiles all three tracks | F-3/F-5/F-13 fixed **before** the freeze; that is the entire point of E0 |
| Windows-specific persistence failure found late | Medium | High | Three-OS CI from commit 4 (F-9) |
| E0 slips and blocks two other people | Low | High | Two days, tightly scoped, no engine dependency |
| Apache-2.0 (librqbit) vs MIT (harbour) NOTICE obligations | Low | Low | Handle at packaging; permissive-compatible |

---

## 7. Open questions for the team

1. **Ishan** — do you accept the Phase 1A/1B split (A-3, A-4)? It is the change that
   decides whether two tracks wait two days or a whole UI phase.
2. **Ishan** — paused download and paused seed: one status or two (F-3)? I recommend
   one; you own how it renders.
3. **Ishan** — `NFR-03` at 100 ms (A-9). If we are not going to be an order of
   magnitude faster to start than the reference product, we should say so in the
   README rather than in a requirement no one measures.
4. **Dhruv** — the lazy-1337x suggestion in §5. Yours to take or leave; it is the
   single biggest search-latency lever and it is cheaper to build that way from the
   start than to retrofit.
5. **Both** — where should the `C:/tmp/harbour-context.md` content live (A-11)?

---

## 8. Verification log

This document was not written and shipped in one pass. What was checked, and how:

**Engine claims** — every fact in §2 that concerns librqbit was re-derived on this
machine against `librqbit 8.1.1`: `cargo search`/`cargo info` for versions, features
and licence; `cargo add` + `Cargo.lock` for resolution; `cargo build` on
Windows/MSVC for buildability; and the vendored source in `~/.cargo/registry` for
every API shape. Nothing is quoted from a summary. Details and file:line evidence in
[`engine-spike-librqbit.md`](engine-spike-librqbit.md).

**Repo claims** — every `file:line` citation in §2 was read in the tree at commit
`5ba92c8` before being cited.

**Adversarial review** — a second model (opencode, `deepseek-v4-flash-free`) was
given this document plus the six artifacts it critiques, and asked only to find
errors: verify each finding's evidence, name anything overstated or already handled,
name what the plan missed for this track, and flag amendments that would break
another track. Its findings were then re-verified against the files rather than
accepted on trust. It was right about all of the following, and this document was
corrected accordingly:

| It found | Fix |
| --- | --- |
| The projection table mapped `Initializing` unconditionally to `downloading`, which would render every **restored complete seed** as an active download, breaking `FR-47` | E0's table now splits `Initializing` on `finished` |
| `FR-42` trackers-override and roadmap Phase 5's recently-downloaded persistence **fell through the phase re-cut** | Carried into E2 and E3 explicitly |
| `FR-07` (`HARBOUR_MAX_DOWNLOADS` env parse) was scheduled nowhere | Added to E0 |
| `FR-37`'s load-bearing half — verifying on-disk files from cached metadata without a swarm re-fetch — had no task, test or DoD | Added to E1 (spike 4b) and E2 |
| A-1 changed the vocabulary but not `FR-30`/`FR-48`, leaving `paused` unpersistable | A-1 now amends all three |
| A-3 would have left the types task in Phase 2, giving **two tracks ownership of the frozen contract** | A-3 now specifies the deletions that must land atomically with the move |
| A-12 contradicted `FR-43`'s surviving "or removed per config" clause | A-12 now co-amends FR-43 and UR-10 |
| `HARBOUR_STATE_DIR` was introduced without being added to the normative vocabulary — this document breaking its own discipline | Added as A-13 |
| F-7 called a redundancy a "contradiction" and asserted a write treadmill the spec does not actually mandate | Reframed as redundant-and-stale-by-construction |
| F-9's evidence line described a file this branch had already fixed | Now dated to commit `5ba92c8` |
| F-12 cited "speed is `Option`"; `Speed` is not an `Option` — it is reachable only through `live: Option<LiveStats>` | Citation corrected |
| F-13 said "three ways"; it is two names across five documents | Corrected, and A-2 extended to `roadmap.md:32` and `design.md:3` |
| The intro said "twelve corrections" and "four blocking" against thirteen findings and three blocking tags | Corrected |

It also read the CI matrix as already-committed and marked F-9 as failing; that is a
sequencing artifact — the matrix was applied on this branch before the review ran —
but the wording was genuinely stale and has been fixed. One thing it missed:
`docs/design.md:3` is a fifth `harbour-tui` reference.

**Still unverified, and flagged as such:** every behavioural claim about librqbit at
runtime (fastresume, missing-file signalling, Windows file-locking on delete) and
every performance number. Those are E1's job. No number in this document is
presented as measured unless it is in the spike doc's evidence table.
