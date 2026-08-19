//! The keybind overlay (`?`).
//!
//! Rendered as a centred panel over whatever screen is underneath, so pressing
//! `?` never loses the user's place.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// Every binding, in the order a new user meets them.
///
/// This is the single source of truth for the overlay. `UR-10` requires `?` to
/// show exactly the bindings the app implements, so a keybind added to the
/// input handler without a row here is a documentation bug that a test catches.
pub const BINDINGS: &[(&str, &str)] = &[
    // Search input pane: every key types; Enter moves to the results pane.
    ("enter", "input: run the search · results: watch now"),
    ("↑ ↓", "move the selection"),
    ("tab", "switch between search and downloads"),
    // Search results pane: plain keys act on the selected row.
    ("d", "search results: download the selected row"),
    ("shift+D", "search results: download to a folder you pick"),
    ("o", "change the default download folder"),
    ("w", "watch — results: stream now · downloads: watch item"),
    ("s", "search results: settings · downloads: seeding tab"),
    (
        "esc",
        "results → search input · input: clear query · overlays & folder prompt: close",
    ),
    (
        "type",
        "results → back to the input, typing what you pressed",
    ),
    // Downloads screen.
    (
        "← →",
        "downloads: switch between the downloads and seeding tabs",
    ),
    ("p", "downloads: pause or resume the selected item"),
    ("r", "downloads: retry a failed item"),
    ("x", "downloads: remove the selected item (keeps files)"),
    (
        "shift+P",
        "anywhere: choose the watch player (mpv/VLC/Windows Media Player)",
    ),
    ("shift+S", "downloads: open settings"),
    ("?", "help — when not typing a query"),
    ("q", "quit — anywhere except while typing a query"),
    ("ctrl+c", "quit"),
];

/// One ELI5 line per field in a search result row — the "reading results"
/// half of the overlay. Keys mirror the header labels and the row's chips.
pub const READING_RESULTS: &[(&str, &str)] = &[
    ("seeds", "people sharing the file — more = faster"),
    ("leeches", "people downloading from them — more = slower"),
    ("size", "how big the download is (GB/MB)"),
    ("quality", "[1080p] [4k] tags read from the release name"),
    ("[source]", "which site the result came from"),
    ("dot", "health: green = up, red = down, · = unknown"),
];

/// Draws the overlay centred in `area`.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let colors = &theme.colors;

    // Wide enough for the longest row (bindings or legend), but never wider
    // than the terminal.
    let width = BINDINGS
        .iter()
        .chain(READING_RESULTS)
        .map(|(k, d)| k.chars().count() + d.chars().count() + 6)
        .max()
        .unwrap_or(40)
        .clamp(30, area.width.saturating_sub(4).max(30) as usize) as u16;
    let height = (BINDINGS.len() + READING_RESULTS.len() + 5).min(area.height as usize) as u16;
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

    let key_width = BINDINGS
        .iter()
        .chain(READING_RESULTS)
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(5);
    let mut lines = vec![Line::from("")];
    for (key, description) in BINDINGS {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{key:>key_width$}"),
                Style::default().fg(colors.accent().to_ratatui()),
            ),
            Span::raw("  "),
            Span::styled(
                (*description).to_string(),
                Style::default().fg(colors.text().to_ratatui()),
            ),
        ]));
    }
    // The "reading results" half: a divider, then one ELI5 line per field.
    let div = " reading results ";
    let fill = theme.symbols.border_h.as_ref().repeat(
        panel
            .width
            .saturating_sub(2)
            .saturating_sub(div.chars().count() as u16)
            .max(1) as usize,
    );
    lines.push(Line::from(Span::styled(
        format!("{div}{fill}"),
        Style::default().fg(colors.muted().to_ratatui()),
    )));
    for (key, description) in READING_RESULTS {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{key:>key_width$}"),
                Style::default().fg(colors.accent().to_ratatui()),
            ),
            Span::raw("  "),
            Span::styled(
                (*description).to_string(),
                Style::default().fg(colors.text().to_ratatui()),
            ),
        ]));
    }

    // Clear first: without it the screen underneath shows through the panel.
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_set(border)
                .border_style(Style::default().fg(colors.border().to_ratatui()))
                .title(Span::styled(
                    " keys ",
                    Style::default().fg(colors.accent().to_ratatui()),
                ))
                .title(
                    Line::from(Span::styled(
                        " [✕] ",
                        Style::default().fg(colors.error().to_ratatui()),
                    ))
                    .alignment(ratatui::layout::Alignment::Right),
                )
                .style(Style::default().bg(colors.bg().to_ratatui())),
        ),
        panel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_is_documented_once() {
        let mut keys: Vec<&str> = BINDINGS.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "a key is listed twice in the help");
        assert!(BINDINGS.iter().all(|(_, d)| !d.is_empty()));
    }

    #[test]
    fn the_bindings_users_reach_for_first_are_present() {
        // UR-10: `?` must show exactly what the app implements. These are the
        // ones a user cannot discover any other way.
        for key in ["enter", "d", "shift+D", "o", "← →", "p", "q", "?", "tab"] {
            assert!(
                BINDINGS.iter().any(|(k, _)| *k == key),
                "`{key}` is implemented but undocumented"
            );
        }
    }

    #[test]
    fn reading_results_legend_lists_every_field_once() {
        let mut keys: Vec<&str> = READING_RESULTS.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "a legend key is listed twice");
        assert!(READING_RESULTS.iter().all(|(_, d)| !d.is_empty()));
        for key in ["seeds", "leeches", "size", "quality", "[source]", "dot"] {
            assert!(
                READING_RESULTS.iter().any(|(k, _)| *k == key),
                "legend missing `{key}`"
            );
        }
    }
}
