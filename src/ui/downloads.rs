//! Downloads view (phase 2): the active queue with eased progress bars, the
//! recently-downloaded section, and the Seeding tab (docs/design.md §2.3).
//!
//! Pure paint: `draw` renders `DownloadsState` + `Theme`; the app loop owns
//! input and the 30fps tick. `QueueItem::progress` is already the eased
//! *display* value (queue layer, tau = 200ms), so the view never animates —
//! an unchanged queue repaints byte-identical frames. All colors come from
//! the theme subset (docs/theming.md), so custom themes work unchanged.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme::{Theme, ThemeColors};
use crate::types::{DownloadsState, HistoryItem, QueueItem, QueueStatus};

/// Panel title — same framing as the splash and search panels.
const TITLE: &str = " harbour — downloads ";
/// Tabs: active = accent + underline, inactive = dim. Underline row mirrors
/// the label row's widths, hence TAB_GAP is shared by both.
const TAB_DOWNLOADS: &str = "Downloads";
const TAB_SEEDING: &str = "Seeding";
const TAB_GAP: &str = "   ";
/// Cap on recently-downloaded rows; no scrolling in phase 2.
const HISTORY_ROWS: usize = 5;
/// Bottom hint — the three actions that matter on this screen.
const HINT: &str = "tab switch · p pause · q quit";

/// Renders the downloads screen: tabs, queue/seeding body, hint line.
pub fn draw(frame: &mut Frame, area: Rect, state: &DownloadsState, theme: &Theme) {
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();

    // Rounded panel around the whole screen, like the splash (app.rs).
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

    let chunks = Layout::vertical([
        Constraint::Length(2), // tabs: label row + underline row
        Constraint::Min(0),    // body
        Constraint::Length(1), // hint line
    ])
    .split(inner);

    frame.render_widget(Paragraph::new(tab_lines(state, theme)), chunks[0]);
    if state.show_seeding {
        draw_seeding(frame, chunks[1], state, theme);
    } else {
        draw_active(frame, chunks[1], state, theme);
    }
    frame.render_widget(
        Paragraph::new(Line::from(suffix_span(HINT.into(), colors.muted().to_ratatui()))),
        chunks[2],
    );
}

/// Tab row plus underline row. The whole underline is one accent span — the
/// spaces in it are invisible, so only the active tab's border glyphs show.
fn tab_lines(state: &DownloadsState, theme: &Theme) -> Vec<Line<'static>> {
    let colors = &theme.colors;
    let downloads_active = !state.show_seeding;
    let accent = colors.accent().to_ratatui();
    let dim = colors.dim().to_ratatui();
    let label = vec![
        Span::raw("  "),
        Span::styled(
            TAB_DOWNLOADS,
            Style::default().fg(if downloads_active { accent } else { dim }),
        ),
        Span::raw(TAB_GAP),
        Span::styled(
            TAB_SEEDING,
            Style::default().fg(if downloads_active { dim } else { accent }),
        ),
        Span::raw("  "),
    ];
    let underline = format!(
        "  {}{TAB_GAP}{}",
        tab_underline(downloads_active, TAB_DOWNLOADS, theme),
        tab_underline(!downloads_active, TAB_SEEDING, theme),
    );
    vec![
        Line::from(label),
        Line::from(vec![Span::styled(underline, Style::default().fg(accent))]),
    ]
}

/// One tab's underline: `n` border glyphs when active, `n` spaces when not.
fn tab_underline(active: bool, tab: &str, theme: &Theme) -> String {
    let n = tab.chars().count();
    if active {
        theme.symbols.border_h.as_ref().repeat(n)
    } else {
        " ".repeat(n)
    }
}

/// Active tab body: queue rows (name + bar lines) above recently downloaded.
fn draw_active(frame: &mut Frame, area: Rect, state: &DownloadsState, theme: &Theme) {
    // The queue takes the flexible remainder so the recent header never
    // vanishes; the section is capped at HISTORY_ROWS.
    let recent_h = (1 + state.history.len().min(HISTORY_ROWS)) as u16;
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(recent_h)]).split(area);
    let active: Vec<(usize, &QueueItem)> = state
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| !matches!(it.status, QueueStatus::Seeding | QueueStatus::Missing))
        .collect();

    let mut lines: Vec<Line> = Vec::new();
    if active.is_empty() {
        lines.push(Line::from(suffix_span(
            "nothing here yet — search and press d".into(),
            theme.colors.muted().to_ratatui(),
        )));
    } else {
        let width = chunks[0].width as usize;
        let vis = chunks[0].height as usize / 2; // one item spans two rows
        // Keep the selection on screen without a scrollbar: a selection past
        // the bottom shifts the window so it sits on the last visible row.
        let start = scroll_start(&active, state.selected, vis);
        for &(i, item) in active.iter().skip(start).take(vis.max(1)) {
            let sel = i == state.selected;
            lines.push(name_line(item, width, sel, theme));
            lines.push(bar_line(item, width, sel, theme));
        }
    }
    frame.render_widget(Paragraph::new(lines), chunks[0]);

    // The section header doubles as a divider: title, then border glyphs
    // filling the rest of the row (mirrors the wireframe's underline).
    let title = " recently downloaded ";
    let rest = chunks[1].width.saturating_sub(title.chars().count() as u16) as usize;
    let divider: String = theme.symbols.border_h.as_ref().repeat(rest);
    let mut recent = vec![Line::from(suffix_span(
        format!("{title}{divider}"),
        theme.colors.muted().to_ratatui(),
    ))];
    for h in state.history.iter().take(HISTORY_ROWS) {
        recent.push(history_line(h, chunks[1].width as usize, theme));
    }
    frame.render_widget(Paragraph::new(recent), chunks[1]);
}

/// Seeding tab body: one row per seeding/missing item.
fn draw_seeding(frame: &mut Frame, area: Rect, state: &DownloadsState, theme: &Theme) {
    let seeding: Vec<(usize, &QueueItem)> = state
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it.status, QueueStatus::Seeding | QueueStatus::Missing))
        .collect();
    let mut lines: Vec<Line> = Vec::new();
    if seeding.is_empty() {
        lines.push(Line::from(suffix_span(
            "nothing seeding yet".into(),
            theme.colors.muted().to_ratatui(),
        )));
    } else {
        let width = area.width as usize;
        let vis = area.height as usize;
        let start = scroll_start(&seeding, state.selected, vis);
        for &(i, item) in seeding.iter().skip(start).take(vis.max(1)) {
            lines.push(seed_row(item, i == state.selected, width, theme));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Window start for a list of `(original_index, item)` pairs so the selected
/// row stays visible; 0 when everything fits.
fn scroll_start(list: &[(usize, &QueueItem)], selected: usize, vis: usize) -> usize {
    match list.iter().position(|(i, _)| *i == selected) {
        Some(p) if vis > 0 && p >= vis => p - vis + 1,
        _ => 0,
    }
}

/// Queue item's first row: name, status chip, right-aligned downloaded/total.
fn name_line(item: &QueueItem, width: usize, selected: bool, theme: &Theme) -> Line<'static> {
    let colors = &theme.colors;
    let chip = format!("[{}]", item.status.label());
    let size = format!(
        "{} / {}",
        human_bytes(item.downloaded_bytes),
        human_bytes(item.total_bytes)
    );
    let fixed = 1 + chip.chars().count() + size.chars().count(); // space + chip + size
    let name = truncate(&item.name, width.saturating_sub(fixed + 1));
    let pad = width.saturating_sub(name.chars().count() + fixed);
    let spans = vec![
        suffix_span(name, colors.text().to_ratatui()),
        Span::raw(" "),
        Span::styled(chip, status_chip_style(item.status, colors)),
        Span::raw(" ".repeat(pad)),
        suffix_span(size, colors.muted().to_ratatui()),
    ];
    row_line(spans, selected, colors)
}

/// Queue item's second row: the eased progress bar, then right-aligned
/// percent / speed / peers / ETA. `peers`/`eta` are `Option` per contract B1
/// (librqbit cannot report them while paused) — `None` renders '—', never 0.
fn bar_line(item: &QueueItem, width: usize, selected: bool, theme: &Theme) -> Line<'static> {
    let colors = &theme.colors;
    let accent = colors.accent().to_ratatui();
    let muted = colors.muted().to_ratatui();
    let pct = (item.progress.clamp(0.0, 1.0) * 100.0).round() as u32;
    let peers = item
        .peers
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".into());
    let eta = item
        .eta_secs
        .map(fmt_eta)
        .unwrap_or_else(|| "—".into());
    let suffix = vec![
        suffix_span(format!("{pct:>3}%"), accent),
        suffix_span(format!("  {:.1} MiB/s", item.speed_mib), accent),
        suffix_span(format!("  peers {peers}"), muted),
        suffix_span(format!("  eta {eta}"), muted),
    ];
    let suffix_w = suffix.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let bar_w = width.saturating_sub(suffix_w + 2);
    let mut spans = bar_spans(item, bar_w, theme);
    if bar_w > 0 {
        spans.push(Span::raw("  "));
    }
    spans.extend(suffix);
    row_line(spans, selected, colors)
}

/// Progress bar glyphs: `fill` cells, a `half` cell past the fractional
/// midpoint, then `empty` cells. `progress` is already eased (module docs),
/// so this is a pure render of the smoothed display value.
fn bar_spans(item: &QueueItem, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let colors = &theme.colors;
    let accent = colors.accent().to_ratatui();
    let dim = colors.dim().to_ratatui();
    let scaled = item.progress.clamp(0.0, 1.0) * width as f64;
    let mut full = scaled.floor() as usize;
    let frac = scaled - full as f64;
    let mut out = Vec::new();
    if full > 0 {
        out.push(suffix_span(
            theme.symbols.progress_fill.as_ref().repeat(full),
            accent,
        ));
    }
    if frac >= 0.5 && full < width {
        // The half cell reads as filled at the eased value; bump `full` so
        // the empty run starts one cell later.
        out.push(suffix_span(theme.symbols.progress_half.as_ref().into(), accent));
        full += 1;
    }
    if full < width {
        out.push(suffix_span(
            theme.symbols.progress_empty.as_ref().repeat(width - full),
            dim,
        ));
    }
    out
}

/// One Seeding-tab row: name, upload speed (accent — the one live number),
/// uploaded total, peers, status chip.
fn seed_row(item: &QueueItem, selected: bool, width: usize, theme: &Theme) -> Line<'static> {
    let colors = &theme.colors;
    let chip = format!("[{}]", item.status.label());
    let uploaded = human_bytes(item.uploaded_bytes);
    let peers = item
        .peers
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".into());
    let suffix = vec![
        suffix_span(format!("  {:.1} MiB/s", item.upload_speed_mib), colors.accent().to_ratatui()),
        suffix_span(format!("  up {uploaded}"), colors.muted().to_ratatui()),
        suffix_span(format!("  peers {peers}"), colors.muted().to_ratatui()),
        Span::styled(format!("  {chip}"), status_chip_style(item.status, colors)),
    ];
    let suffix_w = suffix.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let name = truncate(&item.name, width.saturating_sub(suffix_w + 1));
    let pad = width.saturating_sub(name.chars().count() + suffix_w);
    let mut spans = vec![
        suffix_span(name, colors.text().to_ratatui()),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(suffix);
    row_line(spans, selected, colors)
}

/// One recently-downloaded row: name, right-aligned size + success chip.
fn history_line(h: &HistoryItem, width: usize, theme: &Theme) -> Line<'static> {
    let colors = &theme.colors;
    let size = human_bytes(h.size_bytes);
    let chip = "  [completed]";
    let suffix_w = size.chars().count() + chip.chars().count();
    let name = truncate(&h.name, width.saturating_sub(suffix_w + 1));
    let pad = width.saturating_sub(name.chars().count() + suffix_w);
    let spans = vec![
        suffix_span(name, colors.text().to_ratatui()),
        Span::raw(" ".repeat(pad)),
        suffix_span(size, colors.muted().to_ratatui()),
        Span::styled(chip, Style::default().fg(colors.success().to_ratatui())),
    ];
    Line::from(spans)
}

/// Selected-row background as the line's base style; spans set only fg, so
/// the highlight shows through every cell.
fn row_line(spans: Vec<Span<'static>>, selected: bool, colors: &ThemeColors) -> Line<'static> {
    let base = if selected {
        Style::default().bg(colors.selected_bg().to_ratatui())
    } else {
        Style::default()
    };
    Line::styled(spans, base)
}

/// One styled span — collapses the repetitive `Span::styled(.., fg(..))`.
fn suffix_span(text: String, fg: ratatui::style::Color) -> Span<'static> {
    Span::styled(text, Style::default().fg(fg))
}

/// Chip color per status: queued waits (dim), downloading is the live accent,
/// paused/missing are degraded (warning), failed is error, seeding is success.
fn status_chip_style(status: QueueStatus, colors: &ThemeColors) -> Style {
    let fg = match status {
        QueueStatus::Queued => colors.dim(),
        QueueStatus::Downloading => colors.accent(),
        QueueStatus::Paused => colors.warning(),
        QueueStatus::Failed => colors.error(),
        QueueStatus::Seeding => colors.success(),
        QueueStatus::Missing => colors.warning(),
    };
    Style::default().fg(fg.to_ratatui())
}

/// Byte size in binary units (KiB/MiB/GiB…), matching the engine's MiB/s
/// speed convention — one unit system everywhere.
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// ETA as `HH:MM:SS` over an hour, `MM:SS` under — the wireframe's `04:12`.
fn fmt_eta(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Truncate to `max` cells, replacing the last with '…', so a long name
/// never bleeds into the right-aligned column.
fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
