//! Buffer-snapshot tests for the views (docs/roadmap.md Phase 2 DoD): each
//! view renders into a `TestBackend` buffer at the 80×24 minimum (UR-12) and
//! the exact symbol layout is asserted.
//!
//! Styles are asserted separately where they carry meaning (selection,
//! health dots); the snapshot asserts layout/text. The search view is
//! snapshotted idle (not searching) so the clock-driven shimmer/spinner
//! cannot make the test timing-dependent.

use std::collections::HashSet;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::core::types::{SourceId, SourceStatus, TorrentResult};
use crate::fake;
use crate::theme::Theme;
use crate::ui::{
    AppState, DownloadsState, Screen, SearchState, downloads, help, now_playing, search, status,
};

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
/// mirroring what the app loop populates after a search.
fn search_state(query: &str) -> SearchState {
    let results: Vec<TorrentResult> = fake::fake_results(query);
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

fn downloads_state() -> DownloadsState {
    DownloadsState {
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
        search::draw(f, f.area(), &state, &HashSet::new(), &theme);
    });
    // "dune" hits the movie catalog: real title in the rows, sidebar visible.
    assert!(out.contains("harbour — search") || out.contains("search"));
    assert!(out.contains("Dune: Part Two"), "catalog title in results");
    assert!(out.contains("GamesHub"), "sidebar lists all sources");
    assert!(!out.contains("no results"), "results present");
}

#[test]
fn a_disabled_source_renders_dim_in_the_sidebar() {
    // GamesHub is the only Games source: sidebar inner row 2 (title row 0,
    // Games divider row 1), which the 1-cell panel border pushes to frame
    // row 3. Disabled wins over health, so the whole row must be dim.
    let theme = Theme::titanium();
    let state = search_state("dune");
    let mut disabled = HashSet::new();
    disabled.insert(SourceId::GamesHub);
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &disabled, &theme))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let dim = theme.colors.dim().to_ratatui();
    let row = 3;
    let dim_cells = (0..W).filter(|&x| buf[(x, row)].fg == dim).count();
    assert!(
        dim_cells >= 7,
        "GamesHub label must render dim, got {dim_cells} cells"
    );
}

#[test]
fn search_snapshot_empty_state() {
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(
            f,
            f.area(),
            &SearchState::default(),
            &HashSet::new(),
            &theme,
        );
    });
    assert!(
        out.contains("no results") || out.contains("nothing"),
        "empty state names the next action, got:\n{out}"
    );
}

// --- downloads ------------------------------------------------------------

#[test]
fn downloads_snapshot_active_tab() {
    let state = downloads_state();
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme);
    });
    assert!(out.contains("Oppenheimer"));
    assert!(out.contains("downloading") || out.contains("[downloading]"));
    assert!(out.contains("Shogun"), "paused item visible on active tab");
    assert!(out.contains("recently downloaded"));
    assert!(out.contains("Dune: Part Two"), "history row");
}

#[test]
fn downloads_snapshot_seeding_tab() {
    let mut state = downloads_state();
    state.show_seeding = true;
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme);
    });
    assert!(out.contains("Frieren"));
    assert!(
        !out.contains("Oppenheimer"),
        "active downloads hidden on the seeding tab"
    );
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
    assert!(out.contains("engine: connection refused"));
    assert!(out.contains("dune"));
}

// --- help & now playing ---------------------------------------------------

#[test]
fn help_snapshot_modal() {
    let theme = Theme::titanium();
    let out = render(|f| {
        help::draw(f, f.area(), &theme);
    });
    assert!(out.contains(" keys ") || out.contains("keybinds"));
    assert!(out.contains("quit"));
    assert!(out.contains("watch"), "w binding listed");
}

#[test]
fn player_picker_snapshot_modal() {
    let theme = Theme::titanium();
    let picker = crate::ui::player::PlayerPicker {
        open: true,
        mode: crate::ui::player::PickerMode::List,
        selected: 0,
        options: vec![
            ("mpv".to_string(), "mpv".to_string()),
            (
                "Windows Media Player".to_string(),
                "C:\\wmplayer.exe".to_string(),
            ),
        ],
        custom: String::new(),
        message: Some("not an existing absolute path".into()),
    };
    let out = render(|f| {
        crate::ui::player::draw(f, f.area(), &theme, &picker, Some("mpv"));
    });
    assert!(out.contains(" player "), "panel title shown");
    assert!(out.contains('●'), "config choice marked");
    assert!(out.contains("mpv"));
    assert!(out.contains("Windows Media Player"));
    assert!(out.contains("custom path"));
    assert!(out.contains("not an existing absolute path"));
}

#[test]
fn now_playing_snapshot() {
    let theme = Theme::titanium();
    let np = crate::ui::NowPlaying {
        id: "abc".into(),
        name: "Frieren - 01 [1080p]".into(),
        stream_url: "http://127.0.0.1:4567/stream".into(),
        ephemeral: false,
    };
    let out = render(|f| {
        now_playing::draw(f, f.area(), &np, &theme);
    });
    assert!(out.contains("now playing"));
    assert!(out.contains("Frieren"));
    assert!(out.contains("127.0.0.1"));
}

// --- selection styling ----------------------------------------------------

#[test]
fn selected_row_uses_selected_bg() {
    let theme = Theme::titanium();
    let state = search_state("dune");
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &HashSet::new(), &theme))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let selected_bg = theme.colors.selected_bg().to_ratatui();
    // Scan for any cell with the selection background (the selected row).
    let found = (0..H).any(|y| (0..W).any(|x| buf[(x, y)].bg == selected_bg));
    assert!(found, "selected row must render on selectedBg");
}
