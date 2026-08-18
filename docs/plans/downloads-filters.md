# Status / tracker / infohash filters
Ref: #62

## Goal
Let the downloads list be narrowed by status, by tracker, and by infohash/name substring —
through the one seam that already keeps the render, the selection, and the key dispatch in
agreement.

## The findings that shape this plan

Read on **2026-08-16** from harbour's source and the vendored librqbit 8.1.1.

**1. The filter seam already exists.** `App::visible_items()` (`src/app/mod.rs:179-190`) is the
single function that decides which items the downloads screen shows, and
`App::selected_item_id()` (169-176) walks the *same* list — with the comment explaining exactly
why: "so the selection never points at a row hidden on the other tab". **Every filter in this
issue belongs inside `visible_items`.** Filtering anywhere else re-introduces the bug that
comment was written to prevent.

**2. The status vocabulary in the issue is qBittorrent's, not harbour's.** `AGENTS.md` fixes six
normative statuses — `queued`, `downloading`, `paused`, `failed`, `seeding`, `missing` — and
`QueueStatus` (`src/core/types.rs:348-361`) is frozen at those six. The issue asks for
`all / downloading / seeding / completed / inactive / error`. Three of those six are **not**
statuses in harbour: `completed` is `QueueItem.finished`, `inactive` is a *speed* property, and
`error` spans `Failed` and possibly `Missing`. The mapping must be defined in SPEC before a
single filter is written, or two people will implement two different "inactive".

**3. `show_seeding` is already a filter and must not become a second one.**
`DownloadsState.show_seeding` (`src/ui/mod.rs:166`) splits active from finished via the
`Seeding` tab, and `visible_items` implements it as `finished == show_seeding` where `finished`
means `Seeding || Missing`. A status filter layered on top without reconciling this gives
contradictory states ("Seeding tab + downloading filter" = always empty).

**4. Per-tracker *status* is not available in librqbit 8.1.1.** `TorrentStats`
(`torrent_state/stats.rs:70-79`) carries `state`, `file_progress`, `error`, `progress_bytes`,
`uploaded_bytes`, `total_bytes`, `finished`, `live` — **no tracker field**. `LiveStats`
(`stats.rs:9-15`) is speeds, ETA, and a snapshot — also none. There is no announce result, no
per-tracker message, no "working / not working / updating". qBittorrent's tracker-status column
has no equivalent to read.

**5. Tracker *URLs* are available two ways.** `ManagedTorrentShared.trackers: HashSet<url::Url>`
is public (`torrent_state/mod.rs:191`) and reachable via `ManagedTorrent::shared()`
(`mod.rs:224`), but reaching it means either an additive `Engine` trait method (Sarthak's track)
or a downcast, and `FakeEngine` would have to fake it. **The client already holds the same
information**: `QueueItem.magnet` carries the `tr=` announce params, and `Config.trackers`
(`src/persist.rs:48-49`) is appended to every torrent via `AddRequest.trackers`. Parsing `tr=`
is deterministic, testable, engine-independent, and pure UI-track work.

**6. There is no filter/search input on the downloads screen today.** `src/input.rs:361-369`
maps only navigation, `?`, `q`, and Tab on `Screen::Downloads`. The infohash filter needs a
typing mode, and the existing inline-edit idiom (`FolderPrompt`, `SettingsState.edit_buffer`)
is the pattern to copy rather than invent a third one.

## SPEC / FR reference

**Exists today.** §4.4 FR-29…FR-41 (downloads), §4.5 FR-42…FR-47 (seeding), UR-* for the
downloads view. FR-45 is load-bearing here: `Missing` means "the files are gone from disk" and is
reachable **only** from the file-gone detector — it is deliberately not an error state.

**Nothing in SPEC covers filtering the downloads list**, and the issue's status names do not
exist in harbour's vocabulary. Both need specifying.

> **FR numbers here are proposed, not reserved.** Several plans in `docs/plans/` were drafted
> in parallel against the same free block (FR-86+), so numbers collide across files. Allocate
> final numbers when the SPEC edit lands (first merged wins, renumber the rest). The
> requirement *text* is the deliverable; the number is bookkeeping.

**Missing from SPEC — add first, then implement.** Proposed **FR-107 … FR-111** in §4.4:

- **FR-107 (the filter vocabulary, normative mapping).** The downloads screen offers these
  filters, defined against the six normative statuses so there is exactly one reading:

  | Filter | Definition |
  | --- | --- |
  | `all` | every item |
  | `downloading` | `status == Downloading` |
  | `seeding` | `status == Seeding` |
  | `completed` | `item.finished == true` (covers `Seeding`, a paused seed, and `Missing`) |
  | `inactive` | not transferring: `Queued`, `Paused`, or a `Downloading`/`Seeding` item whose live down **and** up speed are both zero |
  | `error` | `status == Failed`. **`Missing` is excluded** — FR-45 says missing files are not an engine error, and folding them in would tell a user their data vanished because a tracker hiccuped. `Missing` shows under `completed`. |

- **FR-108 (one filter, one list).** The active filter is applied in the single function that
  builds the visible list, so the rendered rows, the selection, and every keybind operate on
  the same set. A filter never leaves the cursor on a hidden row.
- **FR-109 (the filter is always visible).** The active filter and the visible/total count
  (`showing 3 of 27`) are shown in the downloads header. A list that is silently short is a bug
  report waiting to happen.
- **FR-110 (tracker filter).** Items can be narrowed to one announce host. The host set is
  derived from each item's own announce URLs; an item with none appears under `no tracker`.
  harbour reports announce **hosts**, not announce health — per-tracker status is not available
  from the engine (see Risks).
- **FR-111 (infohash / name filter).** A typed filter matches case-insensitively against the
  item's infohash prefix **and** its name, so the same box serves "find this hash" and "find
  this show". An empty filter matches everything. Clearing it restores the full list.

Add `f` (cycle status filter), `t` (tracker filter), and `/` (infohash/name filter) to the
keybind table.

## Workstream

**Terminal UI (Ishan)** owns all of it. Every input is already in `QueueItem` / `ItemView` /
`Config`; nothing crosses into the engine.

**Shared-type dependency: none, and that is the point.** No new `QueueStatus` variant, no
`QueueItem` field, no `Engine` trait method. The optional tracker-health step is the only part
that would touch Sarthak's surface, and it is explicitly deferred.

## Approach

**Step 1 — SPEC (docs only).** FR-107…FR-111 into §4.4, including the mapping table verbatim —
that table is the actual deliverable of this step. ~40 lines.

**Step 2 — the filter type and predicate (UI track, pure).** In `src/ui/downloads.rs`:

```rust
pub enum StatusFilter { All, Downloading, Seeding, Completed, Inactive, Error }
pub struct DownloadFilter {
    pub status: StatusFilter,
    pub tracker: Option<String>,  // announce host, or the "no tracker" sentinel
    pub text: String,             // infohash prefix or name substring
}
impl DownloadFilter { pub fn accepts(&self, view: &ItemView) -> bool { … } }
```

Pure over `&ItemView`, unit-testable without a terminal or an engine, and it encodes FR-107's
table in exactly one place. ~120 lines with tests.

**Step 3 — announce hosts (UI track, pure).** `fn announce_hosts(item: &QueueItem,
extra: &[String]) -> Vec<String>` — scan `magnet` for `tr=` params, percent-decode, take the
host, lowercase it, dedupe, and append `Config.trackers`' hosts. Lives next to
`crate::core::magnet`'s existing helpers (`info_hash_from_magnet`, `build_magnet`,
`src/core/magnet.rs`) and follows their hand-rolled style. An item with `magnet: None` (a
`.torrent`-file add, `QueueItem.bytes`) yields an empty list → the `no tracker` bucket. ~90 lines.

**Step 4 — thread it through the one seam (UI track).** `DownloadsState.filter: DownloadFilter`;
`visible_items()` gains `.filter(|v| self.state.downloads.filter.accepts(v))`. Reconcile with
`show_seeding` explicitly: the tab stays the outer split and the status filter applies within it
— **or**, cleaner, the `Seeding` tab becomes `StatusFilter::Completed` and `show_seeding` is
retired. Pick one in the PR and write the reason in the code; do not ship both meanings. After
filtering, clamp `selected` the way `refresh_downloads` (`src/app/mod.rs:193-201`) already does,
so a filter that shrinks the list never strands the cursor. ~60 lines.

**Step 5 — the header (UI track).** FR-109's `filter: downloading · showing 3 of 27` line in
`src/ui/downloads.rs`'s tab row, plus the tracker name when one is set. Pure paint. ~50 lines.

**Step 6 — keys (UI track).** `f` cycles `StatusFilter`; `t` cycles the tracker set derived from
the current items (plus `all` and `no tracker`); `/` enters the text filter using the existing
`edit_buffer` idiom — Enter commits, Esc clears and exits. Add the three `Action` variants and
the `Screen::Downloads` arms in `src/input.rs:361-369`. ~80 lines.

**Step 7 (OPTIONAL, engine track, deferred) — real tracker data.** If tracker *hostnames from
the engine* (rather than from the magnet) are wanted — which is the only way to cover
`.torrent`-file adds — the shape is an additive `Engine` trait method with a default:

```rust
fn trackers<'a>(&'a self, _id: &'a str) -> EngineFuture<'a, Vec<String>> {
    Box::pin(async move { Vec::new() })
}
```

implemented in `RqbitEngine` over `handle.shared().trackers`, defaulting to empty in
`FakeEngine` — the same additive-default pattern `stream_url` and `add_bytes` already use
(`src/core/types.rs:673-718`). That is Sarthak's call and a separate PR. **Tracker *health*
remains impossible; see Risks.**

## Files to create / modify

- `SPEC.md` — FR-107…FR-111 in §4.4 (with the mapping table); keybind table gains `f`, `t`, `/`.
- `src/ui/downloads.rs` — `StatusFilter`, `DownloadFilter`, `accepts`, the header line.
- `src/core/magnet.rs` — `announce_hosts` / a `tr=` param extractor, next to the existing
  magnet helpers.
- `src/ui/mod.rs` — `DownloadsState.filter`, `DownloadsState.filter_editing`,
  `DownloadsState.filter_buffer`.
- `src/app/mod.rs` — `visible_items()` applies the filter; the `show_seeding` reconciliation.
- `src/input.rs` — `f` / `t` / `/` on `Screen::Downloads`, and the typing sub-mode.
- `src/app/actions.rs` — the `FilterCycle` / `FilterTracker` / `FilterType` arms.
- `src/ui/help.rs` — the three new keys.
- `src/ui/tests.rs` — buffer snapshots.

**Deliberately not created:** a `QueueStatus::Completed` or `QueueStatus::Inactive` variant.
Both are *derived* properties (`finished`, speed) and adding either would break the AGENTS.md
vocabulary and every persisted ledger.

## Key APIs / libraries

- **`ItemView`** (`src/core/types.rs:477-529`) already exposes everything the predicate needs:
  `progress()`, `speed_mib()`, `upload_speed_mib()`, plus `item.status` and `item.finished`.
  `speed_mib()` returns `0.0` when `stats` is `None`, which is correct for FR-107's `inactive`:
  an item the engine has no stats for is not transferring.
- **`QueueItem.magnet`** — the `tr=` source. Magnets carry announce URLs percent-encoded and
  repeated (`&tr=…&tr=…`); harbour's existing `urlencode`/`info_hash_from_magnet`
  (`src/core/magnet.rs:74`) are the style to match.
- **librqbit 8.1.1** — `ManagedTorrentShared.trackers: HashSet<url::Url>` is public
  (`torrent_state/mod.rs:191`), reachable via `ManagedTorrent::shared()` (`mod.rs:224`), and is
  step 7's source. `TorrentStats` (`stats.rs:70-79`) and `LiveStats` (`stats.rs:9-15`) carry
  **no** tracker information — verified by reading both structs on 2026-08-16.

**New crates: none.** Everything is a predicate over data harbour already holds.

## Risks / edge cases

- **Tracker *health* is not implementable against librqbit 8.1.1 and the plan must say so.**
  There is no per-tracker announce result anywhere in the public API. Shipping a
  "tracker status: working / not working" column would be a control reporting a state harbour
  cannot observe — the same trap `docs/plans/sequential-download.md` rejected for the sequential
  toggle. FR-110 therefore promises **hosts**, not health, and the honest follow-up is an
  upstream issue on `ikatson/rqbit` asking for per-tracker announce state on `TorrentStats`,
  linked from FR-110.
- **`.torrent`-file adds have `magnet: None`.** They land in `no tracker` even though they have
  trackers. Stated as a known limitation in FR-110; step 7 is the fix, and it is engine-track
  work, not something to fake in the UI.
- **The `show_seeding` collision is the most likely real bug.** `visible_items` currently
  hard-splits on `finished == show_seeding`. Adding a status filter without deciding which of
  the two wins produces silently-empty screens. Decide in step 4, comment it at the decision
  site, and add a test asserting the combination that used to be empty.
- **A filter that hides everything must look deliberate.** An empty filtered list needs its own
  empty state ("no items match `downloading`, press f to change") — not the same blank panel as
  a genuinely empty queue. This is the FR-109 count line doing its job.
- **Selection must be clamped after every filter change**, exactly as `refresh_downloads` does
  on removal, or `selected_item_id()` returns a hidden item and `p`/`x`/`w` act on the wrong
  torrent. This is the single highest-severity bug available in this issue.
- **`inactive` on a stalled download.** A `Downloading` item at 0 B/s is genuinely inactive and
  is the main reason anyone wants this filter, so FR-107 includes it deliberately rather than
  restricting `inactive` to `Paused`/`Queued`.
- **Don't fold `Missing` into `error`.** FR-45 exists precisely to keep "your files are gone"
  distinct from "a transfer failed", and `project_status`' doc comment
  (`src/core/types.rs:578-584`) says so at the decision site. The mapping table honors it.

## Test strategy

- **Unit, `src/ui/downloads.rs`** — `DownloadFilter::accepts` against a fixture set covering all
  six statuses × finished/unfinished × zero/non-zero speed. One assertion per row of FR-107's
  table, so the table and the code cannot drift. Explicitly: a `Missing` item matches
  `completed` and **not** `error`; a `Downloading` item at 0.0 MiB/s matches `inactive`; a
  `Seeding` item uploading at 1.0 MiB/s does not.
- **Unit, `src/core/magnet.rs`** — `announce_hosts`: a magnet with two `tr=` params yields both
  hosts, lowercased and deduped; a percent-encoded `udp%3A%2F%2F…` decodes; a magnet with no
  `tr=` yields empty; `magnet: None` yields empty; `Config.trackers` hosts are appended.
- **Unit, selection clamping** — build a `DownloadsState` with 10 items, select index 8, apply a
  filter matching 2, and assert `selected <= 1`. This is the regression guard for the
  act-on-the-wrong-torrent bug.
- **Buffer snapshot, `src/ui/tests.rs`** — the header renders `filter: downloading · showing 3
  of 27`; the filter-typing mode renders the buffer with the cursor glyph; a filter matching
  nothing renders the distinct empty state, not the empty-queue one.
- **No engine tests.** Nothing in this plan calls the engine; that is why it is UI-track work.

## Verification

1. `SPEC.md` §4.4 contains FR-107…FR-111 including the mapping table, and the keybind table
   lists `f`, `t`, `/`.
2. `cargo run` with a mixed queue (one downloading, one paused, one seeding, one failed). Press
   `f` repeatedly: the header cycles through all six filters and the row count changes to match.
   `completed` shows the seed; `error` shows the failed item and **not** any `missing` one.
3. With `downloading` active, move the cursor to the last visible row and press `p`. **The item
   that pauses is the one under the cursor** — the observable proof that selection and filter
   agree. (Do this before and after applying a filter; getting it wrong pauses a hidden torrent,
   which is the whole reason FR-108 exists.)
4. Press `t` and pick a tracker host. Only items whose magnet announces that host remain; a
   `.torrent`-file-added item appears under `no tracker`, matching the documented limitation.
5. Press `/`, type the first 6 characters of an infohash, Enter → exactly that item. Clear it →
   the full list returns and the cursor is still on a real row.
6. Apply a filter that matches nothing: the panel shows "no items match…", not a blank body and
   not the empty-queue message.
7. `grep -n "Completed\|Inactive" src/core/types.rs` returns nothing — `QueueStatus` is still the
   six normative values and every existing ledger still loads.
