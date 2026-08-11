# Engine & Foundation — plan of record (Sarthak)

> **Status:** final, end-to-end. This replaces the first revision (git history has
> it at `f58ec1e`). That version was written before the UI and Sources tracks
> replied and before phase-2 UI code merged; both changed what this track owes.
>
> **Scope:** the Engine & Foundation track only — `AGENTS.md:11`'s "crate skeleton,
> shared-types freeze, librqbit integration, queue, persistence, bootguard, resume",
> plus the search-orchestration work the two replies assigned here.
>
> **Verification:** every `file:line` below was read in the tree at `110b674`.
> Every librqbit fact is re-derived locally against 8.1.1
> ([`engine-spike-librqbit.md`](engine-spike-librqbit.md)). Every compile claim was
> compiled. Reviewed adversarially by a second model; §9 logs what that changed.

---

## 1. Where things actually stand

**Merged:** theme system, animation primitives, splash, theme watcher, phase-2 views
(search/downloads/status), deterministic fake data, a working `src/types.rs`, and
three-OS CI. Both other tracks have replied in full and agreed to every decision I
asked for.

**Not merged, and this matters:** Dhruv's reply describes `net.rs`, `magnet.rs`,
`cache.rs`, `parse.rs`, a pinned registry and "22 tests green"
(`notes-for-dhruv.md:158-175`). **None of it is on any pushed branch** — `main` has
only `src/{anim,app,fake,main,theme,theme_watch,types}.rs` and `src/ui/*`. I cannot
reconcile the freeze against code I cannot read, so §3 picks shapes on merit and
states the migration cost for each.

**Of my thirteen amendments, two landed.** `HARBOUR_STATE_DIR` is in `AGENTS.md:49`
and `docs/context.md` is committed. The rest did not, including two that
`notes-reply-ishan.md:46-49` reports as done:

| Claimed done | Actual |
| --- | --- |
| "Crate name standardized on `harbour` (fixed AGENTS/roadmap/design)" | `harbour-tui` still at `AGENTS.md:51`, `docs/roadmap.md:32`, `docs/design.md:3` |
| "all `C:/tmp/harbour-context.md` citations fixed" | `docs/roadmap.md:5` and `docs/design.md:5` still cite the uncommitted scratch file |

Not a criticism — Ishan explicitly deferred the SPEC edits to my 1A pass
(`notes-reply-ishan.md:42`, `:51-53`). But the amendments are mine to land, and I
should stop treating them as someone else's queue.

**Three documents now describe the same contract differently**, and none of them
compiles as written. Resolving that is the whole of E0.

---

## 2. The two defects that decide the freeze

### 2.1 Neither candidate `Source` trait can back a registry

This is the finding that matters most, and I verified it by compiling both.

`docs/sources.md:39-55` declares `async fn search(...)` and then
`pub type ArcSource = Arc<dyn Source>`. `src/types.rs:84-91` declares
`fn search(...) -> impl Future<...>`. **Both are dyn-incompatible.** A type alias
isn't checked until used, which is why the error has stayed hidden — the moment
either is used for the 10-source registry the fan-out needs, it fails:

```
error[E0038]: the trait `Source` is not dyn compatible
   = help: consider moving `search` to another trait
```

`async fn` and `-> impl Future` in a trait both forbid a vtable. So
`Vec<Arc<dyn Source>>` — the natural shape for "fan out across ten heterogeneous
sources" — is impossible in either. Dhruv is building his registry against a
contract that cannot exist, and neither CI nor a review would have caught it
because nothing has instantiated it yet.

**Fix, compiled and verified with a real registry and fan-out:** return a boxed
future at the trait boundary. Adapters still write ordinary `async` code; only the
signature changes.

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

This is exactly what `#[async_trait]` generates, without the dependency
(`AGENTS.md` rule 8). Cost to Dhruv: one `Box::pin(async move { … })` wrapper per
adapter. Cost of not doing it: the registry never compiles.

### 2.2 `&'static str` cannot round-trip, and the cache depends on it

`docs/sources.md:86-88` derives `Serialize`/`Deserialize` on `TorrentResult`
"because the search cache persists it verbatim". `src/types.rs:55` types
`source: SourceId = &'static str`. Those are mutually exclusive — `&'static str`
has no `Deserialize`. I already hit this on the persisted structs and had to widen
them to `String` to make PR #3 compile (`src/types.rs:147`, `:173`).

**Fix:** `SourceId` becomes an enum with serde, as `docs/sources.md:22-28` already
has it, plus `as_str()` for display and cache paths. Exhaustive matching, no typos,
and the cache becomes possible.

---

## 3. The freeze (E0 deliverable)

Type by type, with the decision and its cost. This is the normative contract;
`src/types.rs` on `main` is superseded.

| # | Item | Decision | Rationale / cost |
| --- | --- | --- | --- |
| T1 | `Source` trait | Boxed-future, `def() -> &'static SourceDef`, typed error, `SearchCtx` param | §2.1. `def()` over five getters: one const per source instead of five fns; mechanical migration |
| T2 | `SourceId` | `enum` + serde + `as_str()` | §2.2. Unblocks the cache; matches `docs/sources.md` |
| T3 | `SourceError` | `enum { Network, Parse, Blocked, Timeout }`, `thiserror` | `Result<_, String>` (`types.rs:90`) erases the classification Dhruv's `Blocked` fast-fail, `429` handling and negative-TTL gating all key off |
| T4 | `TorrentResult.magnet` | `Option<String>`; `None` = resolvable on demand | Both tracks committed to it (`notes-for-dhruv.md:189-191`, `notes-reply-ishan.md:80-83`). Contract: **a displayable row never requires the magnet** |
| T5 | `TorrentResult.added` | `Option<i64>` unix seconds | `docs/sources.md:82` wants `DateTime<Utc>`; chrono is not a dependency and this is one integer. Serde-trivial |
| T6 | `QueueStatus` | Six: `queued, downloading, paused, failed, seeding, missing` | Already in `types.rs:111-118` and agreed by both. Needs the doc amendments (A-1) |
| T7 | `QueueItem` | **Durable fields only** — id, name, source, magnet, dir, status, `finished`, total_bytes, added_at | F-7. Keeps `finished`, which Ishan's paused-seed rendering depends on (`notes-reply-ishan.md:25-30`) |
| T8 | `EngineStats` | All volatile stats, **never persisted**; `eta: Option<Duration>` | Resolves the file's internal split (`eta_secs: Option<u64>` at `types.rs:161` vs `time_remaining: Option<Duration>` at `:188`) |
| T9 | `ItemView` | `QueueItem` + `Option<EngineStats>`, what the UI renders | The seam that lets T7 happen without deleting the downloads view |
| T10 | `SourceStatus` | `Unknown, Checking, Online, Empty, Offline` | `FR-18` mandates `checking`; `types.rs:61-66` has no way to express it, so the UI renders every unanswered source as "unknown" forever. Also carries Ishan's 3s *pending* dot |
| T11 | `SearchCtx` | `{ list_deadline, total_deadline, host_hint, cancel }` | Carries `FR-20` cancellation, Dhruv's per-phase budget and his session-scope sticky-host hint. Needs `tokio-util` for `CancellationToken` |
| T12 | `EngineEvent` | `Metadata, Progress, Done, Error, SourceAnswered, SourceFailed` | Search events join engine events on one channel; the UI needs `SourceAnswered` to update "N results from M sources" |
| T13 | `Engine` trait + fake | Object-safe, with an in-memory fake | Makes the queue unit-testable with no network and gives the UI a real contract instead of `fake.rs` |

**Engine → harbour status projection**, written down once, here:

| librqbit state | harbour status |
| --- | --- |
| `Initializing && !finished` | `downloading` |
| `Initializing && finished` | `seeding` (verifying) |
| `Live && !finished` | `downloading` |
| `Live && finished` | `seeding` |
| `Paused` | `paused` |
| `Error` | `failed` |

`queued` is harbour-side (before the item reaches the engine). **`missing` is
reachable only from the file-gone detector, never from an engine error** — this
contradicts `src/types.rs:206` ("seed → Missing") and the contradiction is
deliberate. Conflating them means a transient tracker error marks a seed's files
missing; `FR-45`'s entire purpose is that we never guess wrong in that direction.

**The `Initializing` split is load-bearing:** after a restart a complete seed passes
through `Initializing` with `finished == true`, and mapping that to `downloading`
would show every restored seed as an active download, breaking `FR-47`.

---

## 4. Failure policy — nothing forced, everything degrades

This section is normative for every line of code in this track, and it outranks the
phase list: a feature that works only on the happy path is not done.

The reference product earns its reputation here, and it is the thing most worth
stealing. A native module that will not build prints a warning and the app runs
without WebRTC. A config that will not parse falls back to defaults. A boot that
died mid-restore comes back paused instead of walking into the same explosion. A
source that is down names itself and the search continues. **Not one of those
failures reaches the user as a crash.** That is the bar.

### 4.1 The three invariants

1. **The app always reaches a usable screen.** No failure in config, ledger, theme,
   engine construction, or restore may leave the user staring at a spinner or a
   panic. If everything else fails, an empty queue and a working search bar is a
   valid outcome.
2. **A subsystem failure never escalates.** A dead source does not stop a search; a
   failed item does not stop the queue; a persistence error does not stop a
   download; a poisoned lock does not stop the render loop.
3. **Never silently destroy user data.** When we are unsure, we stop and say so —
   we do not re-download 50 GB, do not delete files, do not overwrite a ledger we
   failed to parse. Uncertainty degrades to *paused and visible*, never to *acted
   upon*.

### 4.2 Failure modes and their fallbacks

Every one of these is a test, not a hope.

| Failure | Fallback | Never |
| --- | --- | --- |
| `config.toml` missing | Defaults, silently — a first run is not an error | — |
| `config.toml` corrupt | Defaults + a loud banner naming the file | Silent defaults; overwriting it |
| `downloads.json` corrupt | Quarantine to `.corrupt`, start empty, banner | Deleting it; starting with a half-parsed queue |
| A single ledger entry is malformed | Skip that entry, keep the rest, log it | Discarding the whole ledger for one bad row |
| Ledger write fails (disk full, permissions) | Banner, keep running in memory, retry on the next transition | Killing the download; a torn file |
| Previous boot died mid-restore | Safe mode: everything paused, no engine starts, banner explains | Auto-resuming into the same crash |
| librqbit `Session` will not construct | Search still works; downloads report unavailable with the reason | Aborting startup |
| A single torrent fails to add | That item → `failed` with the message + retry affordance | Failing the batch or the restore |
| Engine stats read throws / returns partial | Use what we have, keep the poller alive | An escaping panic in the poll task |
| Seed's files are gone | `missing`, engine stopped, item visible | Re-downloading; deleting the record |
| Metadata never arrives (dead magnet) | Time out, `failed`, explain "no peers found" | Hanging forever |
| A source times out / is blocked | `Checking` → `Offline` with the reason; other nine continue | Failing the search |
| Every source fails | Empty state that says so and offers retry | A blank pane |
| Magnet resolution fails on `d` | Row stays, banner explains, item is not enqueued | A half-enqueued item |
| Cache read fails or schema drifted | Treat as a miss, re-fetch, overwrite | Propagating a deserialization error |
| Cache write fails | Continue — the cache is an optimisation | Failing the search |
| `HARBOUR_*` env var is garbage | Documented default + a warning | Panicking on parse; silent reinterpretation |
| Theme lock poisoned by a dying watcher | Recover the inner value and keep rendering | Panicking in the render loop |
| Panic anywhere | Hook restores the terminal, writes a crash log | Leaving the user in a wrecked alt-screen |

### 4.3 Coding rules that make the table true

- **No `unwrap`/`expect`/`panic!` on any path reachable from I/O, the engine, or
  the render loop.** Where an invariant genuinely cannot fail, `debug_assert!` plus
  a graceful branch — not a release-mode panic. The one existing precedent I am
  keeping is `Spinner::new`'s assert, which is a pure programming error at
  construction, and even there `set_frames` degrades instead of asserting.
- **Every `catch`-equivalent explains itself.** A silently swallowed error is a
  future bug report with no evidence; log with context, surface what the user can
  act on.
- **Timeouts on everything that touches the network or the swarm.** No unbounded
  await, ever.
- **Persistence is atomic (temp + rename) and flushed synchronously on exit**, so a
  crash leaves either the old file or the new one.
- **Degrade in the direction of the user's data.** If the choice is between losing
  a download and showing a stale row, show the stale row.

### 4.4 On not forcing other tracks

Two items in this plan change code other people own — the `Source` trait
(§2.1) and the `QueueItem` stats split (T7/T8/T9). Neither will be landed as a
break-and-let-them-fix-it:

- I write the migration, not just the contract. The `Source` change is one
  `Box::pin(async move { … })` per adapter, and I will send the diff rather than
  the requirement.
- The stats split ships as **one PR touching both `types.rs` and
  `downloads.rs`**, reviewed with Ishan, so `main` is never red. If that PR is not
  ready in time, `QueueItem` keeps the volatile fields as deprecated pass-throughs
  for one cycle rather than breaking the view — worse code for a week beats a
  broken `main`.
- Where I disagree with a merged decision, I raise it and let the owner decide.
  §3's `Error → failed` mapping contradicts `src/types.rs:206`, and it goes to
  Ishan as a question with the reasoning, not as a silent overwrite.

---

## 5. Phases

**Every phase carries its §4.2 rows as acceptance criteria, not as follow-up.** A
phase is done when its happy path works *and* each failure mode it introduces has a
test proving the fallback. E1 in particular exists to observe real failures before
E2 codes against guessed ones.

### E0 — Foundation & freeze · ~3 days · blocks both other tracks

1. `src/core/types.rs` per §3, rustdoc on every public item, announced frozen.
2. `Engine` trait + in-memory fake (T13).
3. `paths.rs` — config dir (`FR-06`), `HARBOUR_STATE_DIR`, `HARBOUR_MAX_DOWNLOADS`
   (`FR-07`), `HARBOUR_SOURCE_TIMEOUT`.
4. Error type; crash-log and bootguard arm/disarm **hook signatures** (the terminal
   side already exists in `app.rs`).
5. CLI parse: `FR-02`–`FR-05`, infohash validation, magnet builder.
6. Land amendments A-1…A-18 (§6).
7. **Coordinated edit with Ishan:** `src/ui/downloads.rs` reads `item.eta_secs`
   (`:237`), `item.speed_mib` (`:240`), `item.upload_speed_mib` (`:307`),
   `item.peers` (`:233-234`). T7/T8/T9 move those to `EngineStats`, so the view
   switches to `ItemView`. Ships as one PR touching both files, agreed with Ishan;
   if that slips, the fields stay as deprecated pass-throughs for a cycle (§4.4).

**DoD:** four CI gates green on all three OSes; a `Vec<ArcSource>` registry and a
fan-out over it **compile** (the §2.1 regression test); round-trip test for the
ledger including `paused`; infohash/magnet/path unit tests under both
`%USERPROFILE%` and `$HOME`.

### E1 — librqbit behavioural spike + gate · ~2–3 days

Static half done. Remaining, each pass/fail:

1. Magnet → metadata → download → finish → seed.
2. `kill -9` mid-download → relaunch → resumes **with no rehash**.
3. Delete the file under a live seed → **record what `TorrentStats` actually
   reports**. That recording is the specification for the `FR-45` detector; the
   torlink constants (2 polls, 10s grace) were derived for webtorrent's
   re-verification behaviour and must not be copied blind.
4. Re-add from cached `.torrent` against files on disk → verifies locally, no swarm
   metadata fetch (`FR-37`'s load-bearing half).
5. `pause`/`unpause`; `delete(id, delete_files)` with an open handle (Windows
   locking is the risk, not Linux).
6. Measure `Session::new` cost, RSS at 1 and 20 torrents, release binary size →
   these become the numbers behind NFR-13/14.
7. `default-tls` vs `rust-tls` — rustls avoids a system OpenSSL dependency.

**DoD:** written verdict appended to the spike doc; `librqbit = "=8.1.1"` pinned
with a why-comment; go/no-go recorded. **No-go fallback, recorded now:** drive the
`rqbit` binary over its `http-api` feature as a sidecar. That costs the
single-binary property — Ishan's call, not a dead end.

### E2 — Engine + queue · ~2 weeks

- `engine.rs`: `Session` wrapper, add from magnet/infohash/`.torrent`, handle
  registry keyed by info_hash, the §3 projection table.
- **Adaptive cadence:** 500ms while anything is downloading, ~5s when everything is
  a settled seed, completion via `wait_until_completed()` rather than polling for
  `finished`. Keeps `FR-32`'s guarantee; drops the idle cost `NFR-04` budgets.
- `queue.rs`: cap (`FR-31`), oldest-first `promote()`, dedupe by info_hash
  (`FR-56`) — all unit-tested against the fake `Engine`, no network.
- **Magnet resolution on demand** (T4): `d` on a row with `magnet: None` calls
  `Source::resolve_magnet`, shows Ishan's `resolve…` affordance, then enqueues.
- Metadata capture → `cache/torrents/<info_hash>.torrent`, **and the re-seed path
  that consumes it** (`FR-37` is two halves).
- Seed-by-default + trackers override (`FR-42`).
- Missing-file detector, re-derived from E1 item 3.
- Error surfacing (`FR-36`) from add-time failures and `stats.error`.

**DoD:** `HARBOUR_TEST_NET=1` integration covering add → download → seed → pause →
resume → missing; a re-seed that performs no swarm metadata fetch; a lazy-magnet
download from a `None` row; queue semantics fully covered network-free; `NFR-11`
path-safety tests (no path derived from a torrent name).

**Degradation gates (§4.2):** a `Session` that will not construct leaves search
working and downloads reporting why; one torrent that fails to add does not fail a
restore of twenty; a dead magnet times out and explains "no peers found"; a stats
read that throws leaves the poller alive; a seed whose files vanish goes `missing`
with the engine stopped and **nothing re-downloaded**.

### E3 — Search orchestration · ~1 week

Assigned here by both replies. Sources produce; **this layer merges**.

- Fan-out over `Vec<ArcSource>`, one task per source, per-source deadline.
- **Per-phase budget** (Dhruv, `notes-for-dhruv.md:246-255`): list ≈3s, follow-ups
  the remainder, total ≤10s, tunable via `HARBOUR_SOURCE_TIMEOUT`. At 3s the UI
  releases the bar; unanswered sources go `Checking`, **not** `Offline`; late
  arrivals stream in and flip to `Online`.
- **Dedupe + merge** (`FR-25`, now decided as a fix): one list, dedupe by
  `info_hash` keeping the higher seeder count, global sort seeders desc then date.
- **Cache**: TTL read/write, empty-success negative caching, and the **hard-failure
  `failed_at` marker Dhruv assigned to the engine** (`notes-for-dhruv.md:232-238`),
  per *host* not per source, ~60s.
- **Sticky host hint** passed in via `SearchCtx.host_hint` — the engine holds the
  session state so sources stay stateless (`docs/sources.md:37-39`).
- Cancellation on new query (`FR-20`).

**DoD:** with the slowest source blackholed, results render inside the list budget
and the dead source reads `Checking` then `Offline`; a repeat query is a cache read
with no network; a cross-source duplicate collapses to one row with the higher
seeder count (Dhruv is shipping fixtures for exactly this).

**Degradation gates (§4.2):** all ten sources failing yields an empty state that
says so and offers retry, never a blank pane; a cache file with a drifted schema is
treated as a miss and overwritten, never propagated as a parse error; a cache write
failure is invisible to the user because the cache is only ever an optimisation.

### E4 — Persistence + bootguard + resume · ~1 week

- Ledger: durable fields only (T7); atomic temp+rename; debounced; **synchronous
  flush on exit**.
- `history.json` = search queries, cap 500 (`FR-49`). Recently-downloaded is
  **derived from the ledger** (`finished == true`), not a second file — this removes
  the `HistoryItem`/`FR-49` collision where `types.rs:166` models completed
  downloads while citing `FR-53`, which is bootguard.
- `config.toml` (`FR-51`); corrupt-file quarantine (`FR-54`).
- Bootguard: arm at boot, **flush the ledger, then clear the marker**, then restore
  the terminal, then exit. A crash between flush and clear must not leave a clean
  marker over a stale ledger.
- Reconcile ledger against librqbit session state on startup (`FR-50`).

**DoD:** `kill -9` mid-download → relaunch resumes with no rehash; simulated crash →
safe mode with zero engines started; corrupt ledger quarantined and startup
survives; all three OSes.

**Degradation gates (§4.2):** one malformed ledger row is skipped and the other
nineteen load; a read-only or full disk surfaces a banner and keeps the download
running in memory; a corrupt `config.toml` falls back loudly and is **never
overwritten**; a crash between ledger flush and marker clear still boots into safe
mode, not into a clean marker over a stale ledger.

### E5 — Integration · joins both tracks

Swap the real `Engine` behind the E0 trait; own M0 acceptance items 4–8 of
`SPEC.md` §8.

**Track total ≈ 5–6 weeks.** Only E0's three days block anyone else.

---

## 6. Amendments to land in E0

A-13 and A-11(partial) are done. The rest are mine now, not Ishan's queue.

| # | Target | Change |
| --- | --- | --- |
| A-1 | `AGENTS.md:48`, `SPEC.md:147` (FR-30), `SPEC.md:195` (FR-48) | Six statuses. All three, or the ledger cannot legally persist `paused` |
| A-2 | `AGENTS.md:51`, `docs/roadmap.md:32`, `docs/design.md:3` | Crate is `harbour` (matches `Cargo.toml:2`) |
| A-3 | `docs/roadmap.md` §2, §6 | Phase 1 → 1A/1B. Atomically delete the types task at `:57`, the "shared types are stable" DoD clause at `:70`, and "phase 2 pins down first" at `:247` |
| A-4 | `docs/roadmap.md` Phase 4 | Depends on 1A, not Phase 2 |
| A-5 | `SPEC.md` FR-32, FR-44 | peers/ETA absent while not live; UI renders `—`, never `0` |
| A-6 | `SPEC.md` FR-48 | Drop "and progress"; volatile stats never persisted; debounced writes; sync flush on exit |
| A-7 | `SPEC.md` FR-08 / UR-08 | Exit ordering: flush → clear marker → restore → exit |
| A-8 | `SPEC.md` FR-45 | Restate in librqbit terms; constants come from E1 item 3. Acceptance stays behavioural |
| A-9 | `SPEC.md` NFR-03 | ≤100ms p95 to first paint (Ishan agreed, `notes-reply-ishan.md:38-42`) |
| A-10 | `SPEC.md` §7 | NFR-13 idle RSS with 20 seeds; NFR-14 release binary size. Numbers from E1 |
| A-11 | `docs/roadmap.md:5`, `docs/design.md:5` | Point at `docs/context.md` — still citing the uncommitted scratch file |
| A-12 | `SPEC.md` FR-43, OQ-1, UR-10 | `p` is pause-only; delete the "or removed per config" clause. Ishan asked for this explicitly |
| A-13 | `AGENTS.md:49` | Add `HARBOUR_SOURCE_TIMEOUT` |
| **A-14** | `SPEC.md` FR-25 | **New.** Single merged list, dedupe by info_hash keeping higher seeders, global seeder sort. Decided by Ishan (`notes-reply-ishan.md:55-62`), agreed by Dhruv, and the shipped UI already draws a flat list — SPEC is the only holdout |
| **A-15** | `SPEC.md` FR-14, `docs/sources.md:79` | **New.** `magnet` is optional at search time, resolvable on demand |
| **A-16** | `SPEC.md` FR-18 | **New.** Health states are unknown/checking/online/empty/offline — `checking` is currently unrepresentable |
| **A-17** | `SPEC.md` FR-49 | **New.** `history.json` is search queries; recently-downloaded derives from the ledger |
| **A-18** | `docs/roadmap.md:3`, `docs/architecture.md:3`, `docs/design.md:5` | **New.** All three still say "repo has no code yet" |

---

## 7. Obligations inherited from the replies

Every one is now scheduled; this table exists so none silently drops again.

| From | Obligation | Lands in |
| --- | --- | --- |
| Dhruv §1 + Ishan | Optional/resolvable magnet in the freeze + on-demand resolution | T4, E2 |
| Ishan #7 + Dhruv §5 | Cross-source dedupe by info_hash | E3, A-14 |
| Dhruv §3a | Engine-enforced negative TTL on hard failures, per host | E3 |
| Dhruv §4 | Session-scope sticky-host hint (sources stay stateless) | T11, E3 |
| Dhruv §3b + Ishan | Per-phase deadline budget + `Checking` state + `HARBOUR_SOURCE_TIMEOUT` | T10, T11, E3, A-13, A-16 |
| Dhruv §2/§4 | Typed `SourceError` must survive the freeze | T3 |
| Ishan #3 | `finished` flag stays — paused-seed rendering depends on it | T7 |
| Ishan #5 | NFR-03 → 100ms + footprint NFRs | A-9, A-10 |
| Ishan #6 | FR-43 clause removal | A-12 |

---

## 8. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| Dhruv's unpushed adapters were built against the `docs/sources.md` trait and need rework | **High** | Medium | §2.1 makes rework unavoidable regardless — that trait cannot compile. Land E0 fast and hand him the migration inline |
| The T7/T8/T9 stats split breaks `downloads.rs` | **Certain if uncoordinated** | High | One PR touching both, with Ishan |
| librqbit fastresume or the missing-file signal misbehaves | Medium | Project-shaping | E1 gate; sidecar fallback recorded |
| 8→9 lands mid-build | Medium | Medium | Pin `=8.1.1`; the `Engine` trait keeps the blast radius to one module |
| More contract drift while three docs describe one contract | **High** | Medium | E0 makes `src/core/types.rs` normative and the other descriptions explicitly derivative |
| Ledger silently persists volatile stats today and nothing flags it | Certain until E4 | Medium | A-6 + the T7 split; a round-trip test asserting the persisted field set |

---

## 9. Verification log

**Compiled, not assumed.** Both `Source` traits were extracted into a scratch crate
and built: `docs/sources.md`'s `Arc<dyn Source>` and `src/types.rs`'s `impl Future`
variant both fail with `E0038: not dyn compatible` the moment a registry uses them.
The boxed-future replacement in §2.1 was then compiled with a real
`Vec<ArcSource>` and an `await`ing fan-out. librqbit 8.1.1 facts come from
`cargo info`/`Cargo.lock`/a Windows build/reading the vendored source.

**Adversarial review.** A second model (opencode, `deepseek-v4-flash-free`) audited
this plan against the merged tree and both replies, asked only for problems. I
re-verified each claim against the files rather than accepting it. It was right
about all of the following, and this document changed accordingly:

- Three obligations from the replies had **no phase at all** — engine-enforced
  negative TTL, the sticky-host hint, and the deadline-budget/pending-state
  contract. Now E3, T11, T10.
- `SourceStatus` cannot express `FR-18`'s `checking`, so an unanswered source
  renders as unknown forever and Ishan's 3s pending dot is unimplementable → T10.
- `types.rs:166` models completed downloads as `HistoryItem` while citing `FR-53`
  (bootguard); `FR-49`'s `history.json` is *search queries*. Two different cap-500
  objects → A-17, E4.
- `types.rs:206` maps an engine `Error` on a seed to `Missing`, contradicting the
  projection table → resolved explicitly in §3, against the merged file.
- `eta_secs: Option<u64>` and `time_remaining: Option<Duration>` coexist in the
  freeze candidate, so "one unit, converted once" was already violated → T8.
- F-2 is now moot (Ishan merged 1B, the serial barrier is gone) and F-9 is done;
  F-4's evidence line was stale (`Cargo.toml` has nine deps now, still no librqbit).
- Ishan's "tidy batch — done" is contradicted by the tree on two of four items.

It missed the §2.1 dyn-compatibility defect in `docs/sources.md`, which is the
single most consequential thing in this document.

**Still unverified, and marked as such:** every runtime librqbit claim (fastresume,
missing-file signalling, Windows delete-with-open-handle) and every performance
number. Those are E1's gate. Nothing here is presented as measured unless it is in
the spike doc's evidence table.
