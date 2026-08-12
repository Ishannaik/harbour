//! Status line + error banner — the shared bottom bar for every phase-2 view.
//!
//! Bottom row on `statusLineBg`: screen label (accent) · separator (border) ·
//! context (muted) · right-aligned spinner glyph. When `AppState::error_banner`
//! is set, an error-bordered block (1-2 content rows) claims the rows directly
//! above, on a dark `selectedBg` fill standing in for omp's `toolErrorBg`.
//!
//! Views are pure paint (ui-contract.md): the spinner glyph is a parameter
//! because the app loop owns the animation clock — it advances the theme
//! spinner every 80ms (docs/design.md §Animation) and passes the current
//! frame in. This module never touches time or mutates state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::types::{ItemView, QueueStatus};
use crate::theme::Theme;
use crate::ui::{AppState, Screen};

/// Left (label, accent) and middle (context, muted) segments for a screen.
fn segments(screen: Screen, state: &AppState) -> (&'static str, String) {
    match screen {
        Screen::Splash => ("splash", "raising anchor…".to_string()),
        Screen::Search => {
            let context = if state.search.query.is_empty() {
                "browse curated lists".to_string()
            } else {
                state.search.query.clone()
            };
            ("search", context)
        }
        Screen::Downloads => ("downloads", download_context(&state.downloads.items)),
        Screen::Help => ("help", "press ? to close".to_string()),
        // The settings view is a modal over the current screen (2.5); this
        // arm exists for the exhaustive match — the status bar shows the
        // screen underneath while the overlay is up.
        Screen::Settings => ("settings", "esc to close".to_string()),
        Screen::NowPlaying => (
            "now playing",
            state
                .now_playing
                .as_ref()
                .map(|n| n.name.clone())
                .unwrap_or_default(),
        ),
    }
}

/// Downloads context mirrors the design's summary ("2 active · 1 seeding ·
/// 1 failed"): only nonzero segments are printed; an empty queue says so
/// instead of showing a wall of zeros.
fn download_context(items: &[ItemView]) -> String {
    let active = items
        .iter()
        .filter(|i| {
            matches!(
                i.item.status,
                QueueStatus::Queued | QueueStatus::Downloading
            )
        })
        .count();
    let seeding = items
        .iter()
        .filter(|i| i.item.status == QueueStatus::Seeding)
        .count();
    let failed = items
        .iter()
        .filter(|i| i.item.status == QueueStatus::Failed)
        .count();
    let mut parts = Vec::new();
    if active > 0 {
        parts.push(format!("{active} active"));
    }
    if seeding > 0 {
        parts.push(format!("{seeding} seeding"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if parts.is_empty() {
        "queue empty".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Truncates to `width` columns, cutting on char boundaries and appending an
/// ellipsis so a clipped message reads as clipped, not chopped. Char count
/// approximates display width — fine for ASCII contexts; any wide-glyph
/// overrun is clipped by the Paragraph anyway.
fn truncate_to(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// Draws the bottom status line plus, when `state.error_banner` is set, the
/// error banner above it.
///
/// Pure paint — no input handling, no mutation (ui-contract.md).
/// `spinner_glyph` is the theme spinner's current frame: the app loop
/// advances it every 80ms and passes the glyph in, keeping this view
/// stateless.
///
/// The status line owns the bottom row — one merged `Line` on status_line_bg,
/// padded to the full row width with bg-colored spaces, so the bar repaints
/// in a single pass and differential rendering never leaves stale cells. The
/// banner claims the rows above it: error-colored border and text on a dark
/// selected_bg fill.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    screen: Screen,
    state: &AppState,
    theme: &Theme,
    spinner_glyph: &str,
) {
    let colors = &theme.colors;
    // Banner height = 1-2 content rows (a message may span newlines) plus the
    // two border rows; `str::lines` yields nothing for "", hence the clamp.
    let banner_h = state
        .error_banner
        .as_ref()
        .map_or(0, |msg| 2 + msg.lines().count().clamp(1, 2) as u16);

    // Top region is Min(0) so it absorbs the shrink first when the terminal
    // is shorter than banner + status; the Lengths are preferred sizes.
    let mut constraints = vec![Constraint::Min(0)];
    if banner_h > 0 {
        constraints.push(Constraint::Length(banner_h));
    }
    constraints.push(Constraint::Length(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let status_area = chunks[chunks.len() - 1];

    // --- status line ----------------------------------------------------
    let status_bg = colors.status_line_bg().to_ratatui();
    let (label, raw_context) = segments(screen, state);
    let sep = format!(" {} ", theme.symbols.border_v);
    let spinner_w = spinner_glyph.chars().count();
    let avail = status_area.width as usize;
    // Reserve label + separator + spinner + one gap column for the
    // right-aligned glyph; the context (ellipsized when overlong) gets the
    // rest, so the spinner always survives a long query.
    let context_w =
        avail.saturating_sub(label.chars().count() + sep.chars().count() + spinner_w + 1);
    let context = truncate_to(&raw_context, context_w);
    let used = label.chars().count() + sep.chars().count() + context.chars().count() + spinner_w;
    let fill = avail.saturating_sub(used); // bg-colored pad; spinner hugs the right edge

    let line = Line::from(vec![
        Span::styled(label, Style::default().fg(colors.accent().to_ratatui())),
        Span::styled(sep, Style::default().fg(colors.border().to_ratatui())),
        Span::styled(context, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" ".repeat(fill)),
        Span::styled(
            spinner_glyph.to_string(),
            Style::default().fg(colors.accent().to_ratatui()),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(status_bg)),
        status_area,
    );

    // --- error banner -----------------------------------------------------
    if let Some(msg) = &state.error_banner {
        let banner_area = chunks[chunks.len() - 2];
        // Content shares the error fg; the block's own style paints the whole
        // banner (borders included) with the dark fill, so it reads as one
        // solid bar rather than a bordered hole.
        let inner_width = banner_area.width.saturating_sub(2) as usize;
        let content: Vec<Line> = msg
            .lines()
            .take(2)
            .map(|l| {
                Line::from(Span::styled(
                    truncate_to(l, inner_width),
                    Style::default().fg(colors.error().to_ratatui()),
                ))
            })
            .collect();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors.error().to_ratatui()))
            .title(Span::styled(
                " error ",
                Style::default().fg(colors.error().to_ratatui()),
            ))
            .style(Style::default().bg(colors.selected_bg().to_ratatui()));
        frame.render_widget(
            Paragraph::new(content)
                .block(block)
                .style(Style::default().bg(colors.selected_bg().to_ratatui())),
            banner_area,
        );
    }
}
