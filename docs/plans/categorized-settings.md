# Categorized settings UI
Ref: #63

## Goal
Reorganize the flat 27-row settings overlay into named categories with per-row explanations —
inline muted blurbs, a focus-driven hint bar, and a scoped `?` detail popup — without breaking
the index-keyed dispatch the current view depends on.

## The finding that shapes this whole plan

Read on **2026-08-16** from `src/ui/settings.rs` and `src/app/settings.rs`:

**Row identity is a bare integer index, in four places that must agree.**

```rust
// src/ui/settings.rs:98
pub fn row_kind(index: usize) -> Option<RowKind> {
    match index {
        0 | 2 | 4 | 5 | 6 | 7 | 8 | 10 | 11 | 14 | 16 => Some(RowKind::Text),
        1 => Some(RowKind::Theme),
        3 | 9 | 12 | 13 | 15 => Some(RowKind::Toggle),
        ...
```

```rust
// src/app/settings.rs:36
fn settings_toggle_row(app: &mut App) {
    match app.settings.selected {
        3  => { app.config.seed_by_default   = !app.config.seed_by_default; ... }
        9  => { app.config.use_alt_rates     = !app.config.use_alt_rates;   ... }
        12 => app.config.enable_upnp = !app.config.enable_upnp,
        13 => app.config.enable_dht  = !app.config.enable_dht,
        15 => { app.config.stop_seed_at_ratio = !app.config.stop_seed_at_ratio; ... }
```

`row_kind`, `text_field`, `row_label`, `row_value`, `source_at` (all in `ui/settings.rs`) and
`settings_toggle_row` (in `app/settings.rs`) each re-derive meaning from the same integer.
**Inserting a single category header shifts every index below it and silently makes "Enable DHT"
toggle UPnP.** Nothing in the type system catches it; the existing test
`row_layout_matches_the_settings_contract` asserts the *old* numbers, so it would go red — which
is the only reason this is survivable at all.

**Therefore step 1 is not "add categories". It is "delete the integers".** Categories are a
trivial follow-on once row identity is a value, not a position.

**Second finding: the panel does not scroll.**

```rust
// src/ui/settings.rs:240-242
let visible = (panel.height as usize).saturating_sub(5).min(rows);
for index in 0..visible {
```

It always renders rows `0..visible`. There is no scroll offset and `state.selected` is never
consulted for viewport placement. Today that is 27 rows (`APP_ROWS = 17` + `SourceId::ALL.len()`
= 10) in a panel capped at the terminal height — **already broken at 24 rows**: select row 26 and
the highlight is off-screen with no way to see it. Adding ~6 headers and doubling row height for
inline blurbs makes it ~72 lines. **Scrolling is not optional in this issue; it is a prerequisite
the current code is already violating.**

## SPEC / FR reference

**Exists today.** `FR-51` (user configuration) is what the overlay edits. `UR-11` ("every async
operation shows state … never a blank pane") is the nearest thing to a discoverability rule.
`NFR-12` requires a why-comment at each non-obvious decision site — the descriptor table is
exactly such a site.

**Missing from SPEC — add first, then implement.** Proposed **UR-14 / UR-15** in §5:

- **UR-14 (settings organisation).** The settings overlay groups rows under named categories —
  Downloads, Speed, Connection, BitTorrent, Sources, Advanced — in a fixed order. A category
  header is not selectable; navigation skips it. The selection is always visible: the panel
  scrolls to follow it.
- **UR-15 (focus-driven explanation).** Every setting carries three tiers of explanation:
  a one-line inline blurb under its label, a longer sentence in the panel's hint bar for the
  **selected** row, and a full paragraph in a scoped detail overlay opened with `?`. All three
  are driven by the keyboard selection. harbour never shows an explanation on pointer hover —
  a TUI has focus, not a cursor, and hover text that appears under a mouse the user is not
  looking at is noise.

UR-15's last sentence is the issue's stated principle written into the referee document, so a
future "add tooltips" PR has something to be measured against.

## Workstream

**Terminal UI (Ishan)** owns every step.

Shared-type dependencies: **`SourceId` only**, read through `SourceId::ALL` exactly as today.
No change to `TorrentResult`, `QueueStatus`, the `Source` trait, or the engine event enum, so
nothing here needs Sarthak's sign-off beyond normal review.

**Ordering against sibling issues.** #63, #64 and #65 all touch `src/ui/settings.rs` and
`src/ui/status.rs`. Land in this order: **#63 → #64 → #65.** #63's descriptor table is what
#64's hint bar reads for the settings screen, and #65's breakpoints switch off #63's inline
blurbs on short terminals. Landing them in any other order produces three-way conflicts in the
same two files.

## Approach

**Step 1 — kill the index magic (pure refactor, no visible change). ~250 lines.**

One const descriptor table in `src/ui/settings.rs` is the single source of truth:

```rust
/// Which category a row belongs to. Order here is the render order.
pub enum Category { Downloads, Speed, Connection, BitTorrent, Sources, Advanced }

/// The toggle rows, so the dispatch matches a value instead of an index —
/// the whole point of this table (a header insert used to renumber every
/// toggle and silently flip the wrong config bit).
pub enum ToggleField { SeedByDefault, UseAltRates, Upnp, Dht, StopSeedAtRatio }

pub struct SettingDef {
    pub kind:  RowKind,
    pub cat:   Category,
    pub label: &'static str,
    pub text:  Option<TextField>,
    pub toggle: Option<ToggleField>,
    /// One line under the label. <= 56 chars so it fits the value column.
    pub blurb: &'static str,
    /// The `?` popup body. Says what it does, the unit, and the default.
    pub detail: &'static [&'static str],
}

const SETTINGS: &[SettingDef] = &[ /* the 17 app rows, in category order */ ];
```

`ToggleField` deliberately mirrors the existing `TextField` — same shape, same
`fn toggle_field(index) -> Option<ToggleField>` accessor — so the file reads as one pattern
rather than two.

Public accessors keep their current signatures (`row_kind`, `row_label`, `text_field`,
`source_at`, `row_count`) and become table lookups. `settings_toggle_row` in
`src/app/settings.rs` becomes `match toggle_field(app.settings.selected) { … }`. The existing
tests are rewritten to assert the *table*, plus one new test asserting every `TextField` and
every `ToggleField` variant appears exactly once (an unreachable variant is now a caught bug,
which the index match could never detect).

Nothing renders differently. This step is independently mergeable and is the one that de-risks
everything after it.

**Step 2 — categories + scrolling. ~180 lines.**

The row list becomes a flattened enum, built once per draw:

```rust
pub enum Row { Header(Category), Setting(usize /* SETTINGS index */), Source(SourceId) }
pub fn rows() -> Vec<Row>;
```

Sources become the `Category::Sources` block, appended in `SourceId::ALL` order — the existing
`source_rows_follow_the_search_sidebar_order` test still passes unchanged, which is the point:
the sidebar and the settings list must never disagree.

Navigation: `settings_move_up`/`_down` skip `Row::Header`. A `SettingsState::offset: usize`
follows the selection (`offset = offset.clamp(sel + 1 - visible, sel)`), and `draw` renders
`offset..offset + visible`. A scrollbar column on the right using the same viewport math as
`ui/downloads.rs` (already solved there — reuse, don't reinvent).

**Step 3 — inline muted blurbs. ~120 lines.**

Each `Row::Setting` renders as two lines: `label … value` then an indented muted `blurb`.
`row_height()` becomes a function of the row kind and the available panel height, and the
viewport math in step 2 counts *lines*, not rows. Blurbs are suppressed when the panel is short
(see #65) — one call to a shared predicate, not a second layout path.

**Step 4 — the contextual hint bar. ~60 lines.**

The panel's static footer (`const HINT: &str = "↑/↓ move · enter edit/toggle · esc back"`)
splits into two rows: the keybind row it is today, and above it a muted sentence for the
**selected** row — the longer "why" that does not fit in a blurb. Driven strictly by
`state.selected`. The `mouse_pos` hover highlight in `setting_line` **stays** (it is a highlight,
not a tooltip) but must never feed the hint bar; a why-comment at the decision site says so.

**Step 5 — the scoped detail popup. ~110 lines.**

`?` on a selected row opens a small sub-overlay over the settings panel: the label, the
`detail` paragraph, the current value, and the default value. Esc or `?` closes it.

The keybinding conflict is real and must be handled explicitly. Today, `src/input.rs:288`
maps *every* `KeyCode::Char(c)` to `Action::SettingsType(c)` while the overlay is open, and
`src/input.rs:689` asserts exactly that for `'?'`. Split on `state.editing`:

- `editing == true` → `Action::SettingsType('?')` (a `?` in a path or tracker URL must type)
- `editing == false` → `Action::SettingsDetail`

`map()` does not currently receive `editing`, so it gains a parameter alongside the existing
`settings_open`. The input.rs test at :689 changes from "`?` always types" to a pair of cases,
and that change is the documentation of the new rule.

## Files to create / modify

- `SPEC.md` — UR-14 / UR-15 in §5. **First commit, before any code.**
- `src/ui/settings.rs` — the descriptor table, `Category`, `ToggleField`, `Row`, `rows()`,
  `toggle_field()`, scroll offset, header rendering, two-line rows, the hint bar, the detail
  popup. This file is 566 lines today and will land near 900 — over the FR-67 700-line norm,
  so **split as part of step 3**: `src/ui/settings/mod.rs` (state + draw) and
  `src/ui/settings/rows.rs` (the table + accessors). The split is mechanical and the table is
  the natural seam.
- `src/app/settings.rs` — `settings_toggle_row` matches `ToggleField`; `settings_move_up`/
  `_down` skip headers and maintain `offset`; `settings_detail_toggle`.
- `src/input.rs` — `Action::SettingsDetail`; `map()` takes `settings_editing`; the `'?'` split;
  Esc closes the detail popup before the overlay.
- `src/app/mod.rs` — `draw`'s settings call unchanged in shape; `SettingsState` gains `offset`
  and `detail_open`.
- `src/ui/help.rs` — one `BINDINGS` row: `("?", "settings: explain the selected row")`. The
  `every_binding_is_documented_once` test enforces it.
- `src/ui/tests.rs` — the new buffer snapshots.

## Key APIs / libraries

**No new crates.** Everything is ratatui **0.30.2** (current — crates.io, checked 2026-08-16;
harbour is already on latest) plus the existing theme tokens.

- The detail popup is a second `Clear` + `Block` + `Paragraph` drawn after the settings panel,
  in the same style as `ui/help.rs::draw` and `ui/status.rs::draw_folder_prompt`. ratatui has no
  built-in popup widget and does not need one — `Clear` over a `Rect` is the whole mechanism,
  and the codebase already does it in five places.
- The scrollbar is `ratatui::widgets::Scrollbar`, already used by `ui/downloads.rs`; reuse its
  viewport math rather than deriving a second one.
- ratatui 0.30 removed `WidgetRef` in favour of `Widget for &T` and moved
  `render_stateful_widget_ref` behind the `unstable-widget-ref` feature
  ([ratatui.rs/highlights/v030](https://ratatui.rs/highlights/v030/), checked 2026-08-16).
  Nothing here needs either — every view in harbour renders by value.

## Risks / edge cases

- **The renumbering bug is the whole risk.** If steps 1 and 2 are merged as one PR, a reviewer
  cannot tell a refactor from a behaviour change. Keep them separate; step 1's diff must show
  zero test-expectation changes other than "assert via the table instead of via literals".
- **`SourceId::ALL` is 10 today but the enum has 13 variants** (`Indexer`, `X1337Movies`,
  `X1337Tv` were dropped from the sidebar in `f3268b1`). `source_label` still has arms for all
  13. Keep it that way — the table must not assume `ALL.len()` equals the variant count.
- **`text_field(1) == None` for the Theme row** is load-bearing (the theme row is `RowKind::Theme`
  and has no buffer). The table must model that as `text: None`, and a test asserts a Theme row
  never enters an inline edit.
- **Rejected: a `Vec<SettingDef>` built at runtime.** A const table is checkable at review time
  and costs nothing; a runtime builder invites conditional rows, which is how the index bug
  would come back wearing a different hat.
- **Rejected: hover tooltips.** Named here so it is rejected once, in writing. The codebase
  already tracks `mouse_pos` and it would be five lines. UR-15 forbids it: a tooltip under a
  pointer the user is not looking at competes with the hint bar they are.
- **Rejected: a settings search/filter box.** Out of scope for #63 and it re-introduces dynamic
  row lists, which is what step 1 exists to remove.
- **Blurbs must not push the value column off-screen.** Blurbs render on their own line,
  indented, and truncate through the existing `truncate()` helper — they never share a row with
  a value.
- **A 24-row terminal shows ~19 panel lines.** With two-line rows that is 9 settings. Step 3
  must land with #65's short-terminal predicate, or the overlay becomes *less* usable than the
  flat list it replaced. This is the one hard cross-issue dependency.

## Test strategy

- **Unit, `src/ui/settings/rows.rs`** — the table is total: every `TextField` variant maps from
  exactly one row; every `ToggleField` variant maps from exactly one row; `row_count()` equals
  `SETTINGS.len() + SourceId::ALL.len()`; `rows()` emits exactly one header per `Category` and
  headers are never adjacent to each other.
- **Unit, navigation** — from the last row, `down` is a no-op; `up` from the row under a header
  lands on the setting above the header, never on the header; `offset` keeps `selected` inside
  `[offset, offset + visible)` for every selection on an 8-line and a 40-line panel.
- **Unit, parity** — a regression test that pins the *behaviour* the old indices encoded:
  `toggle_field` for the row labelled "Enable DHT" is `ToggleField::Dht`, and so on for all five.
  This is the test that would have caught the renumbering bug, so it is written first.
- **Buffer snapshot, `src/ui/tests.rs`** — at 100×40: the six category headers render; the
  selected row's blurb and hint bar text are both present. At 100×24: headers render, blurbs do
  not, the hint bar survives. With `selected = row_count() - 1`: the last source row is on
  screen (the scroll regression test).
- **Buffer snapshot** — `detail_open` with a `Text` row selected renders the label, the detail
  paragraph, and the word "default"; with the detail closed, it does not.
- **Unit, `src/input.rs`** — `'?'` with `settings_open && !editing` is `Action::SettingsDetail`;
  `'?'` with `settings_open && editing` is `Action::SettingsType('?')`.

## Verification

1. `cargo run` → `shift+S`. The panel shows **Downloads / Speed / Connection / BitTorrent /
   Sources / Advanced** headers, each row with a muted one-liner under it.
2. Hold `↓` from the top. The selection never lands on a header, the panel scrolls once the
   selection reaches the bottom, and **the last source row is reachable and visible** — which it
   is not on `main` today. That single scroll is the clearest user-visible proof.
3. Move the selection with the mouse pointer parked over a *different* row. The hint bar tracks
   the **keyboard** selection, not the pointer. That is UR-15 demonstrated.
4. Select "Stop Seeding at Ratio", press `?`. A small overlay explains ratio-based seed stopping
   and names the default (`1.0`). Esc returns to the list with the selection unmoved.
5. Select "Enable DHT", press Enter. `~/.harbour/config.toml` shows `enable_dht` flipped and
   **nothing else changed** — the anti-renumbering check, run by hand once before merge.
6. Resize to 80×24 with settings open. No panic, no clipped border, blurbs collapse, hint bar
   remains.
