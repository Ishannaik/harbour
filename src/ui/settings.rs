//! Settings view (2.5): every `Config` value editable from the TUI.
//!
//! Pure paint like the other views: `draw` renders `Config` + the disabled
//! source set + [`SettingsState`] (selection / inline edit buffer) with the
//! theme; the app loop owns key dispatch and persistence (app.rs's
//! `SettingsActivate`/`SettingsType`/… arms). The row layout is shared with
//! the dispatch through [`row_count`], [`row_kind`], [`row_label`] and
//! [`text_field`], so the view and the key handler agree on what each row is
//! without duplicating the layout.
//!
//! Rows, in order: player (text), theme (cycles), download dir (text),
//! seed-by-default (toggle), trackers (text), then one toggle per source in
//! the search sidebar's matrix order (`SourceId::ALL` mirrors that matrix
//! exactly — the sidebar's private const is duplicated here as labels only).

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::core::types::SourceId;
use crate::persist::Config;
use crate::theme::{Theme, ThemeColors};

/// Panel title — same framing as the downloads view and the splash.
const TITLE: &str = " harbour — settings ";
/// Bottom hint — the actions that matter on this screen.
const HINT: &str = "↑/↓ move · enter edit/toggle · esc back";
/// Block cursor glyph at the end of an inline edit — the input's focus
/// marker, mirroring the search bar's.
const CURSOR: &str = "▌";
/// Rows before the per-source toggles: player, theme, download dir,
/// seed-by-default, trackers.
/// Total settings rows before the per-source toggles.
const APP_ROWS: usize = 17;
/// Fixed label column width — the value column gets the rest of the panel.
const LABEL_W: usize = 16;
/// Panel width: label column + a value column long enough for a download
/// dir, plus padding. Clamped to the terminal on narrow screens.
const PANEL_WIDTH: u16 = 62;

/// What a settings row is — the dispatch branches on this so a row's
/// behavior lives in one place instead of being re-derived from its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A free-text value edited inline (player, download dir, trackers).
    Text,
    /// Cycles the active theme through the installed ones.
    Theme,
    /// An immediate boolean toggle (seed by default).
    Toggle,
    /// One source enabled/disabled toggle.
    Source,
}

/// The text-editable rows, so the view and the dispatch agree on which
/// config field an edit buffer commits into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Player,
    DownloadDir,
    Trackers,
    DownloadLimit,
    UploadLimit,
    AltDownloadLimit,
    AltUploadLimit,
    MaxActiveDownloads,
    ListenPort,
    SocksProxy,
    SeedRatio,
}

/// Settings-overlay state: the row selection and the inline text-edit
/// buffer. Persistence lives in app.rs — this struct only tracks what the
/// view paints (ui-contract.md: views hold state, the loop owns mutations).
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    /// Selected row index; clamped to [`row_count`] by the app loop.
    pub selected: usize,
    /// A text row is being edited inline (`edit_buffer` is live).
    pub editing: bool,
    /// The text being edited while `editing`; committed on Enter, discarded
    /// on Esc or when the selection moves.
    pub edit_buffer: String,
    /// Installed theme names — "titanium" plus every `<state>/themes/*.json`
    /// custom theme, in cycle order. Refreshed when the overlay opens.
    pub themes: Vec<String>,
}

/// Total settings rows: the five app settings plus one per source.
pub fn row_count() -> usize {
    APP_ROWS + SourceId::ALL.len()
}

/// The kind of row at `index`, or `None` past the end.
pub fn row_kind(index: usize) -> Option<RowKind> {
    match index {
        0 | 2 | 4 | 5 | 6 | 7 | 8 | 10 | 11 | 14 | 16 => Some(RowKind::Text),
        1 => Some(RowKind::Theme),
        3 | 9 | 12 | 13 | 15 => Some(RowKind::Toggle),
        _ => {
            let source = index.checked_sub(APP_ROWS)?;
            SourceId::ALL.get(source).map(|_| RowKind::Source)
        }
    }
}

/// The text field edited by the text row at `index`, if it is one.
pub fn text_field(index: usize) -> Option<TextField> {
    match index {
        0 => Some(TextField::Player),
        2 => Some(TextField::DownloadDir),
        4 => Some(TextField::Trackers),
        5 => Some(TextField::DownloadLimit),
        6 => Some(TextField::UploadLimit),
        7 => Some(TextField::AltDownloadLimit),
        8 => Some(TextField::AltUploadLimit),
        10 => Some(TextField::MaxActiveDownloads),
        11 => Some(TextField::ListenPort),
        14 => Some(TextField::SocksProxy),
        16 => Some(TextField::SeedRatio),
        _ => None,
    }
}

/// The source a source row toggles, or `None` past the source rows.
pub fn source_at(index: usize) -> Option<SourceId> {
    SourceId::ALL.get(index.checked_sub(APP_ROWS)?).copied()
}

/// The user-facing label for row `index`. Source labels match the search
/// sidebar's matrix so the same source reads the same in both places.
pub fn row_label(index: usize) -> Option<&'static str> {
    match index {
        0 => Some("player"),
        1 => Some("theme"),
        2 => Some("download dir"),
        3 => Some("seed by default"),
        4 => Some("trackers"),
        5 => Some("download limit (MiB/s)"),
        6 => Some("upload limit (MiB/s)"),
        7 => Some("alt download limit (MiB/s)"),
        8 => Some("alt upload limit (MiB/s)"),
        9 => Some("use alt rates"),
        10 => Some("max active downloads (0 = unlimited)"),
        11 => Some("listen port (empty = auto)"),
        12 => Some("upnp port forwarding"),
        13 => Some("dht"),
        14 => Some("socks proxy"),
        15 => Some("stop seeding at ratio"),
        16 => Some("seed ratio"),
        _ => source_at(index).map(source_label),
    }
}

/// Sidebar label per source — mirrors `ui/search.rs`'s matrix labels.
fn source_label(id: SourceId) -> &'static str {
    match id {
        SourceId::Indexer => "Indexer",
        SourceId::FitGirl => "FitGirl",
        SourceId::Yts => "YTS",
        SourceId::TpbMovies => "TPB",
        SourceId::TpbTv => "TPB",
        SourceId::X1337Movies => "1337x",
        SourceId::X1337Tv => "1337x",
        SourceId::Eztv => "EZTV",
        SourceId::Nyaa => "Nyaa",
        SourceId::SubsPlease => "SubsPlease",
        SourceId::Bittorrented => "BitTorrented",
    }
}

/// Installed theme names: "titanium" first, then every custom
/// `<state>/themes/*.json` in sorted order. `theme_dir()` creates the
/// directory if it is missing, so a fresh install still lists titanium.
pub fn installed_themes() -> Vec<String> {
    let mut names = vec!["titanium".to_string()];
    let mut custom: Vec<String> = match std::fs::read_dir(crate::theme_watch::theme_dir()) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .filter(|name| name != "titanium")
            .collect(),
        Err(_) => Vec::new(),
    };
    custom.sort();
    names.extend(custom);
    names
}

/// Draws the settings overlay centred in `area`, over whatever screen is
/// underneath — pressing shift+S never loses the user's place.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    config: &Config,
    disabled: &HashSet<SourceId>,
    state: &SettingsState,
    theme: &Theme,
) {
    let colors = &theme.colors;
    let rows = row_count();

    // Panel geometry: the label column + a value column, clamped to the
    // terminal; blank + rows + blank + hint + two borders in height.
    let width = PANEL_WIDTH.min(area.width.saturating_sub(2).max(30));
    let height = (rows as u16 + 5).min(area.height);
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height,
    };

    let border = BorderSet {
        top_left: theme.symbols.border_tl.as_ref(),
        top_right: theme.symbols.border_tr.as_ref(),
        bottom_left: theme.symbols.border_bl.as_ref(),
        bottom_right: theme.symbols.border_br.as_ref(),
        vertical_left: theme.symbols.border_v.as_ref(),
        vertical_right: theme.symbols.border_v.as_ref(),
        horizontal_top: theme.symbols.border_h.as_ref(),
        horizontal_bottom: theme.symbols.border_h.as_ref(),
    };

    let mut lines = vec![Line::from("")];
    let visible = (height as usize).saturating_sub(5).min(rows);
    let value_width = (width as usize).saturating_sub(LABEL_W + 4);
    for index in 0..visible {
        lines.push(setting_line(
            index,
            config,
            disabled,
            state,
            colors,
            value_width,
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        HINT.to_string(),
        Style::default().fg(colors.muted().to_ratatui()),
    )));

    // Clear first: without it the screen underneath shows through the panel.
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_set(border)
                    .border_style(Style::default().fg(colors.border().to_ratatui()))
                    .title(Span::styled(
                        TITLE.to_string(),
                        Style::default().fg(colors.accent().to_ratatui()),
                    ))
                    .style(Style::default().bg(colors.bg().to_ratatui())),
            )
            .style(Style::default().bg(colors.bg().to_ratatui())),
        panel,
    );
}

/// One settings row: label (muted, accent when selected) + value (text,
/// accent when selected). The selected row's background is the theme's
/// selected_bg, so the highlight reads even where the colors switch.
fn setting_line(
    index: usize,
    config: &Config,
    disabled: &HashSet<SourceId>,
    state: &SettingsState,
    colors: &ThemeColors,
    value_width: usize,
) -> Line<'static> {
    let selected = index == state.selected;
    let bg = if selected {
        colors.selected_bg().to_ratatui()
    } else {
        colors.bg().to_ratatui()
    };
    let base = Style::default().bg(bg);
    let label_fg = if selected {
        colors.accent().to_ratatui()
    } else {
        colors.muted().to_ratatui()
    };
    let value_fg = if selected {
        colors.accent().to_ratatui()
    } else {
        colors.text().to_ratatui()
    };
    let label = row_label(index).unwrap_or("");
    Line::from(vec![
        Span::styled(format!("  {label:<LABEL_W$}"), base.fg(label_fg)),
        Span::styled(
            truncate(&row_value(index, config, disabled, state), value_width),
            base.fg(value_fg),
        ),
    ])
}

/// The current value text for row `index`: the config value, or the live
/// edit buffer + cursor while that row is being edited.
fn row_value(
    index: usize,
    config: &Config,
    disabled: &HashSet<SourceId>,
    state: &SettingsState,
) -> String {
    if state.editing && state.selected == index {
        return format!("{}{CURSOR}", state.edit_buffer);
    }
    match text_field(index) {
        Some(TextField::Player) => config.player.clone().unwrap_or_else(|| "auto".to_string()),
        Some(TextField::DownloadDir) => config.download_dir.display().to_string(),
        Some(TextField::Trackers) => {
            if config.trackers.is_empty() {
                "none".to_string()
            } else {
                config.trackers.join(", ")
            }
        }
        Some(TextField::DownloadLimit) => opt_mib(config.download_limit_mib),
        Some(TextField::UploadLimit) => opt_mib(config.upload_limit_mib),
        Some(TextField::AltDownloadLimit) => opt_mib(config.alt_download_limit_mib),
        Some(TextField::AltUploadLimit) => opt_mib(config.alt_upload_limit_mib),
        Some(TextField::MaxActiveDownloads) => config
            .max_active_downloads
            .map(|n| n.to_string())
            .unwrap_or_else(|| "env default".to_string()),
        Some(TextField::ListenPort) => config
            .listen_port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "auto".to_string()),
        Some(TextField::SocksProxy) => config
            .socks_proxy_url
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        Some(TextField::SeedRatio) => format!("{:.1}", config.seed_ratio),
        None => match index {
            1 => config.theme.clone(),
            3 => bool_glyph(config.seed_by_default),
            9 => bool_glyph(config.use_alt_rates),
            12 => bool_glyph(config.enable_upnp),
            13 => bool_glyph(config.enable_dht),
            15 => bool_glyph(config.stop_seed_at_ratio),
            _ => match source_at(index) {
                Some(id) => bool_glyph(!disabled.contains(&id)),
                None => String::new(),
            },
        },
    }
}

/// MiB/s limit as "unlimited" or the number — the empty-buffer round-trip
/// form for the settings text rows.
fn opt_mib(mib: Option<u64>) -> String {
    mib.map(|m| m.to_string())
        .unwrap_or_else(|| "unlimited".to_string())
}

/// ✓ for an on/checked value, ✗ for off/unchecked — the toggle rows' glyph.
fn bool_glyph(on: bool) -> String {
    if on {
        "✓".to_string()
    } else {
        "✗".to_string()
    }
}

/// Truncate to `max` cells, replacing the last with '…', so a long path or
/// tracker list never bleeds past the panel.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn row_layout_matches_the_settings_contract() {
        assert_eq!(row_count(), 17 + SourceId::ALL.len());
        assert_eq!(row_kind(0), Some(RowKind::Text));
        assert_eq!(text_field(0), Some(TextField::Player));
        assert_eq!(row_kind(1), Some(RowKind::Theme));
        assert_eq!(row_kind(2), Some(RowKind::Text));
        assert_eq!(text_field(2), Some(TextField::DownloadDir));
        assert_eq!(row_kind(3), Some(RowKind::Toggle));
        assert_eq!(row_kind(4), Some(RowKind::Text));
        assert_eq!(text_field(4), Some(TextField::Trackers));
        // The qBittorrent-parity rows.
        assert_eq!(row_kind(5), Some(RowKind::Text));
        assert_eq!(text_field(5), Some(TextField::DownloadLimit));
        assert_eq!(text_field(6), Some(TextField::UploadLimit));
        assert_eq!(text_field(7), Some(TextField::AltDownloadLimit));
        assert_eq!(text_field(8), Some(TextField::AltUploadLimit));
        assert_eq!(row_kind(9), Some(RowKind::Toggle));
        assert_eq!(text_field(10), Some(TextField::MaxActiveDownloads));
        assert_eq!(text_field(11), Some(TextField::ListenPort));
        assert_eq!(row_kind(12), Some(RowKind::Toggle));
        assert_eq!(row_kind(13), Some(RowKind::Toggle));
        assert_eq!(text_field(14), Some(TextField::SocksProxy));
        assert_eq!(row_kind(15), Some(RowKind::Toggle));
        assert_eq!(text_field(16), Some(TextField::SeedRatio));
        // Sources follow the app rows.
        assert_eq!(row_kind(17), Some(RowKind::Source));
        assert_eq!(row_kind(row_count() - 1), Some(RowKind::Source));
        assert_eq!(row_kind(row_count()), None);
        assert_eq!(text_field(1), None);
        assert_eq!(source_at(16), None);
        assert_eq!(source_at(row_count()), None);
    }

    #[test]
    fn source_rows_follow_the_search_sidebar_order() {
        // The settings sources mirror the sidebar matrix: FitGirl, YTS, TPB
        // movies, 1337x movies, BitTorrented, EZTV, TPB tv, 1337x tv, Nyaa,
        // SubsPlease — which is exactly SourceId::ALL, the registry's
        // canonical sidebar order.
        let got: Vec<SourceId> = (17..row_count()).filter_map(source_at).collect();
        assert_eq!(got, SourceId::ALL);
        assert_eq!(got[0], SourceId::FitGirl);
        assert_eq!(got[9], SourceId::SubsPlease);
    }

    #[test]
    fn source_labels_match_the_search_sidebar() {
        let labels: Vec<&str> = (17..row_count()).filter_map(row_label).collect();
        assert_eq!(
            labels,
            [
                "FitGirl",
                "YTS",
                "TPB",
                "1337x",
                "BitTorrented",
                "EZTV",
                "TPB",
                "1337x",
                "Nyaa",
                "SubsPlease"
            ]
        );
    }

    #[test]
    fn installed_themes_lists_titanium_first() {
        let names = installed_themes();
        assert_eq!(names.first().map(String::as_str), Some("titanium"));
        let mut sorted = names[1..].to_vec();
        sorted.sort();
        assert_eq!(&names[1..], sorted, "custom themes are sorted");
    }

    #[test]
    fn overlay_renders_labels_values_and_hint() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let cfg = config();
        let disabled = HashSet::new();
        let state = SettingsState::default();
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let frame = terminal
            .draw(|frame| draw(frame, frame.area(), &cfg, &disabled, &state, &theme))
            .expect("draw");
        let symbols: String = frame
            .buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for expected in [
            "harbour — settings",
            "player",
            "theme",
            "download dir",
            "seed by default",
            "trackers",
            "FitGirl",
            "titanium",
            "auto",
            "↑/↓ move · enter edit/toggle · esc back",
        ] {
            assert!(symbols.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn disabled_sources_render_as_off_and_toggles_as_glyphs() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let cfg = config();
        let mut disabled = HashSet::new();
        disabled.insert(SourceId::FitGirl);
        let state = SettingsState::default();
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let frame = terminal
            .draw(|frame| draw(frame, frame.area(), &cfg, &disabled, &state, &theme))
            .expect("draw");
        let symbols: String = frame
            .buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        // Defaults: seed-by-default ✓, upnp ✓, dht ✓ (3 toggles on);
        // use-alt-rates ✗, stop-at-ratio ✗; 9 sources enabled (✓), FitGirl
        // off (✗).
        assert_eq!(symbols.matches('✓').count(), 12);
        assert_eq!(symbols.matches('✗').count(), 3);
    }
}
