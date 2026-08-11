//! Now-playing view (FR-57..FR-59, phase 6): the watch screen shown while an
//! external player (mpv/VLC) plays the item's stream.
//!
//! Pure paint: `draw` renders the item + stream URL + hint; the app loop owns
//! the player lifecycle (launch on `w`, return on player exit or `q`/esc).
//! harbour ships no render engine — the external player is the renderer
//! (design.md §2.4).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme::Theme;
use crate::types::NowPlaying;

/// Title — same framing as the other views.
const TITLE: &str = " harbour — now playing ";
/// Bottom hint.
const HINT: &str = "q / esc back to the TUI";

/// Renders the now-playing screen: item name, the loopback stream URL the
/// player opened, and the hint. Player transport (seek/volume) lives in the
/// player; this view is a status screen.
pub fn draw(frame: &mut Frame, area: Rect, state: &NowPlaying, theme: &Theme) {
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();

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
        .title(Span::styled(TITLE, Style::default().fg(accent)))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            "streaming to your player…".to_string(),
            Style::default().fg(colors.success().to_ratatui()),
        )),
        Line::default(),
        Line::from(Span::styled(
            state.name.clone(),
            Style::default().fg(colors.text().to_ratatui()),
        )),
        Line::from(Span::styled(
            state.stream_url.clone(),
            Style::default().fg(colors.muted().to_ratatui()),
        )),
        Line::default(),
        Line::from(Span::styled(
            HINT.to_string(),
            Style::default().fg(colors.muted().to_ratatui()),
        )),
    ];
    lines.push(Line::default());
    frame.render_widget(Paragraph::new(lines), inner);
}
