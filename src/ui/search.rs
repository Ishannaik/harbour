//! Search view (design.md §2.2): sidebar with the four groups and their
//! source-health dots, gradient query bar with shimmer while results stream,
//! and the deduped results list with colored size/seeders and staggered
//! source tags.
//!
//! Pure paint: takes `&SearchState` + eased display values, returns nothing,
//! reads no clock. Keybind dispatch and the fake-engine streaming live in
//! app.rs; this module only decides what a given state looks like.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{DisplayState, FrameVars, border_set, human_size};
use crate::fake::{GROUP_ORDER, sources_in_group};
use crate::theme::{Color, Theme, lerp_color};
use crate::types::{Focus, SearchState, SourceStatus, TorrentResult};

/// Sidebar width in cells; keeps the health dots + counts readable.
const SIDEBAR_W: u16 = 22;

/// A selectable sidebar row. Groups and sources are rows; "all" clears the
/// filter (design.md §2.2: group filters the list, source filters tighter).
enum SidebarEntry<'a> {
    All,
    Group(crate::types::SourceGroup),
    Source(&'a crate::types::SourceDef),
}

/// The flat sidebar row list, in display order.
fn sidebar_entries<'a>() -> Vec<SidebarEntry<'a>> {
    let mut out = vec![SidebarEntry::All];
    for group in GROUP_ORDER {
        out.push(SidebarEntry::Group(*group));
        for def in sources_in_group(*group) {
            out.push(SidebarEntry::Source(def));
        }
    }
    out
}

/// Indexes into `state.results` of the rows that survive the sidebar filter.
/// The filter is cumulative (design.md open questions): a group keeps rows
/// with any tagged source in it, a source keeps rows it reported.
fn visible_indices(state: &SearchState) -> Vec<usize> {
    state
        .results
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            let tags = state
                .tags
                .get(&r.info_hash)
                .map(|t| t.as_slice())
                .unwrap_or(&[]);
            match &state.filter {
                crate::types::SidebarFilter::All => true,
                crate::types::SidebarFilter::Group(g) => tags
                    .iter()
                    .any(|s| crate::fake::source_by_id(s).is_some_and(|d| d.groups.contains(g))),
                crate::types::SidebarFilter::Source(s) => tags.contains(s),
            }
        })
        .map(|(i, _)| i)
        .collect()
}

/// The result the selection currently points at (respecting the filter).
pub(crate) fn selected_result(state: &SearchState) -> Option<&TorrentResult> {
    let visible = visible_indices(state);
    visible.get(state.selected).map(|&i| &state.results[i])
}

/// Number of rows the filter leaves visible — what navigation clamps against.
pub(crate) fn visible_count(state: &SearchState) -> usize {
    visible_indices(state).len()
}

/// How many selectable sidebar rows exist (navigation bound).
pub(crate) fn sidebar_count() -> usize {
    sidebar_entries().len()
}

/// The sidebar entry kind at a row index, for filter application (app.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarEntryKind {
    All,
    Group(crate::types::SourceGroup),
    Source(&'static str),
}

/// Maps a sidebar row index to its kind — the app applies this as the filter.
pub(crate) fn sidebar_entry_at(index: usize) -> SidebarEntryKind {
    match sidebar_entries().get(index) {
        Some(SidebarEntry::All) => SidebarEntryKind::All,
        Some(SidebarEntry::Group(g)) => SidebarEntryKind::Group(*g),
        Some(SidebarEntry::Source(def)) => SidebarEntryKind::Source(def.id),
        None => SidebarEntryKind::All,
    }
}

/// Seeders color rule (design.md §2.2): green above a threshold, red near
/// zero, warning in between.
fn seeder_color(theme: &Theme, seeders: u32) -> Color {
    if seeders > 100 {
        theme.colors.success()
    } else if seeders >= 10 {
        theme.colors.warning()
    } else {
        theme.colors.error()
    }
}

/// One result row's second line: size, seeders, leechers, staggered tags.
fn detail_line<'a>(theme: &Theme, r: &TorrentResult, tags: &[&'static str]) -> Line<'a> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            human_size(r.size_bytes),
            Style::default().fg(theme.colors.syntax_number().to_ratatui()),
        ),
        Span::styled(
            format!("   ⬆ {}", r.seeders),
            Style::default().fg(seeder_color(theme, r.seeders).to_ratatui()),
        ),
        Span::styled(
            format!("  ⬇ {}", r.leechers),
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ),
    ];
    for tag in tags {
        let label = crate::fake::source_by_id(tag)
            .map(|d| d.label)
            .unwrap_or(tag);
        spans.push(Span::styled(
            format!("  [{label}]"),
            Style::default().fg(theme.colors.syntax_string().to_ratatui()),
        ));
    }
    Line::from(spans)
}

/// The shimmer band under the query bar: a moving white-hot highlight over
/// the accent→text gradient, only while results stream (design.md §2.2).
fn shimmer_line(theme: &Theme, width: usize, elapsed: std::time::Duration) -> Line<'static> {
    let colors = &theme.colors;
    let hot = Color::Rgb(255, 255, 255);
    let period = 1500.0; // ms per sweep
    let center = (elapsed.as_secs_f64() * 1000.0 / period).fract() * width as f64;
    let mut spans = Vec::new();
    for x in 0..width {
        let t = if width <= 1 {
            0.0
        } else {
            x as f64 / (width - 1) as f64
        };
        let base = lerp_color(colors.accent(), colors.text(), t);
        let d = x as f64 - center;
        let band = (-(d * d) / (2.0 * 2.2 * 2.2)).exp();
        let color = if band > 0.05 {
            lerp_color(base, hot, 0.7 * band)
        } else {
            base
        };
        spans.push(Span::styled(
            theme.symbols.border_h.to_string(),
            Style::default().fg(color.to_ratatui()),
        ));
    }
    Line::from(spans)
}

/// Paints the search screen: outer rounded frame, query bar + shimmer,
/// sidebar and results, then the status line inside the frame.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    display: &DisplayState,
    vars: &FrameVars,
    theme: &Theme,
) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(theme))
        .border_style(Style::default().fg(theme.colors.border().to_ratatui()))
        .title(Span::styled(
            " harbour — search ",
            Style::default().fg(theme.colors.accent().to_ratatui()),
        ))
        .style(Style::default().bg(theme.colors.bg().to_ratatui()));
    frame.render_widget(block, area);

    if area.width < 8 || area.height < 5 {
        return;
    }
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    let areas: [Rect; 3] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    let (query_area, main_area, status_area) = (areas[0], areas[1], areas[2]);

    // --- query bar -------------------------------------------------------
    let mut query = Line::default();
    query.push_span(Span::styled(
        "▸ ",
        Style::default().fg(theme.colors.accent().to_ratatui()),
    ));
    query.push_span(Span::styled(
        if state.draft.is_empty() {
            "search 10 sources…".to_string()
        } else {
            state.draft.clone()
        },
        Style::default().fg(if state.draft.is_empty() {
            theme.colors.dim()
        } else {
            theme.colors.text()
        }
        .to_ratatui()),
    ));
    if !state.searching {
        query.push_span(Span::styled(
            "█",
            Style::default().fg(theme.colors.accent().to_ratatui()),
        ));
    }
    let right = "  [⏎ enter]  ";
    let qw = query_area
        .width
        .saturating_sub(right.chars().count() as u16 + 4);
    let mut padded = query.patch_style(Style::default().bg(theme.colors.bg().to_ratatui()));
    padded.push_span(Span::raw(
        " ".repeat(qw.saturating_sub(padded.width() as u16) as usize),
    ));
    padded.push_span(Span::styled(
        right,
        Style::default().fg(theme.colors.muted().to_ratatui()),
    ));
    frame.render_widget(Paragraph::new(padded), query_area);

    // Shimmer under the bar while results stream.
    let shimmer_area = Rect::new(query_area.x, query_area.y + 1, query_area.width, 1);
    if state.searching {
        frame.render_widget(
            shimmer_line(theme, shimmer_area.width as usize, vars.elapsed),
            shimmer_area,
        );
    }

    // --- sidebar | results ------------------------------------------------
    let columns: [Rect; 2] =
        Layout::horizontal([Constraint::Length(SIDEBAR_W), Constraint::Min(0)]).areas(main_area);
    let (sidebar_area, results_area) = (columns[0], columns[1]);
    draw_sidebar(frame, sidebar_area, state, theme);
    draw_results(frame, results_area, state, theme);

    // --- status line -----------------------------------------------------
    let answered_n = state.source_health.len();
    let left = if state.searching {
        format!(
            " {} searching · {}",
            vars.spinner.unwrap_or(""),
            state.query
        )
    } else if state.query.is_empty() && state.draft.is_empty() {
        " curated top lists".to_string()
    } else {
        format!(" search · {}", state.query)
    };
    let answered = if answered_n == 0 {
        None
    } else {
        Some((
            display.answered,
            format!(" {answered_n}/{}", crate::fake::SOURCES.len()),
        ))
    };
    super::status::draw(
        frame,
        status_area,
        theme,
        &super::status::StatusLine {
            left,
            right: "tab downloads · ? help",
            answered,
            spinner: state.searching.then_some(vars.spinner.unwrap_or("")),
        },
    );
}

fn draw_sidebar(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(theme))
        .border_style(Style::default().fg(theme.colors.border().to_ratatui()))
        .title(Span::styled(
            " sources ",
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ))
        .style(Style::default().bg(theme.colors.bg().to_ratatui()));
    frame.render_widget(block, area);
    if area.width < 4 || area.height < 3 {
        return;
    }
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);

    let entries = sidebar_entries();
    let selected = state.sidebar_selected.min(entries.len().saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let is_focus = state.focus == Focus::Sidebar && i == selected;
        let matches_filter = match (&state.filter, entry) {
            (crate::types::SidebarFilter::All, SidebarEntry::All) => true,
            (crate::types::SidebarFilter::Group(g), SidebarEntry::Group(eg)) => g == eg,
            (crate::types::SidebarFilter::Source(s), SidebarEntry::Source(es)) => *s == es.id,
            _ => false,
        };
        let fg = if is_focus || matches_filter {
            theme.colors.accent()
        } else {
            theme.colors.muted()
        };
        let mut style = Style::default().fg(fg.to_ratatui());
        if is_focus {
            style = style.bg(theme.colors.selected_bg().to_ratatui());
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        match entry {
            SidebarEntry::All => {
                spans.push(Span::styled(
                    if is_focus { "▸ " } else { "  " }.to_string(),
                    Style::default().fg(theme.colors.accent().to_ratatui()),
                ));
                spans.push(Span::styled("all sources", style));
            }
            SidebarEntry::Group(g) => {
                spans.push(Span::styled("   ", Style::default()));
                spans.push(Span::styled(
                    g.label().to_ascii_uppercase(),
                    style.fg(theme.colors.syntax_type().to_ratatui()),
                ));
            }
            SidebarEntry::Source(def) => {
                spans.push(Span::styled(
                    if is_focus { "▸ " } else { "    " }.to_string(),
                    Style::default().fg(theme.colors.accent().to_ratatui()),
                ));
                let health = state
                    .source_health
                    .get(def.id)
                    .copied()
                    .unwrap_or(SourceStatus::Empty);
                let (dot, dot_color) = match health {
                    SourceStatus::Online => (&theme.symbols.dot_online, theme.colors.success()),
                    SourceStatus::Empty => (&theme.symbols.dot_online, theme.colors.dim()),
                    SourceStatus::Offline => (&theme.symbols.dot_offline, theme.colors.error()),
                };
                spans.push(Span::styled(
                    format!("{} ", dot),
                    Style::default().fg(dot_color.to_ratatui()),
                ));
                spans.push(Span::styled(def.label.to_string(), style));
                if let Some(n) = state.source_counts.get(def.id).filter(|n| **n > 0) {
                    spans.push(Span::styled(
                        format!(" {n}"),
                        Style::default().fg(theme.colors.dim().to_ratatui()),
                    ));
                }
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_results(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let visible = visible_indices(state);
    let title = format!(
        " results · {} ",
        if visible.is_empty() { 0 } else { visible.len() }
    );
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(theme))
        .border_style(Style::default().fg(theme.colors.border().to_ratatui()))
        .title(Span::styled(
            title,
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ))
        .style(Style::default().bg(theme.colors.bg().to_ratatui()));
    frame.render_widget(block, area);
    if area.width < 8 || area.height < 3 {
        return;
    }
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);

    let mut lines: Vec<Line> = Vec::new();
    if visible.is_empty() {
        let msg = if state.searching {
            " streaming results…"
        } else if state.draft.is_empty() && state.query.is_empty() {
            " type a query and press Enter — or Enter empty for curated lists"
        } else {
            " no results — try another query"
        };
        lines.push(Line::from(Span::styled(
            msg,
            Style::default().fg(theme.colors.dim().to_ratatui()),
        )));
    } else {
        for (pos, &ri) in visible.iter().enumerate() {
            let r = &state.results[ri];
            let tags = state.tags.get(&r.info_hash).cloned().unwrap_or_default();
            let is_selected = pos == state.selected;
            let name_style = if is_selected {
                Style::default()
                    .fg(theme.colors.accent().to_ratatui())
                    .bg(theme.colors.selected_bg().to_ratatui())
            } else {
                Style::default().fg(theme.colors.text().to_ratatui())
            };
            let mut name = Line::default();
            name.push_span(Span::styled(
                format!("{:>2} ", pos + 1),
                Style::default().fg(theme.colors.dim().to_ratatui()),
            ));
            name.push_span(Span::styled(
                r.name.clone(),
                if is_selected {
                    name_style
                } else {
                    Style::default().fg(theme.colors.text().to_ratatui())
                },
            ));
            lines.push(name);
            lines.push(detail_line(theme, r, &tags));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeEngine;
    use crate::types::{SearchState, SidebarFilter};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(state: &SearchState, display: &DisplayState) -> Vec<String> {
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 24);
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

    fn fixture_state() -> (SearchState, DisplayState) {
        let engine = FakeEngine::new();
        let mut state = SearchState {
            query: "dune".to_string(),
            draft: "dune".to_string(),
            ..SearchState::default()
        };
        // Ingest yts then tpb-movies; the shared Remux kind dedupes into one
        // row with two tags (exactly what the app loop does).
        for src in ["yts", "tpb-movies"] {
            for r in engine.results("dune", src) {
                state
                    .tags
                    .entry(r.info_hash.clone())
                    .or_default()
                    .push(r.source);
                if !state.results.iter().any(|e| e.info_hash == r.info_hash) {
                    state.results.push(r);
                }
            }
            state.source_health.insert(src, SourceStatus::Online);
        }
        state.searching = false;
        (state, DisplayState::default())
    }

    #[test]
    fn search_renders_title_sidebar_and_rows() {
        let (state, display) = fixture_state();
        let lines = render(&state, &display);
        let joined = lines.join("\n");
        assert!(joined.contains("harbour — search"), "frame title");
        assert!(joined.contains("all sources"), "sidebar all row");
        assert!(
            joined.contains("FITGIRL") || joined.contains("GAMES"),
            "group header"
        );
        assert!(joined.contains("FitGirl"), "sidebar source row (label)");
        assert!(joined.contains("REMUX"), "a result row");
        assert!(joined.contains("GB"), "size shown");
        assert!(joined.contains("[YTS]"), "staggered tag 1");
        assert!(joined.contains("[TPB]"), "staggered tag 2");
        assert!(joined.contains("⬆"), "seeders shown");
    }

    #[test]
    fn empty_state_prompts_for_query() {
        let (_, display) = fixture_state();
        let state = SearchState::default();
        let lines = render(&state, &display);
        assert!(
            lines.iter().any(|l| l.contains("type a query")),
            "prompt for first query"
        );
    }

    #[test]
    fn filter_hides_rows_from_other_sources() {
        let (mut state, display) = fixture_state();
        state.filter = SidebarFilter::Source("fitgirl");
        let before = state.results.len();
        let visible = visible_indices(&state);
        assert!(visible.len() < before, "fitgirl filters out movie rows");
        let lines = render(&state, &display);
        let joined = lines.join("\n");
        assert!(!joined.contains("[YTS]"), "filtered rows gone");
    }
}
