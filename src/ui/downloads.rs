//! Downloads view (design.md §2.3): Active/Seeding tabs over the queue,
//! eased animated progress bars, speed/peers/ETA, inline failures, and the
//! recently-downloaded list.
//!
//! Pure paint like the other views: the app loop owns easing (`DisplayState`)
//! and queue mutation; this module turns a snapshot into pixels.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{DisplayState, FrameVars, border_set, human_duration, human_size, human_speed};
use crate::theme::{Color, Theme};
use crate::types::{DownloadsState, QueueItem, QueueStatus};

/// The fake concurrency cap shown in the queue line (HARBOUR_MAX_DOWNLOADS=2).
const MAX_DOWNLOADS: usize = 2;

/// Renders an eased progress bar from theme symbols: filled, a half-cell at
/// the boundary, then empty. Width in cells, value 0..=1 (already eased).
fn progress_bar(theme: &Theme, width: usize, value: f64, color: Color) -> Line<'static> {
    let value = value.clamp(0.0, 1.0);
    let filled = (value * width as f64).floor() as usize;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for i in 0..width {
        let (glyph, fg) = if i < filled {
            (&theme.symbols.progress_fill, color)
        } else if i == filled && (value * width as f64 - filled as f64) > 0.01 {
            (&theme.symbols.progress_half, color)
        } else {
            (&theme.symbols.progress_empty, theme.colors.dim())
        };
        spans.push(Span::styled(
            glyph.to_string(),
            Style::default().fg(fg.to_ratatui()),
        ));
    }
    Line::from(spans)
}

fn status_color(theme: &Theme, status: QueueStatus) -> Color {
    match status {
        QueueStatus::Downloading => theme.colors.accent(),
        QueueStatus::Queued => theme.colors.muted(),
        QueueStatus::Paused => theme.colors.warning(),
        QueueStatus::Seeding => theme.colors.success(),
        QueueStatus::Failed | QueueStatus::Missing => theme.colors.error(),
    }
}

/// First line of a queue item: name + right-aligned status word.
fn name_line(theme: &Theme, item: &QueueItem, selected: bool) -> Line<'static> {
    let name_style = if selected {
        Style::default()
            .fg(theme.colors.accent().to_ratatui())
            .bg(theme.colors.selected_bg().to_ratatui())
    } else {
        Style::default().fg(theme.colors.text().to_ratatui())
    };
    let status = match item.status {
        QueueStatus::Downloading => "downloading",
        QueueStatus::Queued => "queued",
        QueueStatus::Paused if item.finished => "stopped seeding",
        QueueStatus::Paused => "paused",
        QueueStatus::Seeding => "seeding",
        QueueStatus::Failed => "failed",
        QueueStatus::Missing => "missing",
    };
    let mut line = Line::default();
    line.push_span(Span::styled(item.name.clone(), name_style));
    line.push_span(Span::raw(" "));
    line.push_span(Span::styled(
        status.to_string(),
        Style::default().fg(status_color(theme, item.status).to_ratatui()),
    ));
    line
}

/// Detail line for an active download: bar, percent, speed, ETA, peers.
fn active_detail<'a>(
    theme: &Theme,
    item: &QueueItem,
    display: &DisplayState,
    width: usize,
) -> Line<'a> {
    let progress = display.progress.get(&item.id).copied().unwrap_or(0.0);
    let pct = (progress * 100.0).round() as u64;
    let bar_w = width.saturating_sub(30).max(4);
    let peers = item
        .peers
        .map(|p| format!("⬆{p}"))
        .unwrap_or_else(|| "⬆—".to_string());
    let eta = item
        .eta_secs
        .map(human_duration)
        .unwrap_or_else(|| "—".to_string());
    let mut line = Line::default();
    line.push_span(Span::styled("  ", Style::default()));
    // The bar is a Line of per-cell spans; splice them into the detail line
    // (push_span only takes a Span).
    let bar = progress_bar(theme, bar_w, progress, theme.colors.accent());
    line.spans.extend(bar.spans);
    line.push_span(Span::styled(
        format!(
            " {pct:>3}%  {}  {eta}  {peers}",
            human_speed(item.speed_mib)
        ),
        Style::default().fg(theme.colors.muted().to_ratatui()),
    ));
    line
}

/// Renders the queue tab (active downloads + recently downloaded section).
fn draw_active(
    frame: &mut Frame,
    area: Rect,
    state: &DownloadsState,
    display: &DisplayState,
    theme: &Theme,
) {
    let mut lines: Vec<Line> = Vec::new();

    // Queue: queued / downloading / paused / failed / missing first.
    for (pos, item) in state.items.iter().enumerate() {
        if item.status == QueueStatus::Seeding {
            continue;
        }
        let selected = pos == state.selected;
        lines.push(name_line(theme, item, selected));
        match item.status {
            QueueStatus::Downloading => {
                lines.push(active_detail(theme, item, display, area.width as usize));
            }
            QueueStatus::Queued => {
                lines.push(Line::from(Span::styled(
                    format!("  waiting for a slot (HARBOUR_MAX_DOWNLOADS={MAX_DOWNLOADS})"),
                    Style::default().fg(theme.colors.dim().to_ratatui()),
                )));
            }
            QueueStatus::Paused => {
                lines.push(Line::from(Span::styled(
                    "  paused — press p to resume",
                    Style::default().fg(theme.colors.dim().to_ratatui()),
                )));
            }
            QueueStatus::Failed => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        item.error
                            .clone()
                            .unwrap_or_else(|| "engine error".to_string())
                    ),
                    Style::default().fg(theme.colors.error().to_ratatui()),
                )));
            }
            QueueStatus::Missing => {
                lines.push(Line::from(Span::styled(
                    "  files missing — re-verify or re-add",
                    Style::default().fg(theme.colors.error().to_ratatui()),
                )));
            }
            QueueStatus::Seeding => {}
        }
    }

    if state.items.iter().any(|i| i.status != QueueStatus::Seeding) {
        lines.push(Line::default());
    }

    // Recently downloaded section (design.md §2.3).
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", theme.symbols.border_h),
            Style::default().fg(theme.colors.border().to_ratatui()),
        ),
        Span::styled(
            " recently downloaded ",
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ),
        Span::styled(
            theme.symbols.border_h.to_string(),
            Style::default().fg(theme.colors.border().to_ratatui()),
        ),
    ]));
    if state.history.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing finished yet — downloads are simulated until the engine lands",
            Style::default().fg(theme.colors.dim().to_ratatui()),
        )));
    }
    for h in state.history.iter().rev().take(8) {
        lines.push(Line::from(vec![
            Span::styled(
                "  ✓ ",
                Style::default().fg(theme.colors.success().to_ratatui()),
            ),
            Span::styled(
                h.name.clone(),
                Style::default().fg(theme.colors.text().to_ratatui()),
            ),
            Span::styled(
                format!(
                    "  {}  {}",
                    human_size(h.size_bytes),
                    clock_time(h.completed_at_epoch_ms)
                ),
                Style::default().fg(theme.colors.dim().to_ratatui()),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Renders the Seeding tab: upload speed, peers, and the `p` pause action.
fn draw_seeding(frame: &mut Frame, area: Rect, state: &DownloadsState, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    let seeding: Vec<&QueueItem> = state
        .items
        .iter()
        .filter(|i| {
            i.status == QueueStatus::Seeding || (i.status == QueueStatus::Paused && i.finished)
        })
        .collect();
    if seeding.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing seeding yet — finished downloads seed by default",
            Style::default().fg(theme.colors.dim().to_ratatui()),
        )));
    }
    for item in seeding {
        let selected = state.items.iter().position(|i| i.id == item.id) == Some(state.selected);
        lines.push(name_line(theme, item, selected));
        let peers = item
            .peers
            .map(|p| format!("{p}"))
            .unwrap_or_else(|| "—".to_string());
        lines.push(Line::from(Span::styled(
            format!(
                "  ⬆ {}  peers {peers}  uploaded {}   [p] {}",
                human_speed(item.upload_speed_mib),
                human_size(item.uploaded_bytes),
                if item.status == QueueStatus::Paused {
                    "resume"
                } else {
                    "pause"
                }
            ),
            Style::default().fg(theme.colors.muted().to_ratatui()),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// HH:MM wall clock from an epoch-ms timestamp.
fn clock_time(epoch_ms: i64) -> String {
    let secs_of_day = (epoch_ms / 1000) % 86_400;
    format!("{:02}:{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60)
}

/// Paints the downloads screen.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &DownloadsState,
    display: &DisplayState,
    _vars: &FrameVars,
    theme: &Theme,
) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(theme))
        .border_style(Style::default().fg(theme.colors.border().to_ratatui()))
        .title(Span::styled(
            " harbour — downloads ",
            Style::default().fg(theme.colors.accent().to_ratatui()),
        ))
        .style(Style::default().bg(theme.colors.bg().to_ratatui()));
    frame.render_widget(block, area);

    if area.width < 8 || area.height < 5 {
        return;
    }
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    let areas: [Rect; 3] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    let (tabs_area, body_area, status_area) = (areas[0], areas[1], areas[2]);

    // Tabs: Active | Seeding, arrow keys switch (design.md §2.3).
    let mut tabs = Line::default();
    for (i, label) in ["Active", "Seeding"].iter().enumerate() {
        let active = (i == 0 && !state.show_seeding) || (i == 1 && state.show_seeding);
        let style = if active {
            Style::default()
                .fg(theme.colors.accent().to_ratatui())
                .bg(theme.colors.selected_bg().to_ratatui())
        } else {
            Style::default().fg(theme.colors.muted().to_ratatui())
        };
        tabs.push_span(Span::styled(format!(" {label} "), style));
        tabs.push_span(Span::styled(
            " ",
            Style::default().fg(theme.colors.border().to_ratatui()),
        ));
    }
    frame.render_widget(Paragraph::new(tabs), tabs_area);

    if state.show_seeding {
        draw_seeding(frame, body_area, state, theme);
    } else {
        draw_active(frame, body_area, state, display, theme);
    }

    // Status line: live counts + totals (design.md §2.3).
    let active_n = state
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.status,
                QueueStatus::Queued | QueueStatus::Downloading | QueueStatus::Paused
            )
        })
        .count();
    let seeding_n = state
        .items
        .iter()
        .filter(|i| i.status == QueueStatus::Seeding)
        .count();
    let failed_n = state
        .items
        .iter()
        .filter(|i| matches!(i.status, QueueStatus::Failed | QueueStatus::Missing))
        .count();
    let up_total: f64 = state.items.iter().map(|i| i.upload_speed_mib).sum();
    let mut left = format!(" {active_n} active · {seeding_n} seeding");
    if failed_n > 0 {
        left.push_str(&format!(" · {failed_n} failed"));
    }
    let right = format!("⬆ {} total   tab search · ? help", human_speed(up_total));
    super::status::draw(
        frame,
        status_area,
        theme,
        &super::status::StatusLine {
            left,
            right: &right,
            answered: None,
            spinner: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HistoryItem;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn item(id: &str, name: &str, status: QueueStatus, progress: f64) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            name: name.to_string(),
            source: Some("yts".to_string()),
            magnet: format!("magnet:?xt=urn:btih:{id}&dn=x"),
            dir: std::path::PathBuf::from("~/Downloads"),
            status,
            finished: status == QueueStatus::Seeding,
            progress,
            total_bytes: 48_200_000_000,
            downloaded_bytes: (48_200_000_000.0 * progress) as u64,
            speed_mib: 3.1,
            upload_speed_mib: 2.2,
            uploaded_bytes: 1_000_000_000,
            peers: Some(12),
            eta_secs: Some(252),
            error: None,
            added_at_epoch_ms: 1_780_000_000_000,
        }
    }

    fn render(state: &DownloadsState, display: &DisplayState) -> Vec<String> {
        let theme = Theme::titanium();
        let backend = TestBackend::new(90, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let vars = FrameVars::default();
        terminal
            .draw(|f| draw(f, f.area(), state, display, &vars, &theme))
            .unwrap();
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
    fn active_tab_shows_queue_and_recent() {
        let mut state = DownloadsState::default();
        state.items.push(item(
            "a",
            "Elden Ring — Shadow of the Erdtree",
            QueueStatus::Downloading,
            0.62,
        ));
        state.items.push(item(
            "b",
            "Interstellar (2014) 1080p",
            QueueStatus::Queued,
            0.0,
        ));
        state
            .items
            .push(item("c", "Dune: Part Two", QueueStatus::Seeding, 1.0));
        state.history.push(HistoryItem {
            id: "c".to_string(),
            name: "Dune: Part Two".to_string(),
            size_bytes: 65_800_000_000,
            source: Some("x1337-movies".to_string()),
            completed_at_epoch_ms: 1_780_000_000_000,
        });
        let mut display = DisplayState::default();
        display.progress.insert("a".to_string(), 0.62);
        display.progress.insert("b".to_string(), 0.0);

        let lines = render(&state, &display);
        let joined = lines.join("\n");
        assert!(joined.contains("harbour — downloads"), "frame title");
        assert!(joined.contains("Active"), "tab 1");
        assert!(joined.contains("Seeding"), "tab 2");
        assert!(joined.contains("Elden Ring"), "item name");
        assert!(joined.contains("62%"), "percent from eased display");
        assert!(joined.contains("3.1 MB/s"), "speed");
        assert!(joined.contains("04:12"), "ETA");
        assert!(joined.contains("⬆12"), "peers");
        assert!(joined.contains("recently downloaded"), "section header");
        assert!(joined.contains("✓"), "history check");
        assert!(joined.contains("2 active · 1 seeding"), "status counts");
    }

    #[test]
    fn seeding_tab_lists_uploads() {
        let mut state = DownloadsState::default();
        state.show_seeding = true;
        state
            .items
            .push(item("s", "Severance — S02", QueueStatus::Seeding, 1.0));
        let display = DisplayState::default();
        let lines = render(&state, &display);
        let joined = lines.join("\n");
        assert!(joined.contains("Severance"), "seeding item");
        assert!(joined.contains("2.2 MB/s"), "upload speed");
        assert!(joined.contains("[p] pause"), "pause hint");
    }
}
