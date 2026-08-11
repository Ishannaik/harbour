//! Help overlay (design.md §2.2): a centered modal listing the keybinds,
//! styled with the theme's markdown tokens. `?` opens it, Esc/`?`/`q` close
//! it. Pure paint; the app decides when it is visible.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::border_set;
use crate::theme::Theme;

/// (key, action) pairs — normative keybinds (design.md §1) plus the phase-2
/// screen switcher.
const KEYS: &[(&str, &str)] = &[
    ("enter", "search — empty query opens curated top lists"),
    ("↑/↓", "navigate results"),
    ("←/→", "results ↔ sidebar filter"),
    ("tab", "search ↔ downloads"),
    ("d", "download selected to default folder"),
    ("shift+d", "download selected to a folder"),
    ("o", "change output folder"),
    ("p", "pause / stop seed"),
    ("?", "help"),
    ("q", "quit"),
];

/// Paints the help modal centered over the current screen.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let width = area.width.min(64);
    let content_lines: Vec<Line> = KEYS
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<10}"),
                    Style::default().fg(theme.colors.syntax_string().to_ratatui()),
                ),
                Span::styled(
                    action.to_string(),
                    Style::default().fg(theme.colors.text().to_ratatui()),
                ),
            ])
        })
        .collect();

    let header = Line::from(vec![
        Span::styled(
            " harbour ",
            Style::default().fg(theme.colors.accent().to_ratatui()),
        ),
        Span::styled(
            "— keybinds",
            Style::default().fg(theme.colors.md_heading().to_ratatui()),
        ),
    ]);
    let footer = Line::from(vec![
        Span::styled(
            "  esc ",
            Style::default().fg(theme.colors.syntax_string().to_ratatui()),
        ),
        Span::styled(
            "closes · downloads are simulated until the engine lands",
            Style::default().fg(theme.colors.dim().to_ratatui()),
        ),
    ]);

    let mut lines = Vec::new();
    lines.push(Line::default());
    lines.push(header);
    lines.push(Line::default());
    lines.extend(content_lines);
    lines.push(Line::default());
    lines.push(footer);
    lines.push(Line::default());

    let content_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let box_w = (content_w + 2).min(width);
    let box_h = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;

    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(theme))
        .border_style(Style::default().fg(theme.colors.border_accent().to_ratatui()))
        .style(Style::default().bg(theme.colors.bg().to_ratatui()));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .style(Style::default().bg(theme.colors.bg().to_ratatui())),
        Rect::new(x, y, box_w, box_h),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn help_lists_keybinds() {
        let theme = Theme::titanium();
        let backend = TestBackend::new(70, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, f.area(), &theme)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut joined = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                joined.push_str(buffer[(x, y)].symbol());
            }
            joined.push('\n');
        }
        assert!(joined.contains("keybinds"), "title");
        for (key, _) in KEYS {
            assert!(joined.contains(key), "key {key} listed");
        }
        assert!(joined.contains("download selected"), "action listed");
    }
}
