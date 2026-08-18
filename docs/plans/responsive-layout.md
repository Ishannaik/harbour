# Responsive layout
Ref: #65

## Goal
Make every harbour view usable from an 80×24 terminal to a 240×80 one: shared breakpoints,
one clamped-panel helper instead of six hand-rolled ones, and progressive disclosure that drops
chrome before it drops content.

## The SPEC collision — decide it here, in writing

`UR-12` today:

> **UR-12** Layout is responsive: resizing the terminal re-flows panels without panics or
> clipped rendering; minimum supported size is 80×24 (below that, a resize hint banner).

The issue says "usable small to large terminals". Those are two different promises, and SPEC is
the referee, so this plan picks one and says so:

**Keep the 80×24 floor.** Below it, `src/ui/status.rs::needs_resize_hint` already swaps the
status context for *"terminal too small — need at least 80x24"* and that stays. #65 is therefore
**graceful degradation above the floor** plus a **hardened no-panic, no-clip guarantee below it**
— not a new sub-80 layout.

Rationale, and it is not just conservatism: the search view needs a 22-cell sidebar
(`SIDEBAR_WIDTH`) plus `SUFFIX_TOTAL_W` (size 9 + seeds 6 + leech 5 + quality 8 + source 10 +
separators ≈ 43) before a single character of a torrent name is visible. A genuinely usable
60-column search view is a **different view**, not a reflow, and inventing one under a
"responsive layout" issue is scope creep that would land untested. If someone wants it, it is
its own issue with its own SPEC line.

**Missing from SPEC — add first, then implement.** Amend **UR-12** and add **UR-19**:

- **UR-12 (amended).** …minimum supported size is 80×24. Below that, the resize hint shows and
  **rendering must still complete without panic and without writing outside the frame** — the
  app stays alive and recovers on resize.
- **UR-19 (progressive disclosure).** Above the floor, views shed chrome in a fixed order as
  space shrinks — decorative first, navigational second, content last. The order is: status-bar
  tab buttons → inline setting blurbs → the source sidebar → optional result columns
  (quality, then leechers, then source). Content rows and the selection are never dropped.

UR-19's ordering is the whole feature. Without it "responsive" means every view improvises, which
is what produces the current inconsistency.

## The findings that shape this plan

Read on **2026-08-16** from the harbour source.

**1. Six independent centred-panel implementations, each with its own clamp.**

| Site | Preferred width | Clamp |
| --- | --- | --- |
| `ui/settings.rs:202 panel_rect` | `PANEL_WIDTH = 78` | `.min(area.width - 2).max(30)` then `.min(area.width)` |
| `ui/help.rs:74-87` | computed from longest row | `.clamp(30, area.width - 4).max(30)` |
| `ui/status.rs:408 draw_folder_prompt` | `62` | `.min(area.width - 4).max(30)` |
| `ui/player.rs` | own const | own |
| `ui/episode_picker.rs` | own const | own |
| `ui/batch_picker.rs` | own const | own |

Four different fudge factors (`-2`, `-4`, `.max(30)` before vs after the `min`). The settings one
is subtly wrong: at `area.width = 20`, `20-2 = 18`, `.max(30) = 30`, so `width` is 30 — wider than
the terminal — and only the trailing `.min(area.width)` on the struct field saves it, *after*
the centring `x` was computed from the too-large 30. The panel ends up flush-left rather than
centred. It does not panic, but it is the class of arithmetic that eventually does.

**2. `ui/settings.rs` has no scroll offset** — `draw` renders rows `0..visible` unconditionally
(`:242`). At 24 rows the last of 27 settings is unreachable. **This is fixed in #63**, and #65
depends on that fix rather than duplicating it.

**3. The status bar already has one breakpoint, hard-coded inline.**

```rust
// src/ui/status.rs:161
if total_width < TOTAL_BUTTONS_WIDTH + 4 { return Vec::new(); }  // 55 + 4
```

Correct behaviour, wrong home. It is the only progressive-disclosure rule in the codebase and it
lives in a hit-test helper.

**4. Resize needs no explicit handling** (`src/app/events.rs:179`) — ratatui re-lays out from the
frame size each draw. True, and it means #65 is entirely about the *math*, not about plumbing a
resize event.

## Workstream

**Terminal UI (Ishan)** owns all of it. No engine or indexer surface is touched.

Shared-type dependencies: **none.** `src/ui/layout.rs` is view-local and must not migrate to
`core::types` — the engine and the sources have no opinion about terminal width, exactly as
`Screen` is documented not to (`ui/mod.rs:23`).

**Ordering:** land **after #63 and #64.** #65 consumes #63's settings scroll offset and #64's
`hints::hint` pair list (dropping trailing hint pairs on a narrow row is a UR-19 rule). Landing
#65 first means rewriting the same three files twice.

## Approach

**Step 1 — `src/ui/layout.rs`: the shared vocabulary (~140 lines, no behaviour change).**

```rust
/// Terminal-size breakpoints. Named here, in one place, so a view and its
/// hit-test helper can never disagree about when a panel is "narrow" — the
/// same reason `row_kind`/`row_label` are shared in the settings view.
pub const MIN_WIDTH:  u16 = 80;   // moved from ui::status (UR-12)
pub const MIN_HEIGHT: u16 = 24;
/// Below this the source sidebar is hidden (22 cells of sidebar + ~43 cells
/// of result suffix leaves nothing for a torrent name).
pub const NARROW_WIDTH: u16 = 100;
/// Below this, two-line settings rows collapse to one and the inline blurbs
/// are suppressed (#63).
pub const SHORT_HEIGHT: u16 = 30;

pub fn is_narrow(width: u16) -> bool;
pub fn is_short(height: u16) -> bool;

/// A centred modal panel, clamped to `area` on both axes. The single clamp:
/// six views had six variants of this arithmetic, differing by whether the
/// margin was subtracted before or after the floor — one of them centred
/// wrongly on tiny terminals as a result.
pub fn centered_panel(area: Rect, preferred_w: u16, preferred_h: u16) -> Rect;
```

`centered_panel` clamps *first*, then centres, and never returns a rect extending past `area`:

```
w = preferred_w.min(area.width.saturating_sub(2)).max(1).min(area.width)
h = preferred_h.min(area.height).max(1)
x = area.x + (area.width  - w) / 2
y = area.y + (area.height - h) / 2
```

`ui::status::{MIN_WIDTH, MIN_HEIGHT, needs_resize_hint}` re-export from here so the existing
tests (`resize_hint_triggers_below_the_minimum`) keep passing untouched — proof the move is
behaviour-preserving.

**Step 2 — migrate the six panels (~120 lines, mechanical).**

`ui/settings.rs::panel_rect`, `ui/help.rs`, `ui/status.rs::draw_folder_prompt`, `ui/player.rs`,
`ui/episode_picker.rs`, `ui/batch_picker.rs` each call `centered_panel`. Each keeps its own
*preferred* size const (`PANEL_WIDTH = 78` stays where it is and stays public — it is the
settings view's design decision); only the clamp is shared. This is the "existing PANEL_WIDTH
pattern" the issue names: keep the pattern, unify the clamping.

**Step 3 — status bar breakpoints move into the vocabulary (~40 lines).**

`status_button_ranges`' inline `TOTAL_BUTTONS_WIDTH + 4` becomes a named
`layout::shows_status_buttons(width)`. The mid-tier gets a real answer instead of all-or-nothing:
below the full-button threshold, render **glyph-only** buttons (`[🔍] [⬇] [⚙] [?]`, ~20 cells)
before dropping them entirely. `status_button_at` must derive its ranges from the same function —
that is the existing bug shape in this file, and a test pins hit-test/render agreement at every
width from 40 to 200.

**Step 4 — search view: sidebar collapse (~110 lines).**

`ui/search.rs:158` splits `[Length(SIDEBAR_WIDTH), Min(0)]`. When `is_narrow(area.width)`, drop
the sidebar constraint entirely and give the results the full width. Source toggling stays fully
reachable — it is duplicated in the settings overlay's `Sources` category (#63), which is exactly
why UR-19 puts the sidebar *before* content in the drop order.

Column shedding at the same threshold, in UR-19 order: quality (`COL_QUAL_W = 8`) → leechers
(`COL_LEECH_W = 5`) → source (`COL_SOURCE_W = 10`). The header row, the sort hit-testing
(`f3268b1` added clickable column sorting) and the row renderer all read the *same*
`visible_columns(width) -> &[Column]` function — the `row_kind`/`row_label` pattern applied to
columns, so a hidden column can never still be clickable.

**Step 5 — settings + downloads short-terminal behaviour (~80 lines).**

Settings: `is_short(height)` suppresses #63's inline blurbs and collapses rows to one line. The
hint bar survives (it is the fallback explanation channel). #63's scroll offset does the rest.

Downloads: `recent_h` (the recently-downloaded strip at `ui/downloads.rs:236`) collapses to zero
when short, and the hint row drops trailing `hints::hint` pairs when narrow.

**Step 6 — the sub-floor hardening pass (~60 lines).**

Every `saturating_sub` on a width/height in a view gets a test at 20×5, 1×1, and 0×0. ratatui
clips writes outside the buffer, so the realistic failure is a division by zero or a `Rect` with
`x > area.right()` producing an invisible panel — not a segfault. The goal is *provably* no
panic, which is a test suite, not a code change.

## Files to create / modify

- `SPEC.md` — UR-12 amended, UR-19 added, in §5. **First commit, before any code.**
- `src/ui/layout.rs` — **new**: breakpoints, `is_narrow`/`is_short`, `centered_panel`,
  `shows_status_buttons`.
- `src/ui/mod.rs` — `pub mod layout;`.
- `src/ui/status.rs` — re-export the minimums from `layout`; `status_button_ranges` /
  `status_button_at` share one range source; glyph-only button tier;
  `draw_folder_prompt` → `centered_panel`.
- `src/ui/search.rs` — sidebar collapse; `visible_columns(width)` shared by the header, the row
  renderer and the sort hit-test.
- `src/ui/downloads.rs` — `recent_h` collapse; hint-pair dropping.
- `src/ui/settings.rs` — `panel_rect` → `centered_panel`; `is_short` gates blurbs (needs #63).
- `src/ui/help.rs`, `src/ui/player.rs`, `src/ui/episode_picker.rs`, `src/ui/batch_picker.rs` —
  `centered_panel`.
- `src/app/mod.rs` — `mouse_view_area` and `status_height` read the shared constants so
  hit-testing and layout cannot drift (the module comment at `:675` already warns that
  under-allocating a single row here made the safe-mode banner invisible — that warning is the
  reason this file is in scope).
- `src/ui/tests.rs` — the size matrix.

## Key APIs / libraries

**No new crates.** ratatui **0.30.2** (current — crates.io, checked 2026-08-16).

- `ratatui::layout::{Layout, Constraint, Rect, Flex}`. Constraints already do the reflowing;
  #65 is about *which* constraints get built, not about a new layout engine.
- `Constraint::Fill(n)` and `Layout::flex(Flex::…)` are available in 0.30 and are the idiomatic
  way to express "this column takes the remainder"
  ([ratatui.rs/highlights/v030](https://ratatui.rs/highlights/v030/), checked 2026-08-16). Use
  `Fill` for the results column; do **not** rewrite the working `Length`/`Min` splits that are
  not changing — a layout rewrite is not a responsive-layout feature.
- `ratatui::backend::TestBackend::new(w, h)` is the entire test strategy: it takes an arbitrary
  size, so every breakpoint is testable without a terminal.
- **Rejected: a `Frame::area()`-driven global "layout mode" enum threaded through every view.**
  It sounds tidy and it centralises the decision, but it makes every view's signature depend on
  a type that changes whenever a breakpoint is added, and views already receive their `Rect`.
  Pure functions on `area.width` keep views pure paint.

## Risks / edge cases

- **Hit-test/render drift is the #1 risk and the codebase has already been bitten by it.**
  Mouse hit-testing (`status_button_at`, the sort-column click handler in `search.rs`, the
  settings row hover in `setting_line`) computes positions *separately* from the renderer. Every
  breakpoint added here must be consumed by both sides through one shared function, and tested
  by asserting agreement across a width sweep. A hidden-but-clickable column is worse than no
  responsiveness at all.
- **`app/mod.rs::status_height` must keep matching `ui/status.rs::draw`'s own split.** The
  existing comment at `:675` documents that under-allocating one row silently swallowed the
  safe-mode warning. Any change to status-bar height here re-opens that.
- **The settings panel is unusable at 24 rows without #63's scroll.** Do not merge step 5 before
  #63. Stated as a hard gate, not a preference.
- **Wide-glyph truncation.** `truncate`/`truncate_to` count `chars`, not display cells; the
  status bar's `[ 🔍 Search ]` is already a 2-cell emoji counted as 1. Narrowing thresholds makes
  the off-by-one more likely to bite. The glyph-only button tier must be measured in **cells**;
  `unicode-width` is already in the lockfile (transitively, via ratatui) — if a direct dependency
  becomes necessary it is zero new crates, but prefer sizing the glyph tier conservatively and
  avoiding the dependency.
- **Rejected: hiding the status bar on short terminals.** It carries the resize hint. Hiding the
  thing that explains why the layout looks wrong is self-defeating.
- **Rejected: a sub-80-column search layout.** See the SPEC decision above. It is a different
  view and belongs to its own issue.
- **Rejected: reflow-on-resize animation / debounce.** ratatui re-lays out per frame at 30fps and
  `app/events.rs:179` documents that no resize handling is needed. Adding one would be state for
  a problem that does not exist.

## Test strategy

- **Unit, `src/ui/layout.rs`** — `centered_panel` never returns a rect exceeding `area` for a
  sweep of `area` sizes (0×0 … 300×100) × preferred sizes (1 … 300); the returned rect is centred
  when it fits and flush when it does not; `is_narrow`/`is_short` are exact at their boundaries.
  This is the test that would have caught the settings off-centre clamp.
- **Unit, `src/ui/status.rs`** — for every width in `40..=200`: `status_button_at(col, w)` returns
  `Some(tab)` exactly for the columns `status_button_ranges(w)` renders, and `None` everywhere
  else. Same sweep for the glyph-only tier.
- **Unit, `src/ui/search.rs`** — `visible_columns(w)` sheds in UR-19 order; the sort hit-test maps
  a click only to a column `visible_columns` returned.
- **Buffer snapshot matrix, `src/ui/tests.rs`** — every view at **80×24, 100×30, 120×40, 200×60**,
  plus **60×20** (below the floor). Assertions per size: no panic; the resize hint appears only
  below the floor; the sidebar is absent at 80×24 and present at 120×40; the selected row is
  visible in every case.
- **Panic sweep** — `draw` for each of the seven views at 1×1, 2×1, 20×5, 79×23. Assert
  completion, not content. Cheap, and it is the literal text of amended UR-12.
- **No engine tests.** Nothing in this issue touches `HARBOUR_TEST_NET` territory.

## Verification

1. `cargo run` in a maximised terminal. Drag-resize the window narrower, slowly, in one motion.
   Observed in order: the status tab buttons collapse to glyphs, then vanish; the settings blurbs
   collapse; the source sidebar disappears and results widen; quality, then leechers, then source
   columns drop. **No flicker, no panic, no clipped border at any width.** That single continuous
   drag is the verification — it is the thing a user does and the thing that is broken today.
2. Resize below 80×24 → the status bar reads *"terminal too small — need at least 80x24"*.
   Resize back up → the full layout returns with the selection where it was.
3. At 100 columns with the sidebar hidden, `shift+S` → the `Sources` category still toggles every
   source. Nothing became unreachable, which is UR-19's content-last promise.
4. At 100 columns with the source column hidden, click where the source column header used to be.
   Nothing sorts. (The hit-test/render agreement check, by hand, once.)
5. `cargo test` — the size matrix and the panic sweep pass.
