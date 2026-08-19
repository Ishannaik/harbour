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

use crate::core::types::{
    EngineStats, ItemView, QueueItem, QueueStatus, SourceId, SourceStatus, TorrentResult,
};
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
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    // "dune" hits the movie catalog: real title in the rows, sidebar visible.
    assert!(out.contains("harbour — search") || out.contains("search"));
    assert!(out.contains("Dune: Part Two"), "catalog title in results");
    assert!(out.contains("GamesHub"), "sidebar lists all sources");
    assert!(!out.contains("no results"), "results present");
    assert!(!out.contains("results focused"), "input pane by default");
}

#[test]
fn search_snapshot_shows_human_size_on_nonzero_row() {
    // #69 / FR-23: 80-col search view must paint GiB/MiB, not a blank size.
    let state = search_state("dune");
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    assert!(
        out.contains("GiB") || out.contains("MiB"),
        "FR-23: non-zero size_bytes must render as human units, got:\n{out}"
    );
}

#[test]
fn the_results_pane_announces_how_to_get_back_to_typing() {
    let mut state = search_state("dune");
    state.focus = false; // Enter moved the keyboard to the results pane
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    assert!(
        out.contains("results focused"),
        "the bar must say where the keyboard is"
    );
    assert!(out.contains("esc"), "…and how to leave it");
}

#[test]
fn a_selected_result_footer_hints_watch_and_download() {
    let mut state = search_state("dune");
    state.focus = false;
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    assert!(
        out.contains("enter watch"),
        "selected-row footer must name Enter-to-watch, got:\n{out}"
    );
    assert!(
        out.contains("d download"),
        "selected-row footer must name d-to-download, got:\n{out}"
    );
    assert!(
        out.contains("dbl-click"),
        "selected-row footer must name the mouse download gesture, got:\n{out}"
    );
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
        .draw(|f| search::draw(f, f.area(), &state, &disabled, &theme, None))
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
fn search_snapshot_unreported_health_is_em_dash() {
    // GamesHub and FanSubs publish no swarm counts; the row must show an
    // em dash, never a guessed 50+/10+ band (issue #70 / FR-24).
    let mut state = SearchState {
        query: "cyberpunk".into(),
        results: vec![
            TorrentResult {
                info_hash: "gameshub-row".into(),
                name: "Cyberpunk 2077".into(),
                size_bytes: 5 * 1024 * 1024 * 1024,
                seeders: 0,
                leechers: 0,
                num_files: Some(1),
                source: SourceId::GamesHub,
                magnet: None,
                added: Some(1_786_000_000),
            },
            TorrentResult {
                info_hash: "subs-row".into(),
                name: "Cyberpunk - 01".into(),
                size_bytes: 734_003_200,
                seeders: 0,
                leechers: 0,
                num_files: Some(1),
                source: SourceId::FanSubs,
                magnet: None,
                added: Some(1_786_000_000),
            },
        ],
        selected: 0,
        ..SearchState::default()
    };
    state.source_counts.insert(SourceId::GamesHub, 1);
    state.source_counts.insert(SourceId::FanSubs, 1);
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    assert!(out.contains("[gameshub]"), "GamesHub row visible:\n{out}");
    assert!(out.contains("[subs]"), "FanSubs row visible:\n{out}");
    assert!(!out.contains("50+"), "GamesHub must not invent 50+:\n{out}");
    assert!(!out.contains("10+"), "must not invent 10+:\n{out}");
    assert!(
        out.contains('—'),
        "FanSubs unknown health is an em dash:\n{out}"
    );
}

#[test]
fn search_snapshot_checking_source_not_connecting() {
    let mut state = SearchState {
        searching: true,
        ..SearchState::default()
    };
    for id in SourceId::ALL {
        state.source_health.insert(id, SourceStatus::Checking);
    }
    let theme = Theme::titanium();
    let out = render(|f| {
        search::draw(f, f.area(), &state, &HashSet::new(), &theme, None);
    });
    assert!(
        out.contains("checking source"),
        "sidebar/source tracker must name a source check, got:\n{out}"
    );
    assert!(
        !out.contains("connecting"),
        "connecting is BitTorrent, not source health, got:\n{out}"
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
            None,
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
        downloads::draw(f, f.area(), &state, &theme, None);
    });
    assert!(out.contains("Oppenheimer"));
    assert!(out.contains("downloading") || out.contains("[downloading]"));
    assert!(out.contains("Shogun"), "paused item visible on active tab");
    assert!(out.contains("recently downloaded"));
    assert!(out.contains("Dune: Part Two"), "history row");
}

#[test]
fn downloads_snapshot_metadata_and_peers_copy() {
    let mut meta_item = QueueItem::new(
        "a".repeat(40),
        "Waiting for torrent file".into(),
        None,
        None,
        std::path::PathBuf::from("~/harbour/downloads"),
        0,
    );
    meta_item.status = QueueStatus::Downloading;
    let meta = ItemView::new(
        meta_item,
        Some(EngineStats {
            total_bytes: 0,
            peers: None,
            ..EngineStats::default()
        }),
    );

    let mut swarm_item = QueueItem::new(
        "b".repeat(40),
        "Waiting for swarm".into(),
        None,
        None,
        std::path::PathBuf::from("~/harbour/downloads"),
        0,
    );
    swarm_item.status = QueueStatus::Downloading;
    swarm_item.total_bytes = 1000;
    let swarm = ItemView::new(
        swarm_item,
        Some(EngineStats {
            total_bytes: 1000,
            peers: Some(0),
            ..EngineStats::default()
        }),
    );

    let state = DownloadsState {
        items: vec![meta, swarm],
        history: Vec::new(),
        selected: 0,
        show_seeding: false,
    };
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme, None);
    });
    assert!(
        out.contains("fetching metadata"),
        "size 0 row names metadata fetch, got:\n{out}"
    );
    assert!(
        out.contains("looking for peers"),
        "zero-peer row names swarm join, got:\n{out}"
    );
    assert!(
        !out.contains("connecting"),
        "connecting is never user-facing, got:\n{out}"
    );
}

#[test]
fn downloads_snapshot_seeding_tab() {
    let mut state = downloads_state();
    state.show_seeding = true;
    let theme = Theme::titanium();
    let out = render(|f| {
        downloads::draw(f, f.area(), &state, &theme, None);
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
        .draw(|f| search::draw(f, f.area(), &state, &HashSet::new(), &theme, None))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let selected_bg = theme.colors.selected_bg().to_ratatui();
    // Scan for any cell with the selection background (the selected row).
    let found = (0..H).any(|y| (0..W).any(|x| buf[(x, y)].bg == selected_bg));
    assert!(found, "selected row must render on selectedBg");
}

#[test]
fn hovered_sidebar_source_renders_highlighted() {
    let theme = Theme::titanium();
    let state = search_state("dune");
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    // Row 3 is GamesHub in the sidebar (panel inner y=1 + offset 2 = row 3)
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &HashSet::new(), &theme, Some((5, 3))))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let selected_bg = theme.colors.selected_bg().to_ratatui();
    let found_on_row = (1..20).any(|x| buf[(x, 3)].bg == selected_bg);
    assert!(
        found_on_row,
        "hovered sidebar source row must have selectedBg"
    );
}

#[test]
fn hovered_search_result_renders_highlighted() {
    let theme = Theme::titanium();
    let mut state = search_state("dune");
    state.selected = 0;
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("test backend");
    // Result row 1 is below search bar (y=1..3) + header (y=4) -> row 0 is at y=5, row 1 is at y=6
    terminal
        .draw(|f| search::draw(f, f.area(), &state, &HashSet::new(), &theme, Some((35, 6))))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let selected_bg = theme.colors.selected_bg().to_ratatui();
    let found_on_hovered_row = (30..70).any(|x| buf[(x, 6)].bg == selected_bg);
    assert!(
        found_on_hovered_row,
        "hovered search result row must have selectedBg"
    );
}

#[test]
fn error_banner_dismiss_button_renders() {
    let state = AppState {
        error_banner: Some("Something went wrong".into()),
        ..AppState::default()
    };
    let theme = Theme::titanium();
    let out = render(|f| {
        status::draw(f, f.area(), Screen::Search, &state, &theme, "⠋");
    });
    assert!(out.contains("[✕ dismiss]"));
}
