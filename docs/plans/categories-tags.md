# Categories + tags
Ref: #46

## Goal
Give downloads a colour-coded **category** (one per item, with its own save path and share
limits) and free-form **tags** (many per item), with qBittorrent-style subcategories that fall
back to their parent when a category is deleted.

## SPEC / FR reference

**Nothing in SPEC.md covers organisation at all** — no category, no tag, no label. The closest
is **FR-40** (*"the user can change the output folder per item at enqueue time only"*,
SPEC.md:187) and **FR-51** (config persists the default output folder, SPEC.md:234), both of
which this feature interacts with. Per AGENTS rule 2, **add to SPEC first**.

FR numbers **FR-98 … FR-102** (FR-69…FR-97 are claimed: FR-69…FR-85 by the existing plans,
FR-86…FR-89 by `speed-limits.md`, FR-90…FR-93 by `queue-management.md`, FR-94…FR-97 by
`share-limits.md`). This needs a **new SPEC subsection §4.9 "Organisation"**, since §4.4 is
downloads and §4.5 is seeding and this spans both.

- **FR-98 (categories).** An item has at most one category. A category has a name, a colour, an
  optional save path, and optional share limits. Categories are user-defined; harbour ships
  none.
- **FR-99 (subcategories).** A category name may contain `/` to nest (`Movies/4K`). Nesting is
  naming, not a separate structure: the parent of `Movies/4K` is `Movies`.
- **FR-100 (deleting a category).** Deleting a category reassigns its items to its **parent**
  category, or to *uncategorised* when it has none. Deleting a category never touches files and
  never moves anything on disk.
- **FR-101 (save path).** A category's save path is the default output folder for items
  assigned to it **at enqueue time**, consistent with FR-40. Changing a category's save path
  does **not** move existing items' files. An empty save path means "use the global default".
- **FR-102 (tags).** An item may carry any number of free-form tags. Tags have no colour, no
  save path, and no limits — they exist to filter. Removing a tag from the tag list removes it
  from every item that carried it.

## Workstream

**Engine & Foundation (Sarthak)** owns the `Category` type, the `QueueItem` fields, and the
resolution rules — `Category` becomes a **shared type** the moment `share-limits.md` FR-97 and
the UI both read it, so it must be frozen by Sarthak before either builds on it. That is the
single most important sequencing fact in this plan.

**Terminal UI (Ishan)** owns the category manager overlay, the colour rendering, the filter
sidebar, and the assign keybinds.

**Depends on:**
- `speed-limits.md` **step 1** (the settings-row table) — this plan adds settings rows.
- **`share-limits.md` is downstream of this plan**, not upstream: it ships its `ShareLimits`
  resolver with a global-only body, and its step 6 fills in the category branch once
  `Category` lands here. Neither plan defines the other's type.

## Approach

**Step 1 — SPEC §4.9, FR-98…FR-102 (docs only).**

**Step 2 — the `Category` type, frozen (engine).**

```rust
/// A user-defined category. `name` may contain `/` to nest (FR-99); the
/// hierarchy is derived from the string, never stored as a tree — a tree
/// would need its own invariants (orphans, cycles, rename cascades) to buy
/// nothing a `rsplit_once('/')` does not already give.
pub struct Category {
    pub name: String,
    pub color: PaletteSlot,
    pub save_path: Option<PathBuf>,
    pub limits: Option<ShareLimits>,
}
```

with `Category::parent(&self) -> Option<&str>` = `self.name.rsplit_once('/').map(|(p, _)| p)`.

**Colour is a theme palette slot, not a hex value.** `ThemeColors` (`src/theme.rs:167-202`) is
a fixed named palette and harbour ships a live theme-switching system (`src/theme_watch.rs`). A
category storing `#ff0000` would be invisible or ugly under a theme that did not expect it, and
would break the theme requirements in SPEC §6. So `PaletteSlot` is a small enum
(`Accent | Success | Warning | Error | Muted | …`) resolved against the *active* `ThemeColors`
at paint time. This is the harbour-shaped decision here and it gets a comment at the type.

**Step 3 — items carry a category and tags (engine).**
`QueueItem` gains, both following the `bytes` precedent at `src/core/types.rs:406`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub category: Option<String>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub tags: Vec<String>,
```

The item stores the category **name**, not an index or a reference — names are what the ledger
should read as by eye, and an item whose category was deleted degrades to uncategorised rather
than to a dangling index. This mirrors how `SourceId::parse` degrades an unknown id to `None`
rather than failing the file (`src/core/types.rs:117-119`).

**Step 4 — the category store and its rules (engine).**
Categories and the tag list persist in `config.toml` alongside `Config` (they are user
configuration, not queue state). A small `Categories` struct owns:
- `add` / `rename` / `remove`, with **FR-100's reparenting** implemented in exactly one place:
  removing `Movies/4K` reassigns its items to `Movies`; removing `Movies` reassigns its items
  to uncategorised **and** reparents `Movies/4K` → `4K`? **No** — decide once: removing a parent
  removes only that category; its *sub*categories are left intact and keep their full names,
  and only items directly on the removed name are reassigned. Renaming cascades to
  subcategories and to every item carrying the old prefix. Record this in FR-100 so it is not
  re-litigated.
- `resolve_save_path(name) -> Option<PathBuf>`: the category's own path, else its parent's, else
  `None` (global default). One resolver, one call site.

**Step 5 — enqueue uses the category path (engine + UI seam).**
`AddInput.dir` (`src/queue.rs:69`) is already the per-item directory and `download_selected`
(`src/app/actions.rs`) already computes it. Category resolution slots in **there** — before the
item is created — which is what keeps FR-101 ("at enqueue time only") true and FR-40 unbroken.
No new mutation path.

**Step 6 — the category manager overlay (UI).**
A modal like the settings overlay: list categories, add/rename/delete, cycle colour through the
palette slots, edit save path inline. Reuse `SettingsState`'s inline-edit pattern
(`src/ui/settings.rs:76-90`) rather than inventing a second text-edit mechanism. Pure paint:
`draw` renders the store, the app loop mutates it.

**Step 7 — assignment and display (UI).**
- `c` on a downloads row opens a category picker; `t` opens a tag editor (comma-separated,
  committed on Enter — free-form means a text field, not a fixed list).
- The downloads row shows a coloured category chip.
- The downloads view gains a filter: by category (including "all under `Movies/`", which
  subcategories-as-prefixes makes a `starts_with`) and by tag.

**Step 8 — `share-limits.md` step 6 lands** (its plan, not this one): `effective_limits`
consults `item.category`.

## Files to create / modify

- `SPEC.md` — new §4.9, FR-98…FR-102; cross-references from FR-40 and FR-51.
- `src/core/types.rs` — `Category`, `PaletteSlot`, `QueueItem::category`, `QueueItem::tags`.
- `src/persist.rs` — `categories: Vec<Category>` and `tags: Vec<String>` on `Config`, both
  `#[serde(default)]`; the `Categories` store with add/rename/remove/resolve.
- `src/theme.rs` — `ThemeColors::slot(PaletteSlot) -> Color`, the one place a slot becomes a
  colour.
- `src/ui/categories.rs` — **new**: the manager overlay (pure paint).
- `src/ui/mod.rs` — `Screen::Categories` and the overlay state struct.
- `src/ui/downloads.rs` — the category chip, tag display, the filter row.
- `src/ui/settings.rs` — a row that opens the manager, in the step-1 table.
- `src/app/actions.rs` — category-aware `dir` at enqueue; assign/tag actions.
- `src/input.rs` — `c` / `t` and the overlay's key dispatch.
- `docs/plans/categories-tags.md` — this file.

## Key APIs / libraries

No librqbit involvement whatsoever — categories are harbour metadata and never reach the
engine. `AddTorrentOptions.output_folder` (`librqbit-8.1.1/src/session.rs:253`) already carries
the resolved directory, and `src/engine/rqbit.rs:427` already sets it from `req.dir`; a
category path is just a different value for that existing field. Verified against the vendored
source on 2026-08-16.

ratatui 0.30.2 is current ([github.com/ratatui/ratatui/releases](https://github.com/ratatui/ratatui/releases),
checked 2026-08-16) and already provides everything the overlay needs — `Block`, `Clear`,
`Paragraph`, `List` — the same widgets `src/ui/settings.rs` uses. No new widget crate.

Subcategory and category-deletion semantics referenced from qBittorrent's category model
(`/`-nested names, per-category save paths); see
[qBittorrent issue #20146](https://github.com/qbittorrent/qBittorrent/issues/20146) and
[trash-guides' qBittorrent category guide](https://trash-guides.info/Downloaders/qBittorrent/How-to-add-categories/),
both checked 2026-08-16. harbour deliberately does **not** copy qBittorrent's "Automatic Torrent
Management" (which moves files when a category path changes) — see Risks.

**New crates: none.**

## Risks / edge cases

- **Rejected approach: automatic file moving on category change.** qBittorrent's ATM moves
  files on disk when a torrent's category or a category's save path changes. Rejected for v1:
  it is a destructive, long-running, failure-prone filesystem operation, and FR-40 already
  restricts the output folder to enqueue time. FR-101 states the non-move explicitly so users
  are not surprised — an unstated non-behaviour is how people lose track of their files.
- **Rejected approach: raw hex colours per category.** Breaks under theme switching, which
  harbour supports live (`src/theme_watch.rs`). Palette slots keep every category readable under
  every theme, including the 256-colour fallback (NFR-09).
- **A deleted category leaves dangling names on items.** Handled by degrading to uncategorised
  on read, the same policy as `SourceId::parse`. The alternative — failing the ledger load —
  would turn a cosmetic problem into data loss (FR-54's whole premise).
- **`/` in a name is meaningful, so it must be validated.** Reject empty segments (`Movies//4K`),
  leading/trailing `/`, and — because a category name can become a **path component** via
  FR-101 — reject `..`, path separators other than `/`, and anything `src/core/paths.rs`'s
  existing safety rules would refuse. NFR-11 requires paths be derived from known-safe ids; a
  free-text category name feeding a directory path is exactly the boundary that needs a
  validator, and it needs one before FR-101 ships, not after.
- **Tags are free-form, so they need the same validation minus the path concern** — trim
  whitespace, reject empties, dedupe case-insensitively. A tag never becomes a path.
- **Rename cascades are the fiddly part.** Renaming `Movies` → `Films` must update
  `Movies/4K` → `Films/4K` and every item carrying either. One function, heavily tested; if it
  is spread across call sites it will drift.
- **Scope split.** Category *filtering* in the downloads view and the manager overlay are two
  separate PRs under the <400-line rule. Steps 6 and 7 must not land as one.

## Test strategy

- **Unit, `src/core/types.rs`** — `Category::parent` for top-level, one-deep, and multi-deep
  names; `QueueItem` round trips with and without `category`/`tags`; a legacy ledger loads with
  `None`/empty.
- **Unit, `src/persist.rs`** (the bulk of the value here):
  - removing a subcategory reassigns its items to the parent (FR-100).
  - removing a top-level category reassigns its items to uncategorised and leaves its
    subcategories intact.
  - renaming cascades to subcategories and to every affected item.
  - `resolve_save_path` falls back category → parent → `None`.
  - an item referencing a deleted category reads as uncategorised, and the ledger still loads.
  - name validation rejects `..`, `Movies//4K`, `/Movies`, `Movies/`, and empty names.
- **Unit, `src/theme.rs`** — every `PaletteSlot` resolves to a colour under every shipped theme;
  no slot resolves to the background colour (an invisible chip is a bug, not a style).
- **Buffer snapshot, `src/ui/tests.rs`** — a row with a category renders its coloured chip; an
  uncategorised row renders none; the manager overlay renders a nested category indented under
  its parent; the tag editor renders its comma-separated buffer.
- **No engine tests.** Categories never reach the engine, and a `HARBOUR_TEST_NET=1` test would
  assert nothing.

## Verification

1. `SPEC.md` has §4.9 with FR-98…FR-102, and FR-100 records the remove-a-parent decision.
2. `cargo run` → category manager → create `Movies` with a save path and a colour, then
   `Movies/4K` with no save path. Enqueue an item into `Movies/4K`. **The item downloads into
   `Movies`' save path** (FR-101's parent fallback) and its row shows a coloured `Movies/4K`
   chip. That combination is the user-visible proof that nesting, colour, and path resolution
   all work together.
3. Delete `Movies/4K`: the item's chip changes to `Movies`, its files do not move, and the
   download keeps running.
4. Switch themes with `t`/the settings row while categories are on screen: **every chip stays
   readable**, which is what the palette-slot decision buys and what a hex colour would fail.
5. Add two tags to an item, filter the downloads view by one of them: only that item shows.
6. Quit and relaunch: categories, colours, paths, assignments and tags all survive.
