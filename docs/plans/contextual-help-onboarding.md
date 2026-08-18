# Contextual help + onboarding
Ref: #64

## Goal
Make harbour teach itself by focus: a one-screen first-launch welcome, empty states that name
the next keystroke, a hint bar that changes with the screen **and the selected row**, and the
existing `?` overlay promoted to a real deep reference — with no tutorial modal anywhere.

## The finding that shapes this whole plan

Read on **2026-08-16** from the harbour source:

**A per-view hint line already exists, and the search view's is already focus-driven.**

```rust
// src/ui/search.rs:45-47
const HINT: &str = "↵ search · tab downloads · s settings · ? help · esc results";
const RESULTS_HINT: &str = "↵ watch now · d download · s settings · ? help · / search · esc input";
// src/ui/search.rs:289, 296
Constraint::Length(1), // hint line
let hint = if state.focus { HINT } else { RESULTS_HINT };
```

`src/ui/downloads.rs:41,135,150` has the same shape with a single static string.
`src/ui/settings.rs:32` has a third. **So this issue is not "add a hint bar" — it is "there are
three unrelated hint strings, make them one selection-aware function".** That is a far smaller,
far safer change than the issue title suggests, and it costs no screen rows.

**Second finding: there is no first-launch signal.**

`Store::load_config()` returns `Loaded::Ok(Config::default())` when `config.toml` is absent —
identical to a clean load of an existing file (`src/persist.rs`, the `Loaded` enum). Nothing
distinguishes "never run" from "ran, changed nothing". A first-launch screen needs an explicit
marker.

**Third finding: `?` is already contractually the reference.**

`UR-10` says "`?` shows exactly these" bindings, and `src/ui/help.rs`'s
`the_bindings_users_reach_for_first_are_present` test enforces it. `help::BINDINGS` is therefore
**already the single source of truth** — so hint strings must be derived from it or checked
against it, never forked. A hint that says `r retry` while `BINDINGS` has no `r` row is exactly
the documentation bug that test exists to prevent.

**Fourth finding: `help.rs` overflows a standard terminal.** `BINDINGS` (19 rows, counted in
`src/ui/help.rs:20-55`) + `READING_RESULTS` (6) + 5 chrome lines = **30 lines**, and `draw`
computes `height = (…).min(area.height)` then renders a `Paragraph` with no scroll. On 80×24
the last ~6 bindings — including `q`, `ctrl+c`, and `?` itself — are **silently cut off**. That
is a present bug in the thing this issue calls "the deep reference".

## SPEC / FR reference

**Exists today.** `UR-10` (`?` shows exactly the implemented bindings), `UR-11` (empty results
show an empty state, never a blank pane), `UR-01` (splash → search → downloads → now-playing),
`UR-13` (the error banner is the single channel for errors).

**Missing from SPEC — add first, then implement.** Proposed **UR-16 / UR-17 / UR-18** in §5:

- **UR-16 (first launch).** On the first launch in a state directory, harbour shows one welcome
  screen after the splash: what harbour is, the six keys that matter, and "press any key".
  Any key dismisses it, permanently, for that state directory. There is no multi-step tour, no
  "next" button, and no way to be shown it twice by accident. `harbour --welcome` re-shows it on
  demand; that is the only way back.
- **UR-17 (contextual hints).** Every screen reserves one hint row naming the actions available
  **for the current selection** — a failed download offers `r retry`, a paused one offers
  `p resume`. Hints are derived from the keybind table (UR-10), so a hint can never name a key
  the app does not implement.
- **UR-18 (empty states name the next action).** Every list that can be empty says what to press
  to make it non-empty. UR-11 requires a non-blank pane; UR-18 requires it be *actionable*.

Note the tension UR-16 resolves: `UR-01` fixes the view order, and the welcome screen is a
one-shot interstitial between splash and search — it is not a new resting view.

## Workstream

**Terminal UI (Ishan)** owns steps 2–5. **Engine & Foundation (Sarthak)** owns the two-method
`Store` addition in step 1 — `src/persist.rs` is foundation code with the atomic-write and
quarantine invariants, and a marker file written carelessly is how a first-run screen becomes an
every-run screen.

Shared-type dependencies: **`QueueStatus`** (read-only, to pick the right hint for the selected
download row). No shared type changes.

**Ordering:** land after **#63**. Step 3 reads #63's `SettingDef.blurb` for the settings screen's
hint; without it the settings hint stays the static string it is today and step 3 lands twice.

## Approach

**Step 1 — the first-launch marker (Engine track, ~50 lines).**

`src/persist.rs` gains two methods next to the existing boot-marker pair (`arm_boot_marker` /
`boot_was_interrupted`) so all marker files live in one place with one set of rules:

```rust
/// True until `mark_welcomed` succeeds. A marker file, not config absence:
/// `Loaded::Ok(Config::default())` is returned both for "no file" and for a
/// clean load, so config cannot tell first launch from an unmodified one.
pub fn needs_welcome(&self) -> bool;
/// Records that the welcome screen was shown. A failed write is a warning,
/// never a panic — the worst case is the screen appearing once more.
pub fn mark_welcomed(&self) -> io::Result<()>;
```

Marker path `<state>/.welcomed`, written through the existing atomic temp-then-rename helper.
`HARBOUR_STATE_DIR` relocates it with everything else, so tests get a fresh first run for free.

**Step 2 — the welcome screen (UI, ~160 lines).**

New `Screen::Welcome` and `src/ui/welcome.rs`. Pure paint, like every other view: title, one
sentence on what harbour is, six keys (`↵ search`, `d download`, `w watch`, `tab downloads`,
`shift+S settings`, `? help`) **pulled from `help::BINDINGS` by key**, and a muted
"press any key to start".

Wiring in `src/app/mod.rs::run`: after `SPLASH_DURATION` elapses, go to `Screen::Welcome` when
`store.needs_welcome()`, otherwise straight to `Screen::Search` as today. In `handle_event`, any
`Event::Key` while on `Screen::Welcome` calls `mark_welcomed()` and moves to `Screen::Search`.

Two things must not regress: the background curated browse search already kicked off at
`run()`'s `InitialAction::None` arm keeps running behind the welcome screen (no first-paint
delay, `NFR-03`), and `--magnet`/`--torrent` startup **skips the welcome entirely** — a user who
launched with work to do gets the work, not a greeting.

`Screen` is matched exhaustively in `ui/status.rs::segments`, `input.rs::map`, and
`app/mod.rs::draw`; all three gain an arm. `cli.rs` gains `--welcome` to force it (UR-16).

**Step 3 — one hint function (UI, ~200 lines).**

New `src/ui/hints.rs`, the single place a hint string is produced:

```rust
/// The hint row for the current screen and selection. Selection-aware:
/// the actions offered depend on what the cursor is on, because a hint that
/// lists actions the selected row cannot perform teaches the wrong thing.
pub fn hint(screen: Screen, state: &AppState) -> Vec<(&'static str, &'static str)>;
```

Returns `(key, verb)` pairs; the view formats them with the theme. Callers:
`ui/search.rs` (replacing `HINT`/`RESULTS_HINT`), `ui/downloads.rs` (replacing `HINT`),
`ui/settings.rs` (feeding #63's hint bar). No new rows are added to any layout — the rows exist.

Selection awareness, concretely:

| Selection | Hint |
| --- | --- |
| downloads, `Failed` | `r retry · x remove · ? help` |
| downloads, `Downloading`/`Queued` | `p pause · x remove · w watch` |
| downloads, `Paused` | `p resume · x remove` |
| downloads, `Seeding` | `p stop seeding · o open folder · x remove` |
| downloads, empty tab | `tab search · press d on a result to download` |
| search, input focus | `↵ search · tab downloads · ? help` |
| search, results focus | `d download · w watch now · shift+D folder · ? help` |
| search, zero results | `type a query · ↵ browse curated` |

**Step 4 — empty states name the key (UI, ~60 lines).**

`ui/search.rs:817` already reads *"no results yet — press Enter to search"* — the pattern exists
and is right. Extend it to the two downloads branches (`ui/downloads.rs:245` active,
`:315` seeding), which currently only say the list is empty:

- active tab empty → *"nothing downloading — press `tab` to search, then `d` on a result"*
- seeding tab empty → *"nothing seeding yet — finished downloads land here"*
- search, query typed, zero results → *"no results for '…' — try fewer words, or `s` to enable
  more sources"* (that last clause is discoverable nowhere else today)

**Step 5 — `?` as a real reference (UI, ~140 lines).**

Fix the silent clipping found above, then make it a reference rather than a list:

1. Group `BINDINGS` under headings (Search · Downloads · Watch · Global) by adding a `section`
   field to the tuple → a 3-tuple, or a small struct. The existing uniqueness tests carry over.
2. Add scrolling: `HelpState { offset }`, `↑/↓`/PgUp/PgDn, plus a `Scrollbar` — the same
   viewport math `ui/downloads.rs` already uses.
3. Footer: *"esc close · ↑/↓ scroll"*, and a line pointing at the scoped settings help from #63.

## Files to create / modify

- `SPEC.md` — UR-16 / UR-17 / UR-18 in §5. **First commit, before any code.**
- `src/persist.rs` — `needs_welcome()` / `mark_welcomed()` (step 1, Engine track).
- `src/ui/welcome.rs` — **new**, the welcome screen.
- `src/ui/hints.rs` — **new**, `hint()` and its `BINDINGS` consistency test.
- `src/ui/mod.rs` — `Screen::Welcome`; `pub mod welcome; pub mod hints;`.
- `src/ui/search.rs` — hint line calls `hints::hint`; the zero-result empty state.
- `src/ui/downloads.rs` — hint line calls `hints::hint`; the two empty states.
- `src/ui/settings.rs` — hint bar calls `hints::hint` (needs #63).
- `src/ui/help.rs` — sections, scrolling, `HelpState`.
- `src/ui/status.rs` — `Screen::Welcome` arm in `segments`.
- `src/app/mod.rs` — welcome routing after the splash; `HelpState` on `App`.
- `src/app/events.rs` — any-key dismiss on `Screen::Welcome`; help scroll keys.
- `src/input.rs` — `Screen::Welcome` arm; `Action::HelpScrollUp`/`Down`.
- `src/cli.rs` — `--welcome`.
- `src/ui/tests.rs` — snapshots.

## Key APIs / libraries

**No new crates.** ratatui **0.30.2** (current — crates.io, checked 2026-08-16) and crossterm
0.29 already provide everything:

- The welcome screen is a `Paragraph` in a `Block`, same construction as `ui/help.rs::draw`.
- Scrolling is `Paragraph::scroll((offset, 0))` plus
  `ratatui::widgets::{Scrollbar, ScrollbarState}` — both already used in `ui/downloads.rs`.
  ratatui 0.30.1 added `Block` shadows and `Fill`
  ([ratatui.rs/highlights/v0301](https://ratatui.rs/highlights/v0301/), checked 2026-08-16);
  neither is needed here and neither should be adopted for a single screen.
- "Dismiss on any key" is `Event::Key` with `KeyEventKind::Press` — the existing filter in
  `app/events.rs` already guards against Windows' duplicate key-release events, which is the
  one way "any key" turns into "dismissed before the user saw it".

**Rejected: `tui-popup` / `tui-widgets`.** A crate to draw a bordered box over a `Clear`, in a
codebase that already does it in six places. Fails the lean-dependency rule.

## Risks / edge cases

- **The welcome screen must never appear twice.** A failed `mark_welcomed()` write is the failure
  mode; it warns (per `NFR-15`) and the screen may appear once more, which is the correct
  degradation. It must never *panic* and must never *block* the transition to search.
- **`--magnet` / `--torrent` skip the welcome.** Otherwise `harbour magnet:?xt=…` on a fresh
  machine greets the user instead of downloading. Assert this in a test.
- **Duplicate key events on Windows.** `KeyEventKind::Release` and repeat events must not
  dismiss. Filter on `Press`.
- **Hint drift is the real long-term risk.** `hints.rs` must ship with a test asserting every key
  it names exists in `help::BINDINGS`. Without that test this issue's value decays the first time
  someone renames a keybind — and `UR-10`'s existing test only covers the overlay, not the hints.
- **Hint width on 80 columns.** The downloads `Seeding` hint above is ~48 cells and the status
  bar is a separate row, so it fits — but `hints::hint` returns pairs precisely so the view can
  drop trailing pairs when the row is narrow (ties into #65). Truncating mid-word is not
  acceptable; drop whole pairs from the right.
- **Rejected: a multi-step tour / tutorial modal.** Named here so it is rejected once, in
  writing. UR-16 fixes it at one screen. A TUI user who pressed a key to launch the app has
  already demonstrated they will press keys; the payoff is a hint bar that is right *later*, not
  a wizard that is in the way *now*.
- **Rejected: hover tooltips.** Same reasoning as #63's UR-15. The codebase tracks `mouse_pos`
  and it would be easy; do not.
- **Rejected: deriving hint strings by parsing `BINDINGS` descriptions.** The descriptions are
  prose ("search results: download the selected row") and parsing them is exactly the regex-
  where-deterministic-code-fits pattern the project forbids. Hints are written explicitly and
  *checked against* `BINDINGS` by key.
- **`help.rs` clipping is a pre-existing bug, not a regression this introduces.** Fix it in
  step 5 and note it in the PR — otherwise "the help overlay is fine on my 50-row terminal"
  closes the issue with the bug still shipped.

## Test strategy

- **Unit, `src/persist.rs`** — against a temp `HARBOUR_STATE_DIR`: `needs_welcome()` is true on
  a fresh root, false after `mark_welcomed()`, and false across a re-constructed `Store`
  (it is a file, not memory).
- **Unit, `src/ui/hints.rs`** — the consistency test: for every `(key, _)` returned by `hint()`
  across every `Screen` × every `QueueStatus` × both search focus states, `key` appears in
  `help::BINDINGS`. This is the test that keeps UR-17 true a year from now.
- **Unit, `src/ui/hints.rs`** — a `Failed` selection yields a hint containing `r`; a `Seeding`
  selection does not; an empty tab yields the "press d" form.
- **Unit, `src/app/events.rs`** — a `Press` on `Screen::Welcome` advances to `Screen::Search`;
  a `Release` does not.
- **Buffer snapshot, `src/ui/tests.rs`** — the welcome screen at 80×24 renders the product name
  and at least four of the six keys, with nothing clipped at the border.
- **Buffer snapshot** — downloads with an empty active list contains the string `press d`;
  search with a query and zero results names both "fewer words" and `s`.
- **Buffer snapshot, help** — at 80×24 with `offset` at maximum, `ctrl+c` is visible. That is
  the regression test for the clipping bug, and it fails on `main` today.

## Verification

1. `HARBOUR_STATE_DIR=$(mktemp -d) cargo run` → splash, then the welcome screen. Press any key →
   search. Quit and re-run with the **same** `HARBOUR_STATE_DIR` → no welcome. That round trip is
   UR-16 proven end to end.
2. `HARBOUR_STATE_DIR=$(mktemp -d) cargo run -- "magnet:?xt=urn:btih:…"` → **no welcome**; the
   magnet enqueues.
3. `cargo run` → `tab` to downloads with an empty queue. The pane reads *"nothing downloading —
   press `tab` to search, then `d` on a result"*, not a blank box.
4. Download something, let it fail (unplug the network or use a dead magnet), select it. The
   hint row changes to include `r retry`. Select a healthy downloading row — the `r` disappears.
   **That change, live, is the observable proof of UR-17**, and nothing on `main` does it.
5. `cargo run` in an **80×24** terminal → `?` → scroll to the bottom. `ctrl+c` is listed. On
   `main` it is not.
6. `cargo run -- --welcome` re-shows the welcome on an already-welcomed state dir.
