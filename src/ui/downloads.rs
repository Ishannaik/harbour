//! Downloads view (phase 2): the active queue with eased progress bars, the
//! recently-downloaded section, and the Seeding tab (docs/design.md §2.3).
//!
//! Pure paint: `draw` renders `DownloadsState` + `Theme`; the app loop owns
//! input and the 30fps tick. The engine's raw progress is eased before
//! render (FR-33): `ItemView::progress` reports what the engine has done,
//! and the smoothed *display* value lives here — one [`EasedValue`] per
//! torrent id, advanced by a nominal frame period per draw so the bar eases
//! toward a moved target instead of snapping. A bar whose target has not
//! moved sits at rest and repaints byte-identical frames. All colors come
//! from the theme subset (docs/theming.md), so custom themes work unchanged.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::anim::EasedValue;
use crate::core::types::{CompletedItem, ItemView, QueueStatus};
use crate::theme::{Theme, ThemeColors};
use crate::ui::DownloadsState;

/// Panel title — same framing as the splash and search panels.
const TITLE: &str = " harbour — downloads ";
/// Tabs: active = accent + underline, inactive = dim. Underline row mirrors
/// the label row's widths, hence TAB_GAP is shared by both.
const TAB_DOWNLOADS: &str = "Downloads";
const TAB_SEEDING: &str = "Seeding";
const TAB_GAP: &str = "   ";
/// Cap on recently-downloaded rows; no scrolling in phase 2.
const HISTORY_ROWS: usize = 5;
/// Bottom hint — the actions that matter on this screen.
const HINT: &str =
    "q quit · o open · c clear completed · p pause · r retry · x remove · w watch · s seeding";

/// Smoothing time constant for display progress (spec §3): a 200ms filter
/// eases a moved bar without ever snapping it.
const EASE_TAU: Duration = Duration::from_millis(200);
/// Nominal frame period — the app loop redraws at 30fps, and each draw call
/// advances the eased bars by exactly one frame. A skipped frame (slow
/// render, or the user being on another screen) simply eases a little
/// slower; the display never jumps.
const FRAME_DT: Duration = Duration::from_millis(33);

/// Smoothed display progress per torrent id (FR-33).
///
/// The view is pure — `draw` receives `&DownloadsState` and the app loop owns
/// the tick — so the eased values live here, advanced once per draw call.
/// New ids are seeded at the engine's current value (a restored mid-download
/// renders where it is, not from 0) and converge as the target moves between
/// polls. Ids that leave the queue are pruned in `draw`, so the map cannot
/// grow with the session's churn.
/// `HashMap::new` is not const, so the map lives behind a [`LazyLock`]; the
/// lock is taken once per item per frame and never held across a draw.
static EASED_BARS: LazyLock<Mutex<HashMap<String, EasedValue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The display progress for `item`: the raw engine value passed through the
/// 200ms exponential smoother, advanced by one nominal frame. Renders the
/// raw value if the lock is ever poisoned — a frame must not die over a
/// smoother.
fn eased_progress(item: &ItemView) -> f64 {
    match EASED_BARS.lock() {
        Ok(mut bars) => {
            let bar = bars
                .entry(item.item.id.clone())
                .or_insert_with(|| EasedValue::new(item.progress(), EASE_TAU));
            bar.set_target(item.progress());
            bar.update(FRAME_DT);
            bar.value()
        }
        Err(_) => item.progress(),
    }
}

/// Drops eased state for torrents no longer in the queue.
///
/// Runs once per draw, and only when the map outgrew the item list — the
/// only situation in which an id can have left — so the common case pays
/// nothing and stale ids never accumulate.
fn prune_eased(items: &[ItemView]) {
    if EASED_BARS.lock().map(|b| b.len()).unwrap_or(0) <= items.len() {
        return;
    }
    let live: std::collections::HashSet<&str> = items.iter().map(|v| v.item.id.as_str()).collect();
    if let Ok(mut bars) = EASED_BARS.lock() {
        bars.retain(|id, _| live.contains(id.as_str()));
    }
}

/// Renders the downloads screen: tabs, queue/seeding body, hint line.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &DownloadsState,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    prune_eased(&state.items);
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

    frame.render_widget(
        Paragraph::new(tab_lines(state, theme, chunks[0], mouse_pos)),
        chunks[0],
    );
    if state.show_seeding {
        draw_seeding(frame, chunks[1], state, theme, mouse_pos);
    } else {
        draw_active(frame, chunks[1], state, theme, mouse_pos);
    }
    frame.render_widget(
        Paragraph::new(Line::from(suffix_span(
            HINT.into(),
            colors.muted().to_ratatui(),
        ))),
        chunks[2],
    );
}

/// Tab row plus underline row. The whole underline is one accent span — the
/// spaces in it are invisible, so only the active tab's border glyphs show.
fn tab_lines(
    state: &DownloadsState,
    theme: &Theme,
    area: Rect,
    mouse_pos: Option<(u16, u16)>,
) -> Vec<Line<'static>> {
    let colors = &theme.colors;
    let downloads_active = !state.show_seeding;
    let accent = colors.accent().to_ratatui();
    let dim = colors.dim().to_ratatui();
    let muted = colors.muted().to_ratatui();

    let tab_y_match = mouse_pos.is_some_and(|(_, my)| my == area.y || my == area.y + 1);
    let downloads_hovered = tab_y_match
        && mouse_pos.is_some_and(|(mx, _)| {
            mx >= area.x && mx < area.x + 2 + TAB_DOWNLOADS.chars().count() as u16 + 2
        });
    let seeding_hovered = tab_y_match
        && mouse_pos.is_some_and(|(mx, _)| {
            mx >= area.x + 2 + TAB_DOWNLOADS.chars().count() as u16 + TAB_GAP.chars().count() as u16
        });

    let downloads_fg = if downloads_active {
        accent
    } else if downloads_hovered {
        muted
    } else {
        dim
    };

    let seeding_fg = if !downloads_active {
        accent
    } else if seeding_hovered {
        muted
    } else {
        dim
    };

    let label = vec![
        Span::raw("  "),
        Span::styled(TAB_DOWNLOADS, Style::default().fg(downloads_fg)),
        Span::raw(TAB_GAP),
        Span::styled(TAB_SEEDING, Style::default().fg(seeding_fg)),
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
fn draw_active(
    frame: &mut Frame,
    area: Rect,
    state: &DownloadsState,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    // The queue takes the flexible remainder so the recent header never
    // vanishes; the section is capped at HISTORY_ROWS.
    let recent_h = (1 + state.history.len().min(HISTORY_ROWS)) as u16;
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(recent_h)]).split(area);
    let active: Vec<(usize, &ItemView)> = state
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| !matches!(it.item.status, QueueStatus::Seeding | QueueStatus::Missing))
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
        for (rel_idx, &(i, item)) in active.iter().skip(start).take(vis.max(1)).enumerate() {
            let sel = i == state.selected;
            let y1 = chunks[0].y + (rel_idx * 2) as u16;
            let y2 = y1 + 1;
            let hovered = mouse_pos.is_some_and(|(mx, my)| {
                mx >= chunks[0].x && mx < chunks[0].right() && (my == y1 || my == y2)
            });
            lines.push(name_line(item, width, sel, hovered, theme));
            lines.push(bar_line(item, width, sel, hovered, theme));
        }
        if active.len() > vis {
            let max_scroll = active.len().saturating_sub(vis);
            let mut scrollbar_state = ScrollbarState::new(max_scroll)
                .position(start)
                .viewport_content_length(vis);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("│"))
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .thumb_style(Style::default().fg(theme.colors.accent().to_ratatui()))
                .track_style(Style::default().fg(theme.colors.dim().to_ratatui()))
                .begin_style(Style::default().fg(theme.colors.dim().to_ratatui()))
                .end_style(Style::default().fg(theme.colors.dim().to_ratatui()));
            frame.render_stateful_widget(scrollbar, chunks[0], &mut scrollbar_state);
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
fn draw_seeding(
    frame: &mut Frame,
    area: Rect,
    state: &DownloadsState,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    let seeding: Vec<(usize, &ItemView)> = state
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it.item.status, QueueStatus::Seeding | QueueStatus::Missing))
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
        for (rel_idx, &(i, item)) in seeding.iter().skip(start).take(vis.max(1)).enumerate() {
            let row_y = area.y + rel_idx as u16;
            let hovered =
                mouse_pos.is_some_and(|(mx, my)| mx >= area.x && mx < area.right() && my == row_y);
            lines.push(seed_row(item, i == state.selected, hovered, width, theme));
        }
        if seeding.len() > vis {
            let max_scroll = seeding.len().saturating_sub(vis);
            let mut scrollbar_state = ScrollbarState::new(max_scroll)
                .position(start)
                .viewport_content_length(vis);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("│"))
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .thumb_style(Style::default().fg(theme.colors.accent().to_ratatui()))
                .track_style(Style::default().fg(theme.colors.dim().to_ratatui()))
                .begin_style(Style::default().fg(theme.colors.dim().to_ratatui()))
                .end_style(Style::default().fg(theme.colors.dim().to_ratatui()));
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Window start for a list of `(original_index, item)` pairs so the selected
/// row stays visible; 0 when everything fits.
fn scroll_start(list: &[(usize, &ItemView)], selected: usize, vis: usize) -> usize {
    match list.iter().position(|(i, _)| *i == selected) {
        Some(p) if vis > 0 && p >= vis => p - vis + 1,
        _ => 0,
    }
}

/// Queue item's first row: name, status chip, right-aligned downloaded/total.
fn name_line(
    item: &ItemView,
    width: usize,
    selected: bool,
    hovered: bool,
    theme: &Theme,
) -> Line<'static> {
    let colors = &theme.colors;
    let chip = format!("[{}]", item.item.status.label());
    let size = format!(
        "{} / {}",
        human_bytes(item.downloaded_bytes()),
        human_bytes(item.total_bytes())
    );
    let fixed = 1 + chip.chars().count() + size.chars().count(); // space + chip + size
    let name = truncate(&item.item.name, width.saturating_sub(fixed + 1));
    let pad = width.saturating_sub(name.chars().count() + fixed);
    let spans = vec![
        suffix_span(name, colors.text().to_ratatui()),
        Span::raw(" "),
        Span::styled(chip, status_chip_style(item.item.status, colors)),
        Span::raw(" ".repeat(pad)),
        suffix_span(size, colors.muted().to_ratatui()),
    ];
    row_line(spans, selected, hovered, colors)
}

/// Peer/swarm column on an active row (FR-32).
///
/// Metadata and swarm join are different waits; `connecting…` named neither
/// and collided with source-health copy. Paused/errored still use the em dash
/// so unknown is never `peers 0`.
fn swarm_label(item: &ItemView) -> String {
    if item.item.status == QueueStatus::Downloading {
        let meta_bytes = item.stats.map(|s| s.total_bytes).unwrap_or(0);
        if meta_bytes == 0 {
            return "fetching metadata…".into();
        }
        if item.peers().unwrap_or(0) == 0 {
            return "looking for peers…".into();
        }
    }
    match item.peers() {
        Some(n) => format!("peers {n}"),
        None => "peers —".into(),
    }
}

/// Queue item's second row: the eased progress bar, then right-aligned
/// percent / speed / peers / ETA. `peers`/`eta` are `Option` per contract B1
/// (librqbit cannot report them while paused) — `None` renders '—', never 0.
fn bar_line(
    item: &ItemView,
    width: usize,
    selected: bool,
    hovered: bool,
    theme: &Theme,
) -> Line<'static> {
    let colors = &theme.colors;
    let accent = colors.accent().to_ratatui();
    let muted = colors.muted().to_ratatui();
    // One eased advance per item per frame; the same display value drives
    // both the bar and the percent label so they always agree (FR-33).
    let display = eased_progress(item);
    let pct = (display * 100.0).round() as u32;
    let eta = item
        .eta()
        .map(|d| fmt_eta(d.as_secs()))
        .unwrap_or_else(|| "—".into());
    let swarm = swarm_label(item);
    let suffix = vec![
        suffix_span(format!("{pct:>3}%"), accent),
        suffix_span(format!("  {:.1} MiB/s", item.speed_mib()), accent),
        suffix_span(format!("  {swarm}"), muted),
        suffix_span(format!("  eta {eta}"), muted),
    ];
    let suffix_w = suffix
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>();
    let bar_w = width.saturating_sub(suffix_w + 2);
    let mut spans = bar_spans(display, bar_w, theme);
    if bar_w > 0 {
        spans.push(Span::raw("  "));
    }
    spans.extend(suffix);
    row_line(spans, selected, hovered, colors)
}

/// Progress bar glyphs: `fill` cells, a `half` cell past the fractional
/// midpoint, then `empty` cells. `progress` is the eased display value
/// (FR-33), so this is a pure render of the smoothed bar.
fn bar_spans(progress: f64, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let colors = &theme.colors;
    let accent = colors.accent().to_ratatui();
    let dim = colors.dim().to_ratatui();
    let scaled = progress * width as f64;
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
        out.push(suffix_span(
            theme.symbols.progress_half.as_ref().into(),
            accent,
        ));
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
fn seed_row(
    item: &ItemView,
    selected: bool,
    hovered: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let colors = &theme.colors;
    let chip = format!("[{}]", item.item.status.label());
    let uploaded = human_bytes(item.stats.map_or(0, |s| s.uploaded_bytes));
    let peers = item
        .peers()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".into());
    let suffix = vec![
        suffix_span(
            format!("  {:.1} MiB/s", item.upload_speed_mib()),
            colors.accent().to_ratatui(),
        ),
        suffix_span(format!("  up {uploaded}"), colors.muted().to_ratatui()),
        suffix_span(format!("  peers {peers}"), colors.muted().to_ratatui()),
        Span::styled(
            format!("  {chip}"),
            status_chip_style(item.item.status, colors),
        ),
    ];
    let suffix_w = suffix
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>();
    let name = truncate(&item.item.name, width.saturating_sub(suffix_w + 1));
    let pad = width.saturating_sub(name.chars().count() + suffix_w);
    let mut spans = vec![
        suffix_span(name, colors.text().to_ratatui()),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(suffix);
    row_line(spans, selected, hovered, colors)
}

/// One recently-downloaded row: name, right-aligned size + success chip.
fn history_line(h: &CompletedItem, width: usize, theme: &Theme) -> Line<'static> {
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
fn row_line(
    spans: Vec<Span<'static>>,
    selected: bool,
    hovered: bool,
    colors: &ThemeColors,
) -> Line<'static> {
    let base = if selected || hovered {
        Style::default().bg(colors.selected_bg().to_ratatui())
    } else {
        Style::default()
    };
    Line::from(spans).style(base)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{EngineStats, QueueItem};

    /// A downloading item with a controllable raw progress. Ids must be
    /// unique per test: `EASED_BARS` is a process-wide map, and a shared id
    /// would let one test's eased value leak into another's.
    fn item_at(id: &str, progress: f64) -> ItemView {
        let mut item = QueueItem::new(
            id.to_string(),
            "eased test item".to_string(),
            None,
            None,
            std::path::PathBuf::from("~/harbour/downloads"),
            0,
        );
        item.status = QueueStatus::Downloading;
        item.total_bytes = 1000;
        ItemView::new(
            item,
            Some(EngineStats {
                progress,
                downloaded_bytes: (1000.0 * progress) as u64,
                total_bytes: 1000,
                speed_mib: 0.0,
                upload_speed_mib: 0.0,
                uploaded_bytes: 0,
                peers: Some(12),
                eta: Some(Duration::from_secs(1800)),
            }),
        )
    }

    #[test]
    fn eased_progress_seeds_at_the_engine_value() {
        // A fresh id renders exactly the engine's value — a restored
        // mid-download must not animate from 0.
        let item = item_at("eased-seed", 0.5);
        assert!(
            (eased_progress(&item) - 0.5).abs() < 1e-9,
            "first render must match the raw value"
        );
        // An unchanged target stays put on later frames (byte-identical
        // repaints once the bar is at rest).
        assert!((eased_progress(&item) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn eased_progress_eases_toward_a_moved_target() {
        let mut item = item_at("eased-move", 0.0);
        assert_eq!(eased_progress(&item), 0.0, "seeds at raw 0");

        // The engine jumps to 50%: the display eases by exactly one filter
        // step, never snaps, and never overshoots.
        item.stats = Some(item_at("eased-move", 0.5).stats.expect("item has stats"));
        let first = eased_progress(&item);
        let expected = crate::anim::eased(0.0, 0.5, FRAME_DT, EASE_TAU);
        assert!(
            (first - expected).abs() < 1e-9,
            "first step must be one eased frame: {first} vs {expected}"
        );
        assert!(first > 0.0 && first < 0.5, "partway, not snapped: {first}");

        let mut prev = first;
        for _ in 0..300 {
            let v = eased_progress(&item);
            assert!(v >= prev && v <= 0.5, "overshoot: {prev} -> {v}");
            prev = v;
        }
        assert!((prev - 0.5).abs() < 1e-6, "converges to target: {prev}");
    }

    #[test]
    fn bar_spans_draws_fill_half_and_empty_cells() {
        let theme = Theme::titanium();
        let fill = theme.symbols.progress_fill.as_ref();
        let half = theme.symbols.progress_half.as_ref();
        let empty = theme.symbols.progress_empty.as_ref();

        // 50% of a 20-cell bar: 10 fill, frac == 0 so no half, 10 empty.
        let spans = bar_spans(0.5, 20, &theme);
        assert_eq!(spans[0].content, fill.repeat(10));
        assert_eq!(spans[1].content, empty.repeat(10));

        // 2.5% of 20 cells: scaled = 0.5 → a half cell past the midpoint,
        // no full cell.
        let spans = bar_spans(0.025, 20, &theme);
        assert_eq!(spans[0].content, half.to_string());
        assert_eq!(spans[1].content, empty.repeat(19));

        // Complete: all fill, no empty run.
        let spans = bar_spans(1.0, 20, &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, fill.repeat(20));
    }

    #[test]
    fn bar_line_percent_matches_the_eased_bar() {
        // The engine reports 50% but the smoother sits at 0 — exactly the
        // frame after a poll jumps the target. The percent label and the bar
        // must both render the eased value, or the two would disagree.
        let item = item_at("eased-line", 0.5);
        EASED_BARS
            .lock()
            .expect("eased bars lock")
            .insert(item.item.id.clone(), EasedValue::new(0.0, EASE_TAU));
        let expected = crate::anim::eased(0.0, 0.5, FRAME_DT, EASE_TAU);

        let line = bar_line(&item, 60, false, false, &Theme::titanium());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let pct = format!("{:>3}%", (expected * 100.0).round() as u32);
        assert!(
            text.contains(&pct),
            "label must show the eased percent {pct}, got: {text}"
        );

        // The bar's fill run must match the same eased value cell-for-cell.
        let suffix = format!("{pct}  0.0 MiB/s  peers 12  eta 30:00");
        let bar_w = 60usize.saturating_sub(suffix.chars().count() + 2);
        let fill_cells = (expected * bar_w as f64).floor() as usize;
        let theme = Theme::titanium();
        assert_eq!(
            line.spans[0].content,
            theme.symbols.progress_fill.as_ref().repeat(fill_cells)
        );
    }

    fn downloading(id: &str, total_bytes: u64, peers: Option<u32>) -> ItemView {
        let mut item = QueueItem::new(
            id.to_string(),
            id.to_string(),
            None,
            None,
            std::path::PathBuf::from("~/harbour/downloads"),
            0,
        );
        item.status = QueueStatus::Downloading;
        item.total_bytes = total_bytes;
        ItemView::new(
            item,
            Some(EngineStats {
                progress: 0.0,
                downloaded_bytes: 0,
                total_bytes,
                speed_mib: 0.0,
                upload_speed_mib: 0.0,
                uploaded_bytes: 0,
                peers,
                eta: None,
            }),
        )
    }

    fn bar_text(item: &ItemView) -> String {
        bar_line(item, 80, false, false, &Theme::titanium())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn bar_line_fetching_metadata_while_size_is_zero() {
        let text = bar_text(&downloading("meta-zero", 0, None));
        assert!(
            text.contains("fetching metadata…"),
            "size 0 is metadata fetch, got: {text}"
        );
        assert!(
            !text.contains("connecting"),
            "never connecting, got: {text}"
        );
        assert!(
            !text.contains("looking for peers"),
            "metadata precedes swarm, got: {text}"
        );
    }

    #[test]
    fn bar_line_looking_for_peers_after_metadata() {
        let none = bar_text(&downloading("peers-none", 1000, None));
        assert!(
            none.contains("looking for peers…"),
            "unknown peers after metadata, got: {none}"
        );
        let zero = bar_text(&downloading("peers-zero", 1000, Some(0)));
        assert!(
            zero.contains("looking for peers…"),
            "zero live peers, got: {zero}"
        );
        assert!(!none.contains("connecting") && !zero.contains("connecting"));
    }

    #[test]
    fn bar_line_paused_unknown_peers_is_em_dash() {
        let mut item = downloading("paused-dash", 1000, None);
        item.item.status = QueueStatus::Paused;
        let text = bar_text(&item);
        assert!(
            text.contains("peers —"),
            "paused unknown is an em dash, got: {text}"
        );
        assert!(
            !text.contains("looking for peers"),
            "paused is not swarm-join, got: {text}"
        );
    }
}
