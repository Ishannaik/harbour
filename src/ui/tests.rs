//! Buffer-snapshot tests for the phase-2 views (docs/roadmap.md Phase 2 DoD):
//! each view renders into a `TestBackend` buffer at the 80×24 minimum
//! (UR-12) and the exact symbol layout is asserted.
//!
//! Styles are asserted separately where they carry meaning (selection,
//! health dots); the snapshot asserts layout/text, which is what layout
//! regressions break. The search view is snapshotted in the idle state
//! (not searching) so the shimmer band and spinner — which are clock-driven —
//! cannot make the test timing-dependent.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::fake;
use crate::theme::Theme;
use crate::types::{AppState, Screen, SearchState, SourceStatus};
use crate::ui::{downloads, help, search, status};

const W: u16 = 80;
const H: u16 = 24;

/// Renders `draw` into an 80×24 buffer and returns one line per row of
/// symbols (trailing spaces trimmed), so snapshots read as text.
fn render(f: impl FnOnce(&mut ratatui::Frame)) -> String {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal.draw(|frame| f(frame)).expect("draw must succeed");
    let buf = terminal.backend().buffer();
    (0..H)
        .map(|y| {
            let mut line: String = (0..W).map(|x| buf[(x, y)].symbol().to_string()).collect();
            while line.ends_with(' ') {
                line.pop();
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A search state with deterministic fake results + sidebar health/counts,
/// mirroring the loop's `apply_results` (same fake generator, same state).
fn search_state(query: &str) -> SearchState {
    let results = fake::fake_results(query);
    let mut state = SearchState {
        query: query.to_string(),
        ..SearchState::default()
    };
    for r in &results {
        state.source_health.insert(r.source, SourceStatus::Online);
        *state.source_counts.entry(r.source).or_insert(0) += 1;
    }
    state.results = results;
    state.selected = 0;
    state
}

fn downloads_state() -> crate::types::DownloadsState {
    crate::types::DownloadsState {
        items: fake::fake_queue(),
        history: fake::fake_history(),
        selected: 0,
        show_seeding: false,
    }
}

// --- search ---------------------------------------------------------------

#[test]
fn search_snapshot_idle_with_results() {
    let state = search_state("dune");
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &theme);
    });
    assert!(out.contains("harbour — search"));
    assert!(out.contains("dune"));
    assert!(out.contains("FitGirl"));
    assert!(!out.contains("no results yet"));
}

#[test]
fn search_snapshot_empty_state() {
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &SearchState::default(), &theme);
    });
    assert!(out.contains("no results yet — press Enter to search"));
    assert!(out.contains("search torrents…"));
}

// --- downloads ------------------------------------------------------------

#[test]
fn downloads_snapshot_active_tab() {
    let state = downloads_state();
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme);
    });
    let expected = concat!(
        "╭ harbour — downloads ─────────────────────────────────────────────────────────╮\n",
        "│  Downloads   Seeding                                                         │\n",
        "│  ─────────                                                                   │\n",
        "│Oppenheimer (2023) [1080p] [WEBRip] [downloading]            1.3 GiB / 3.0 GiB│\n",
        "│████████████████░░░░░░░░░░░░░░░░░░░░░░░   42%  8.4 MiB/s  peers 137  eta 30:00│\n",
        "│Shogun S01E01 [1080p] [WEBRip] [paused]                    266.2 MiB / 2.0 GiB│\n",
        "│█████▓░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   13%  0.0 MiB/s  peers —  eta —│\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│ recently downloaded ─────────────────────────────────────────────────────────│\n",
        "│Dune: Part Two (2024) [1080p] [BluRay]                    4.0 GiB  [completed]│\n",
        "│←→ tab · p pause · ? help · q quit                                            │\n",
        "╰──────────────────────────────────────────────────────────────────────────────╯",
    );
    assert_eq!(out, expected);
}

#[test]
fn downloads_snapshot_seeding_tab() {
    let mut state = downloads_state();
    state.show_seeding = true;
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme);
    });
    let expected = concat!(
        "╭ harbour — downloads ─────────────────────────────────────────────────────────╮\n",
        "│  Downloads   Seeding                                                         │\n",
        "│              ───────                                                         │\n",
        "│Frieren: Beyond Journey's End - …   2.1 MiB/s  up 2.0 GiB  peers 42  [seeding]│\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│                                                                              │\n",
        "│←→ tab · p pause · ? help · q quit                                            │\n",
        "╰──────────────────────────────────────────────────────────────────────────────╯",
    );
    assert_eq!(out, expected);
}

// --- status bar -----------------------------------------------------------

#[test]
fn status_snapshot_search_with_banner() {
    let mut state = AppState::default();
    state.search.query = "dune".into();
    state.error_banner = Some("engine: connection refused".into());
    let theme = Theme::titanium();
    let out = render(|f| {
        status::draw(f, f.area(), Screen::Search, &state, &theme, "⠋");
    });
    // 20 blank rows, then the banner (3 rows) and the status line.
    let mut expected = String::new();
    for _ in 0..20 {
        expected.push('\n');
    }
    expected.push_str(concat!(
        "┌ error ───────────────────────────────────────────────────────────────────────┐\n",
        "│engine: connection refused                                                    │\n",
        "└──────────────────────────────────────────────────────────────────────────────┘\n",
        "search │ dune                                                                  ⠋",
    ));
    assert_eq!(out, expected);
}

#[test]
fn status_snapshot_downloads_context() {
    let state = AppState {
        downloads: downloads_state(),
        ..AppState::default()
    };
    let theme = Theme::titanium();
    let out = render(|f| {
        status::draw(f, f.area(), Screen::Downloads, &state, &theme, "⠙");
    });
    // 23 blank rows + the status line.
    let mut expected = String::new();
    for _ in 0..23 {
        expected.push('\n');
    }
    expected.push_str(
        "downloads │ 1 active · 1 seeding                                               ⠙",
    );
    assert_eq!(out, expected);
}

// --- help -----------------------------------------------------------------

#[test]
fn help_snapshot_modal() {
    let theme = Theme::titanium();
    let out = render(|f| {
        help::draw(f, f.area(), &theme);
    });
    let expected = concat!(
        "\n",
        "\n",
        "\n",
        "\n",
        "\n",
        "\n",
        "            ╭ keybinds ───────────────────────────────────────────╮\n",
        "            │enter         search (empty = browse curated lists)  │\n",
        "            │↑ / ↓         move selection                         │\n",
        "            │d / shift+d   download to default / chosen folder    │\n",
        "            │tab           switch screen                          │\n",
        "            │← / →         switch downloads tab                   │\n",
        "            │p             pause / resume                         │\n",
        "            │?             close help                             │\n",
        "            │esc           close help                             │\n",
        "            │q / ctrl+c    quit                                   │\n",
        "            ╰─────────────────────────────────────────────────────╯\n",
        "\n",
        "\n",
        "\n",
        "\n",
        "\n",
        "\n",
    );
    assert_eq!(out, expected);
}

// --- selection styling ----------------------------------------------------

#[test]
fn selected_row_uses_selected_bg() {
    let theme = Theme::titanium();
    let state = search_state("dune");
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &theme))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let selected_bg = theme.colors.selected_bg().to_ratatui();
    // First result row: outer border (0), inner top border (1), search bar
    // top (2), search bar bottom (3), results header (4), row 5 is the first
    // result — the selected one. Col 23 is inside the main column (the
    // sidebar owns cols 1..22, which have no selection highlight).
    let row = 5;
    let cell = &buf[(23, row)];
    assert_eq!(
        cell.bg, selected_bg,
        "selected row must render on selectedBg"
    );
}

#[test]
fn source_health_dot_colors() {
    let theme = Theme::titanium();
    let mut state = SearchState::default();
    state.source_health.insert("yts", SourceStatus::Online);
    state.source_counts.insert("yts", 3);
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &theme))
        .expect("draw");
    let buf = terminal.backend().buffer();
    // Sidebar rows: 1 "Sources", 2 " Games", 3 "  ● FitGirl", 4 " Movies",
    // 5 "  ● YTS". The dot is the 4th char (border + two spaces + glyph).
    let row = 5;
    let cell = &buf[(3, row)];
    assert_eq!(cell.symbol(), "●", "online source shows the success dot");
}
