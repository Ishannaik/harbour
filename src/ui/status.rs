//! Status line (design.md §2.2/§2.3) and the omp-style error banner (§8).
//!
//! The status line sits inside each screen's outer frame: left status text
//! (with the streaming spinner), an eased answered-sources bar, and
//! right-aligned hints. The error banner is a full-width strip rendered over
//! the top of the screen — red background, never silent.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::Theme;

/// What the status line should show for the current screen. Bundled so the
/// draw call stays small (clippy: too-many-arguments).
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusLine<'a> {
    /// Left status text (with spinner, when present).
    pub left: String,
    /// Right-aligned hints.
    pub right: &'a str,
    /// Eased answered-sources fraction plus its label (e.g. `0.7, "7/10"`).
    /// The app loop eases the fraction so the bar never jumps (design.md §3).
    pub answered: Option<(f64, String)>,
    /// Current status-spinner frame (80ms cadence, advanced by the loop).
    pub spinner: Option<&'a str>,
}

/// Paints the themed status line.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme, line: &StatusLine) {
    if area.height == 0 {
        return;
    }
    let bg = theme.colors.status_line_bg().to_ratatui();
    let mut spans: Vec<Span<'static>> = Vec::new();

    if let Some(spin) = line.spinner {
        spans.push(Span::styled(
            format!("{spin} "),
            Style::default()
                .fg(theme.colors.accent().to_ratatui())
                .bg(bg),
        ));
    }
    spans.push(Span::styled(
        line.left.clone(),
        Style::default().fg(theme.colors.text().to_ratatui()).bg(bg),
    ));

    if let Some((frac, label)) = &line.answered {
        let frac = frac.clamp(0.0, 1.0);
        let bar_w = 12usize;
        let filled = (frac * bar_w as f64).round() as usize;
        let mut bar = String::new();
        for i in 0..bar_w {
            bar.push_str(if i < filled {
                &theme.symbols.progress_fill
            } else {
                &theme.symbols.progress_empty
            });
        }
        spans.push(Span::styled(
            format!("  {bar} "),
            Style::default()
                .fg(theme.colors.success().to_ratatui())
                .bg(bg),
        ));
        spans.push(Span::styled(
            label.clone(),
            Style::default()
                .fg(theme.colors.muted().to_ratatui())
                .bg(bg),
        ));
    }

    let left_w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let right_w = line.right.chars().count();
    let pad = area
        .width
        .saturating_sub(left_w as u16)
        .saturating_sub(right_w as u16);
    if pad > 1 {
        spans.push(Span::styled(
            " ".repeat(pad as usize),
            Style::default().bg(bg),
        ));
    }
    spans.push(Span::styled(
        line.right.to_string(),
        Style::default()
            .fg(theme.colors.muted().to_ratatui())
            .bg(bg),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        area,
    );
}

/// Paints the error banner: full-width, error background, message inline.
/// Rendered on top of the screen until the next action clears it (app.rs).
pub fn draw_error(frame: &mut Frame, area: Rect, theme: &Theme, msg: &str) {
    let bg = theme.colors.error().to_ratatui();
    let fg = theme.colors.text().to_ratatui();
    let line = Line::from(vec![
        Span::styled(
            " ⚠ ",
            Style::default()
                .fg(fg)
                .bg(bg)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(msg.to_string(), Style::default().fg(fg).bg(bg)),
    ]);
    let strip = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), strip);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render<F: FnOnce(&mut Frame)>(w: u16, h: u16, f: F) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| f(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn status_shows_left_right_and_answered_bar() {
        let theme = Theme::titanium();
        let lines = render(60, 1, |f| {
            draw(
                f,
                f.area(),
                &theme,
                &StatusLine {
                    left: " streaming…".to_string(),
                    right: "? help",
                    answered: Some((0.7, "7/10".to_string())),
                    spinner: Some("⠋"),
                },
            )
        });
        let line = lines[0].clone();
        assert!(line.contains("⠋"), "spinner");
        assert!(line.contains("streaming"), "left");
        assert!(line.contains("? help"), "right");
        assert!(line.contains("7/10"), "answered label");
        assert!(line.contains('█'), "filled bar cells");
    }

    #[test]
    fn error_banner_renders_message() {
        let theme = Theme::titanium();
        let lines = render(50, 1, |f| {
            draw_error(f, f.area(), &theme, "source tpb-movies timed out")
        });
        assert!(lines[0].contains("source tpb-movies timed out"), "message");
        assert!(lines[0].contains("⚠"), "warning glyph");
    }
}
