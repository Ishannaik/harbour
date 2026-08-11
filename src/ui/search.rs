//! Search view (phase 2): sidebar with per-source health dots, the accent
//! search bar with a shimmer sweep while results stream, and the result list
//! (docs/design.md §2.2).
//!
//! Pure paint: `draw` renders `SearchState` + `Theme`; the app loop owns input
//! and the 30fps tick. The shimmer sweep and the in-bar spinner are periodic,
//! but the draw contract (ui-contract.md) carries no clock — the loop renders
//! at a fixed cadence, so a monotonic phase derived from a module epoch
//! animates them identically without breaking the pure-signature convention.
//! All colors come from the theme subset (docs/theming.md), so custom themes
//! work unchanged.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::types::{SourceGroup, SourceId, SourceStatus, TorrentResult};
use crate::theme::{Color, Theme, ThemeColors};
use crate::ui::SearchState;

/// Panel title — same framing as the downloads view and the splash.
const TITLE: &str = " harbour — search ";
/// Sidebar width (docs/design.md §2.2).
const SIDEBAR_WIDTH: u16 = 22;
/// Search bar height: top border, content row, bottom border.
const SEARCH_BAR_H: u16 = 3;
/// Block cursor glyph at the end of the query — the input's focus marker.
const CURSOR: &str = "▌";
/// Bottom hint — the keybinds that matter on this screen.
const HINT: &str = "enter search · d download · ? help · q quit";
/// Placeholder while the query is empty and idle.
const PLACEHOLDER: &str = "search torrents…";
/// Bar label while searching on an empty query — the curated-browse mode
/// (design §2.2: Enter with no query = top lists).
const BROWSE_LABEL: &str = "browse curated library…";
/// Shimmer sweep period — one white-hot band per cycle, like the splash.
const SHIMMER_PERIOD: Duration = Duration::from_millis(2200);
/// Status-spinner cadence (docs/design.md §Animation: 80ms).
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);
/// White-hot highlight for the shimmer band — the one literal color in the
/// view, mirroring app.rs's HOT (a highlight, not a theme choice).
const HOT: Color = Color::Rgb(255, 255, 255);

/// Sidebar source matrix: group → (source id, label), mirroring the source
/// matrix in docs/sources.md §2. Ids are the engine's canonical SourceId
/// strings; labels are what the user sees.
const SIDEBAR: &[(SourceGroup, &[(SourceId, &str)])] = &[
    (SourceGroup::Games, &[(SourceId::FitGirl, "FitGirl")]),
    (
        SourceGroup::Movies,
        &[
            (SourceId::Yts, "YTS"),
            (SourceId::TpbMovies, "TPB"),
            (SourceId::X1337Movies, "1337x"),
            (SourceId::Bittorrented, "BitTorrented"),
        ],
    ),
    (
        SourceGroup::Tv,
        &[
            (SourceId::Eztv, "EZTV"),
            (SourceId::TpbTv, "TPB"),
            (SourceId::X1337Tv, "1337x"),
        ],
    ),
    (
        SourceGroup::Anime,
        &[
            (SourceId::Nyaa, "Nyaa"),
            (SourceId::SubsPlease, "SubsPlease"),
        ],
    ),
];

/// Monotonic anchor for the periodic effects, initialized on the first draw
/// (see module docs — the contract's signature carries no clock).
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Renders the search screen: sidebar + search bar + result list in one
/// rounded panel (same framing as downloads.rs / the splash).
pub fn draw(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();

    // Rounded panel around the whole screen, like the downloads view.
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

    let cols =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).split(inner);
    draw_sidebar(frame, cols[0], state, theme);
    draw_main(frame, cols[1], state, theme);
}

/// Left column: "Sources" title, then one divider header per group with its
/// sources beneath (dot + label). A group stays dim until one of its sources
/// reports a count — the stagger: groups pop in as sources answer.
fn draw_sidebar(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let colors = &theme.colors;
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Sources",
        Style::default().fg(colors.accent().to_ratatui()),
    )));
    for (group, sources) in SIDEBAR {
        let active = sources
            .iter()
            .any(|(id, _)| state.source_counts.contains_key(id));
        // Divider header (mirrors the "recently downloaded" line): label,
        // then border glyphs filling the rest of the row.
        let label = group.label();
        let rest = width.saturating_sub(label.chars().count() + 2).max(1);
        let head_color = if active { colors.muted() } else { colors.dim() };
        lines.push(Line::from(Span::styled(
            format!(" {label} {}", theme.symbols.border_h.as_ref().repeat(rest)),
            Style::default().fg(head_color.to_ratatui()),
        )));
        for (id, label) in *sources {
            lines.push(source_line(*id, label, state, theme, width));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// One source row: health dot + label. A missing dot renders the muted
/// placeholder so an unprobed source reads "unknown", not "down".
fn source_line(
    id: SourceId,
    label: &'static str,
    state: &SearchState,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let colors = &theme.colors;
    // A source that has not answered *yet* must not read as dead: `Checking`
    // renders the live glyph muted (Ishan's pending dot), while never-probed
    // stays the neutral placeholder.
    let (dot, dot_color) = match state.source_health.get(&id).copied() {
        Some(SourceStatus::Online) => (theme.symbols.dot_online.as_ref(), colors.success()),
        Some(SourceStatus::Offline) => (theme.symbols.dot_offline.as_ref(), colors.error()),
        Some(SourceStatus::Empty) => (theme.symbols.dot_offline.as_ref(), colors.warning()),
        Some(SourceStatus::Checking) => (theme.symbols.dot_online.as_ref(), colors.muted()),
        Some(SourceStatus::Unknown) | None => ("·", colors.muted()),
    };
    Line::from(vec![
        Span::styled(
            format!("  {dot} "),
            Style::default().fg(dot_color.to_ratatui()),
        ),
        Span::styled(
            truncate(label, width.saturating_sub(4)),
            Style::default().fg(colors.text().to_ratatui()),
        ),
    ])
}

/// Right column: search bar, results header, result list, hint line.
fn draw_main(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let rows = Layout::vertical([
        Constraint::Length(SEARCH_BAR_H),
        Constraint::Length(1), // results header
        Constraint::Min(0),    // result list
        Constraint::Length(1), // hint line
    ])
    .split(area);
    let elapsed = clock();
    draw_search_bar(frame, rows[0], state, theme, elapsed);
    draw_header(frame, rows[1], state, theme);
    draw_results(frame, rows[2], state, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            HINT,
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ))),
        rows[3],
    );
}

/// The accent-bordered input: query + block cursor, spinner at the right
/// while a search runs. The view owns input focus in this phase, so the
/// border is always accent — a focus field gates this once other views
/// can own the input.
fn draw_search_bar(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    theme: &Theme,
    elapsed: Duration,
) {
    if area.width < 6 || area.height < SEARCH_BAR_H {
        return; // no room for both border rows and the content row
    }
    let colors = &theme.colors;
    let s = &theme.symbols;
    let accent = Style::default().fg(colors.accent().to_ratatui());
    let inner = (area.width - 2) as usize;
    let fill = s.border_h.as_ref().repeat(inner);
    let (top, bottom) = (
        format!("{}{}{}", s.border_tl.as_ref(), fill, s.border_tr.as_ref()),
        format!("{}{}{}", s.border_bl.as_ref(), fill, s.border_br.as_ref()),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(top, accent))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(bottom, accent))),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );

    let spinner = if state.searching {
        spinner_frame(theme, elapsed)
    } else {
        ""
    };
    // Empty query = browse mode (design §2.2), so the bar labels it as such
    // while a search runs instead of pretending there is a query to edit.
    let (text, base) = match (state.query.is_empty(), state.searching) {
        (true, true) => (BROWSE_LABEL, colors.accent()),
        (true, false) => (PLACEHOLDER, colors.dim()),
        (false, true) => (state.query.as_str(), colors.accent()),
        (false, false) => (state.query.as_str(), colors.text()),
    };
    let max_q = inner.saturating_sub(3 + spinner.chars().count()); // 2 pads + cursor
    let q = truncate(text, max_q);
    let q_len = q.chars().count();

    let mut spans = vec![
        Span::styled(s.border_v.as_ref().to_string(), accent),
        Span::raw(" "),
    ];
    if state.searching {
        // Shimmer: a white-hot gaussian band sweeps the bar (the app.rs
        // pattern), lerped over the accent base; off-band glyphs stay accent.
        // Runs of equal color merge into one span so a long query doesn't
        // emit one span per char per frame (same trick as app.rs push_run).
        let mut run = String::new();
        let mut run_color = Color::Default;
        for (col, ch) in q.chars().enumerate() {
            let band = shimmer_intensity(col, inner, elapsed);
            let c = if band > 0.02 {
                lerp_color(colors.accent(), HOT, 0.8 * band)
            } else {
                colors.accent()
            };
            if c != run_color {
                if !run.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        Style::default().fg(run_color.to_ratatui()),
                    ));
                }
                run_color = c;
            }
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(Span::styled(
                run,
                Style::default().fg(run_color.to_ratatui()),
            ));
        }
    } else {
        spans.push(Span::styled(q, Style::default().fg(base.to_ratatui())));
    }
    spans.push(Span::styled(
        CURSOR,
        Style::default().fg(colors.accent().to_ratatui()),
    ));
    let pad = inner.saturating_sub(1 + q_len + 1 + spinner.chars().count());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    if !spinner.is_empty() {
        spans.push(Span::styled(
            spinner,
            Style::default().fg(colors.accent().to_ratatui()),
        ));
    }
    spans.push(Span::styled(s.border_v.as_ref().to_string(), accent));
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

/// Current spinner frame for the 80ms cadence — the index derives from the
/// module epoch clock (see module docs for why draw() times itself).
fn spinner_frame(theme: &Theme, elapsed: Duration) -> &str {
    let frames = &theme.symbols.spinner_frames;
    if frames.is_empty() {
        return "";
    }
    let idx = (elapsed.as_millis() / SPINNER_INTERVAL.as_millis()) as usize % frames.len();
    &frames[idx]
}

/// Time since the first draw — the animation phase for shimmer and spinner.
fn clock() -> Duration {
    Instant::now().duration_since(*EPOCH)
}

/// Gaussian shimmer band: highlight 0..=1 per column, peaking at the band
/// center (~4 columns wide at half height), sweeping once per period
/// (same shape as app.rs).
fn shimmer_intensity(column: usize, width: usize, elapsed: Duration) -> f64 {
    let cycle = elapsed.as_secs_f64() / SHIMMER_PERIOD.as_secs_f64();
    let center = (cycle % 1.0) * width as f64;
    let d = (column as f64 - center).abs();
    (-(d * d) / (2.0 * 1.7 * 1.7)).exp()
}

/// Linear interpolation between two RGB colors; non-RGB endpoints fall back
/// to `a` (mirrors app.rs — Index/Default can't be blended).
fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => Color::Rgb(
            (ar as f64 + (br as f64 - ar as f64) * t).round() as u8,
            (ag as f64 + (bg as f64 - ag as f64) * t).round() as u8,
            (ab as f64 + (bb as f64 - ab as f64) * t).round() as u8,
        ),
        _ => a,
    }
}

/// Results header: "N results from M sources" — or, on an empty query, the
/// browse-mode line (design §2.2: Enter with no query = curated top lists).
fn draw_header(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let text = if state.query.is_empty() {
        "browse the curated library".to_string()
    } else {
        format!(
            "{} results from {} sources",
            state.results.len(),
            distinct_sources(&state.results)
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ))),
        area,
    );
}

/// Number of distinct sources among the results — walks the fixed registry
/// instead of allocating a set per frame.
fn distinct_sources(results: &[TorrentResult]) -> usize {
    let mut seen = 0;
    for (_, list) in SIDEBAR {
        for (id, _) in *list {
            if results.iter().any(|r| r.source == *id) {
                seen += 1;
            }
        }
    }
    seen
}

/// Result list: one row per result, scrolled so the selection stays visible.
/// The empty state names the next action instead of sitting blank.
fn draw_results(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    if state.results.is_empty() {
        let msg = if state.searching {
            "searching…"
        } else {
            "no results yet — press Enter to search"
        };
        frame.render_widget(Paragraph::new(empty_line(msg, theme)), area);
        return;
    }
    let width = area.width as usize;
    let vis = area.height as usize;
    let start = scroll_start(state.results.len(), state.selected, vis);
    let mut lines: Vec<Line> = Vec::new();
    for (i, result) in state
        .results
        .iter()
        .enumerate()
        .skip(start)
        .take(vis.max(1))
    {
        lines.push(result_line(
            result,
            i == state.selected,
            state,
            theme,
            width,
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// One result row: name (accent when selected), then right-aligned size,
/// seeders, leechers, and the source chip. The chip stays dim until its
/// source reports a count — the staggered pop-in as sources answer
/// (design §2.2).
fn result_line(
    result: &TorrentResult,
    selected: bool,
    state: &SearchState,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let colors = &theme.colors;
    let chip = format!("[{}]", chip_label(result.source));
    let size = fmt_size(result.size_bytes);
    // Zero seeders/leechers means the source doesn't report health (e.g.
    // RSS feeds, docs/sources.md §3.7) — '—', never 0, so no fake health.
    let seeds = if result.seeders > 0 {
        result.seeders.to_string()
    } else {
        "—".into()
    };
    let leeches = if result.leechers > 0 {
        result.leechers.to_string()
    } else {
        "—".into()
    };
    let suffix_w = size.chars().count()
        + seeds.chars().count()
        + leeches.chars().count()
        + chip.chars().count()
        + 3; // one gap before each of the four right-aligned cells
    let name = truncate(&result.name, width.saturating_sub(suffix_w + 1));
    let pad = width.saturating_sub(name.chars().count() + suffix_w);
    let name_fg = if selected {
        colors.accent()
    } else {
        colors.text()
    };
    let seed_fg = if result.seeders > 0 {
        colors.success()
    } else {
        colors.muted()
    };
    let chip_fg = if state.source_counts.contains_key(&result.source) {
        colors.text()
    } else {
        colors.dim()
    };
    let spans = vec![
        Span::styled(name, Style::default().fg(name_fg.to_ratatui())),
        Span::raw(" ".repeat(pad)),
        Span::styled(size, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" "),
        Span::styled(seeds, Style::default().fg(seed_fg.to_ratatui())),
        Span::raw(" "),
        Span::styled(leeches, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" "),
        Span::styled(chip, Style::default().fg(chip_fg.to_ratatui())),
    ];
    row_line(spans, selected, colors)
}

/// Narrow chip text per source — shorter than the sidebar label so names
/// keep their width (design's `[x1337-m]` style).
fn chip_label(id: SourceId) -> &'static str {
    // The two sources that appear in both a Movies and a TV row share a chip:
    // the row already says which category it is.
    match id {
        SourceId::FitGirl => "fitgirl",
        SourceId::Yts => "yts",
        SourceId::TpbMovies | SourceId::TpbTv => "tpb",
        SourceId::X1337Movies | SourceId::X1337Tv => "x1337",
        SourceId::Bittorrented => "bttr",
        SourceId::Eztv => "eztv",
        SourceId::Nyaa => "nyaa",
        SourceId::SubsPlease => "subsplease",
    }
}

/// Human-readable size in GB/MB/KB (binary units, matching the wireframe's
/// "48.2 GB"); 0 bytes renders '—' — sources like SubsPlease report no size
/// (docs/sources.md §3.7), so a fake number would be worse than none.
fn fmt_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".to_string();
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{:.0} MB", mb)
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

/// Window start so the selected row stays visible; 0 when it fits or is out
/// of range (a stale selection after a fresh search). No scrollbar in
/// phase 2 (same rule as downloads.rs).
fn scroll_start(len: usize, selected: usize, vis: usize) -> usize {
    if vis > 0 && (vis..len).contains(&selected) {
        selected - vis + 1
    } else {
        0
    }
}

/// Selected-row background as the line's base style; spans set only fg, so
/// the highlight shows through every cell.
fn row_line(spans: Vec<Span<'static>>, selected: bool, colors: &ThemeColors) -> Line<'static> {
    let base = if selected {
        Style::default().bg(colors.selected_bg().to_ratatui())
    } else {
        Style::default()
    };
    // `Line::styled` builds a line from *text*; a line already made of spans
    // takes its base style via `.style()`. Same result, and the per-span fg
    // still wins over the line's bg.
    Line::from(spans).style(base)
}

/// One muted, left-aligned line for empty states.
fn empty_line(text: &str, theme: &Theme) -> Line<'static> {
    // Owned: the returned Line outlives the borrowed `text`.
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme.colors.muted().to_ratatui()),
    ))
}

/// Truncate to `max` cells, replacing the last with '…', so a long name
/// never bleeds into the right-aligned columns.
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
