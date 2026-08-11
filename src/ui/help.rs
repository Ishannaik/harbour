//! Help overlay (UR-10): a centered modal listing the normative keybinds.
//!
//! Pure paint: `draw` renders the keybind table + theme; the app loop owns
//! input (any key closes the overlay, `q`/Ctrl+C still quit). All colors
//! come from the theme subset (docs/theming.md), so custom themes work
//! unchanged.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme::Theme;

/// Keybind rows, key first then the action — the exact set from
/// docs/design.md §Keybinds plus the two screen-nav keys added for the
/// multi-view loop (Tab cycles screens, ←/→ switch the downloads tabs).
const ROWS: &[(&str, &str)] = &[
    ("enter", "search (empty = browse curated lists)"),
    ("↑ / ↓", "move selection"),
    ("d / shift+d", "download to default / chosen folder"),
    ("tab", "switch screen"),
    ("← / →", "switch downloads tab"),
    ("p", "pause / resume"),
    ("?", "close help"),
    ("esc", "close help"),
    ("q / ctrl+c", "quit"),
];

/// Draws the help modal centered over the current view.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let colors = &theme.colors;
    let accent = colors.accent().to_ratatui();

    // Key column width = longest key + padding; the modal wraps the table.
    let key_w = ROWS
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        + 3;
    let body_w = key_w
        + ROWS
            .iter()
            .map(|(_, a)| a.chars().count())
            .max()
            .unwrap_or(0);
    let modal_w = ((body_w + 4) as u16).min(area.width.saturating_sub(4));
    let modal_h = ((ROWS.len() + 2) as u16).min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(modal_w) / 2;
    let y = area.y + area.height.saturating_sub(modal_h) / 2;

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
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border)
        .border_style(Style::default().fg(colors.border().to_ratatui()))
        .title(Span::styled(" keybinds ", Style::default().fg(accent)))
        .style(Style::default().bg(colors.selected_bg().to_ratatui()));

    let inner = block.inner(Rect::new(x, y, modal_w, modal_h));
    let chunks =
        Layout::horizontal([Constraint::Length(key_w as u16), Constraint::Min(0)]).split(inner);
    let mut keys: Vec<Line> = Vec::new();
    let mut actions: Vec<Line> = Vec::new();
    for (k, a) in ROWS {
        keys.push(Line::from(Span::styled(
            k.to_string(),
            Style::default().fg(accent),
        )));
        actions.push(Line::from(Span::styled(
            a.to_string(),
            Style::default().fg(colors.muted().to_ratatui()),
        )));
    }
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().bg(colors.selected_bg().to_ratatui())),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(actions).style(Style::default().bg(colors.selected_bg().to_ratatui())),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Line::default())
            .block(block)
            .style(Style::default().bg(colors.selected_bg().to_ratatui())),
        Rect::new(x, y, modal_w, modal_h),
    );
}
