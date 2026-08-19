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

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::core::types::{SourceGroup, SourceId, SourceStatus, TorrentResult};
use crate::theme::{Color, Theme, ThemeColors};
use crate::ui::SearchState;

/// Panel title — same framing as the downloads view and the splash.
const TITLE: &str = " harbour — search ";
/// Sidebar width (docs/design.md §2.2). Shared with `input.rs`'s mouse
/// mapping: the results column starts one cell past the sidebar.
pub(crate) const SIDEBAR_WIDTH: u16 = 22;
/// Search bar height: top border, content row, bottom border. Shared with
/// `input.rs`'s mouse mapping: results start below the bar and its header.
pub(crate) const SEARCH_BAR_H: u16 = 3;
/// Block cursor glyph at the end of the query — the input's focus marker.
const CURSOR: &str = "▌";
/// Bottom hint — the keybinds that matter on this screen.
///
/// Every printable key types on this screen (so "dune" can never fire a
/// download on the `d`); downloading is shift+D mid-query, or plain `d`
/// with an empty query.
const HINT: &str = "↵ search · tab downloads · s settings · ? help · esc results";
/// The results pane owns the keyboard: plain keys act on the selected row.
/// Kept short so it fits the main column beside the sidebar (issue #71).
const RESULTS_HINT: &str = "enter watch · d download · shift+P player · dbl-click";
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
            (SourceId::Bittorrented, "BitTorrented"),
        ],
    ),
    (
        SourceGroup::Tv,
        &[
            (SourceId::Eztv, "EZTV"),
            (SourceId::TpbTv, "TPB"),
            (SourceId::ShowRss, "showRSS"),
        ],
    ),
    (
        SourceGroup::Anime,
        &[
            (SourceId::Nyaa, "Nyaa"),
            (SourceId::SubsPlease, "SubsPlease"),
            (SourceId::AnimeTosho, "AnimeTosho"),
        ],
    ),
];

/// Monotonic anchor for the periodic effects, initialized on the first draw
/// (see module docs — the contract's signature carries no clock).
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Maps a sidebar row to the source it renders, or `None` for rows that are
/// not a source: the "Sources" title (row 0), the group dividers, and rows
/// past the last source. The offset is relative to the sidebar's inner area,
/// so `input.rs` can hit-test clicks against the same matrix the painter
/// draws without duplicating its layout.
pub fn sidebar_source_at(row: u16) -> Option<SourceId> {
    // Row 0 is the "Sources" title; the first source row sits at offset 1.
    let mut cursor = row.saturating_sub(1);
    for (_, sources) in SIDEBAR {
        // Each group spends one row on its divider header.
        if cursor == 0 {
            return None;
        }
        cursor -= 1;
        for (id, _) in *sources {
            if cursor == 0 {
                return Some(*id);
            }
            cursor -= 1;
        }
    }
    None
}

/// Renders the search screen: sidebar + search bar + result list in one
/// rounded panel (same framing as downloads.rs / the splash).
///
/// `disabled` is the app's current set of user-disabled sources, so the
/// sidebar can paint them dim and inert.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    disabled: &HashSet<SourceId>,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
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
    draw_sidebar(frame, cols[0], state, disabled, theme, mouse_pos);
    draw_main(frame, cols[1], state, disabled, theme, mouse_pos);
}

/// Left column: "Sources" title, then one divider header per group with its
/// sources beneath (dot + label). A group stays dim until one of its sources
/// reports a count — the stagger: groups pop in as sources answer.
fn draw_sidebar(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    disabled: &HashSet<SourceId>,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
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
            let row_y = area.y + lines.len() as u16;
            let hovered =
                mouse_pos.is_some_and(|(mx, my)| mx >= area.x && mx < area.right() && my == row_y);
            lines.push(source_line(
                *id,
                disabled.contains(id),
                label,
                state,
                theme,
                width,
                hovered,
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// One source row: health dot + label. A missing dot renders the muted
/// placeholder so an unprobed source reads "unknown", not "down". A
/// user-disabled source is drawn dim and inert — the neutral dot, no health
/// color — so the sidebar reads "off" at a glance.
fn source_line(
    id: SourceId,
    disabled: bool,
    label: &'static str,
    state: &SearchState,
    theme: &Theme,
    width: usize,
    hovered: bool,
) -> Line<'static> {
    let colors = &theme.colors;
    let base_style = if hovered {
        Style::default().bg(colors.selected_bg().to_ratatui())
    } else {
        Style::default()
    };

    // Disabled wins over every health state: the row is deliberately flat.
    if disabled {
        let (dot_str, fg) = if hovered {
            ("  ▸ ", colors.text().to_ratatui())
        } else {
            ("  · ", colors.dim().to_ratatui())
        };
        return Line::from(vec![
            Span::styled(dot_str, Style::default().fg(fg)),
            Span::styled(
                truncate(label, width.saturating_sub(4)),
                Style::default().fg(fg),
            ),
        ])
        .style(base_style);
    }
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
    let text_fg = if hovered {
        colors.accent().to_ratatui()
    } else {
        colors.text().to_ratatui()
    };
    Line::from(vec![
        Span::styled(
            format!("  {dot} "),
            Style::default().fg(dot_color.to_ratatui()),
        ),
        Span::styled(
            truncate(label, width.saturating_sub(4)),
            Style::default().fg(text_fg),
        ),
    ])
    .style(base_style)
}

/// Right column: search bar, results header, result list, hint line.
fn draw_main(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    disabled: &HashSet<SourceId>,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    let rows = Layout::vertical([
        Constraint::Length(SEARCH_BAR_H),
        Constraint::Length(1), // results header
        Constraint::Min(0),    // result list
        Constraint::Length(1), // hint line
    ])
    .split(area);
    let elapsed = clock();
    draw_search_bar(frame, rows[0], state, theme, elapsed, mouse_pos);
    draw_header(frame, rows[1], state, theme, mouse_pos);
    draw_results(frame, rows[2], state, disabled, theme, mouse_pos);
    let hint = if state.focus { HINT } else { RESULTS_HINT };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
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
    mouse_pos: Option<(u16, u16)>,
) {
    if area.width < 6 || area.height < SEARCH_BAR_H {
        return; // no room for both border rows and the content row
    }
    let colors = &theme.colors;
    let s = &theme.symbols;
    // The bar is the mode indicator: accent + cursor means "typing here",
    // muted + a label means "results focused — esc to type". Clicking the
    // bar returns focus to the input (mouse_to_action).
    let input_focused = state.focus;
    let is_hovered = mouse_pos.is_some_and(|(mx, my)| {
        mx >= area.x && mx < area.right() && my >= area.y && my < area.bottom()
    });
    let accent = Style::default().fg(colors.accent().to_ratatui());
    let muted = Style::default().fg(colors.muted().to_ratatui());
    let border_style = if input_focused || is_hovered {
        accent
    } else {
        muted
    };
    let inner = (area.width - 2) as usize;
    let fill = s.border_h.as_ref().repeat(inner);
    let (top, bottom) = (
        format!("{}{}{}", s.border_tl.as_ref(), fill, s.border_tr.as_ref()),
        format!("{}{}{}", s.border_bl.as_ref(), fill, s.border_br.as_ref()),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(top, border_style))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(bottom, border_style))),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );

    let spinner = if state.searching {
        spinner_frame(theme, elapsed)
    } else {
        ""
    };

    let mut spans = vec![
        Span::styled(s.border_v.as_ref().to_string(), border_style),
        Span::raw(" "),
    ];

    if !input_focused {
        // The results pane owns the keyboard: the bar says so, and says how
        // to get back to typing. No cursor — nothing is being typed.
        let label = if is_hovered {
            "click or type to focus search input"
        } else {
            "results focused — esc or backspace to type"
        };
        let label_style = if is_hovered { accent } else { muted };
        spans.push(Span::styled(
            truncate(label, inner.saturating_sub(3)),
            label_style,
        ));
        let pad = inner.saturating_sub(2 + label.chars().count());
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        if !spinner.is_empty() {
            spans.push(Span::styled(spinner, label_style));
        }
        spans.push(Span::styled(s.border_v.as_ref().to_string(), border_style));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        return;
    }

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
            if c == run_color {
                run.push(ch);
                continue;
            }
            if !run.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    Style::default().fg(run_color.to_ratatui()),
                ));
            }
            run_color = c;
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

use crate::ui::{SortColumn, SortOrder};

pub(crate) const COL_SIZE_W: usize = 9;
pub(crate) const COL_SEEDS_W: usize = 6;
pub(crate) const COL_LEECH_W: usize = 5;
pub(crate) const COL_QUAL_W: usize = 8;
pub(crate) const COL_SOURCE_W: usize = 10;
pub(crate) const SUFFIX_TOTAL_W: usize =
    COL_SIZE_W + COL_SEEDS_W + COL_LEECH_W + COL_QUAL_W + COL_SOURCE_W; // 38

/// Returns the sort column under a click or hover at `(col, row)` in the header area.
pub fn header_sort_col_at(area: Rect, col: u16, row: u16) -> Option<SortColumn> {
    if row != area.y {
        return None;
    }
    if col >= area.x && col < area.x + 6 {
        return Some(SortColumn::Name);
    }
    let suffix_x = area.right().saturating_sub(SUFFIX_TOTAL_W as u16);
    if col < suffix_x {
        return None;
    }
    let rel_x = (col - suffix_x) as usize;
    if rel_x < COL_SIZE_W {
        Some(SortColumn::Size)
    } else if rel_x < COL_SIZE_W + COL_SEEDS_W {
        Some(SortColumn::Seeds)
    } else if rel_x < COL_SIZE_W + COL_SEEDS_W + COL_LEECH_W {
        Some(SortColumn::Leechers)
    } else if rel_x < COL_SIZE_W + COL_SEEDS_W + COL_LEECH_W + COL_QUAL_W {
        None
    } else if rel_x < SUFFIX_TOTAL_W {
        Some(SortColumn::Source)
    } else {
        None
    }
}

fn col_badge(
    label: &str,
    col: SortColumn,
    current: SortColumn,
    order: SortOrder,
    width: usize,
    align_right: bool,
) -> String {
    let arrow = if current == col {
        match order {
            SortOrder::Asc => "▲",
            SortOrder::Desc => "▼",
        }
    } else {
        ""
    };
    let text = if arrow.is_empty() {
        label.to_string()
    } else {
        format!("{label}{arrow}")
    };
    if align_right {
        format!("{:>width$}", text)
    } else {
        format!("{:<width$}", text)
    }
}

fn header_span(
    text: String,
    col: SortColumn,
    current: SortColumn,
    hovered: bool,
    colors: &ThemeColors,
) -> Span<'static> {
    let fg = if current == col {
        colors.accent().to_ratatui()
    } else if hovered {
        colors.text().to_ratatui()
    } else {
        colors.dim().to_ratatui()
    };
    Span::styled(text, Style::default().fg(fg))
}

/// Results header: count line + column labels with sort indicators and hover styling.
fn draw_header(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    let colors = &theme.colors;
    let latency_str = state.latency_ms.map_or(String::new(), |ms| {
        format!(" in {:.1}s", ms as f64 / 1000.0)
    });
    let count = if state.query.is_empty() {
        if state.results.is_empty() {
            "browse the curated library".to_string()
        } else {
            format!(
                "browse the curated library ({} items{})",
                state.results.len(),
                latency_str
            )
        }
    } else {
        format!(
            "{} results from {} sources{}",
            state.results.len(),
            distinct_sources(&state.results),
            latency_str
        )
    };
    let width = area.width as usize;
    let count_w = width.saturating_sub(SUFFIX_TOTAL_W + 1 + 5); // 5: "name" + gap
    let count = truncate(&count, count_w);
    let pad = width.saturating_sub(5 + count.chars().count() + SUFFIX_TOTAL_W);

    let h_col = mouse_pos.and_then(|(mx, my)| header_sort_col_at(area, mx, my));
    let cur = state.sort_column;
    let ord = state.sort_order;

    let name_arrow = if cur == SortColumn::Name {
        if ord == SortOrder::Asc { "▲" } else { "▼" }
    } else {
        ""
    };
    let name_label = format!("name{name_arrow}");

    let size_arrow = if cur == SortColumn::Size {
        if ord == SortOrder::Asc { "▲" } else { "▼" }
    } else {
        ""
    };
    let size_label = format!("{:<COL_SIZE_W$}", format!("size{size_arrow}"));

    let seed_arrow = if cur == SortColumn::Seeds {
        if ord == SortOrder::Asc { "▲" } else { "▼" }
    } else {
        ""
    };
    let seed_label = format!("{:>COL_SEEDS_W$}", format!("seed{seed_arrow}"));

    let leech_arrow = if cur == SortColumn::Leechers {
        if ord == SortOrder::Asc { "▲" } else { "▼" }
    } else {
        ""
    };
    let leech_label = format!("{:<COL_LEECH_W$}", format!("leech{leech_arrow}"));

    let quality_label = format!("{:<COL_QUAL_W$}", "quality");

    let source_arrow = if cur == SortColumn::Source {
        if ord == SortOrder::Asc { "▲" } else { "▼" }
    } else {
        ""
    };
    let source_label = format!("{:<COL_SOURCE_W$}", format!("source{source_arrow}"));

    let spans = vec![
        header_span(
            name_label,
            SortColumn::Name,
            cur,
            h_col == Some(SortColumn::Name),
            colors,
        ),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" ".repeat(pad)),
        header_span(
            size_label,
            SortColumn::Size,
            cur,
            h_col == Some(SortColumn::Size),
            colors,
        ),
        header_span(
            seed_label,
            SortColumn::Seeds,
            cur,
            h_col == Some(SortColumn::Seeds),
            colors,
        ),
        header_span(
            leech_label,
            SortColumn::Leechers,
            cur,
            h_col == Some(SortColumn::Leechers),
            colors,
        ),
        Span::styled(
            quality_label,
            Style::default().fg(colors.dim().to_ratatui()),
        ),
        header_span(
            source_label,
            SortColumn::Source,
            cur,
            h_col == Some(SortColumn::Source),
            colors,
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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

fn searching_source_line<'a>(
    id: SourceId,
    label: &'static str,
    group_name: &SourceGroup,
    state: &SearchState,
    theme: &Theme,
    spinner: &'a str,
) -> Line<'a> {
    let health = state
        .source_health
        .get(&id)
        .copied()
        .unwrap_or(SourceStatus::Unknown);
    let count = state.source_counts.get(&id).copied().unwrap_or(0);
    let (dot, status_str, style) = match health {
        SourceStatus::Online => {
            let text = if count > 0 {
                format!("{count} results found")
            } else {
                "ready".to_string()
            };
            (
                "●",
                text,
                Style::default().fg(theme.colors.success().to_ratatui()),
            )
        }
        // Source health, not a BitTorrent handshake — `connecting…` made this
        // look like torrent-peer UX (issue #75 / FR-32).
        SourceStatus::Checking => (
            spinner,
            "checking source…".to_string(),
            Style::default().fg(theme.colors.accent().to_ratatui()),
        ),
        SourceStatus::Offline => (
            "○",
            "offline".to_string(),
            Style::default().fg(theme.colors.dim().to_ratatui()),
        ),
        SourceStatus::Empty => (
            "○",
            "0 results".to_string(),
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ),
        SourceStatus::Unknown => (
            "·",
            "querying…".to_string(),
            Style::default().fg(theme.colors.dim().to_ratatui()),
        ),
    };
    Line::from(vec![
        Span::raw("    "),
        Span::styled(dot, style),
        Span::raw(" "),
        Span::styled(
            format!("{label:<14}"),
            Style::default().fg(theme.colors.text().to_ratatui()),
        ),
        Span::styled(
            format!("{status_str:<18} · {}", group_name.label()),
            Style::default().fg(theme.colors.muted().to_ratatui()),
        ),
    ])
}

/// Result list: one row per result, scrolled so the selection stays visible.
/// The empty state names the next action instead of sitting blank.
fn draw_results(
    frame: &mut Frame,
    area: Rect,
    state: &SearchState,
    disabled: &HashSet<SourceId>,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) {
    if state.results.is_empty() {
        if state.searching {
            let spinner = spinner_frame(theme, clock());
            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {spinner} "),
                    Style::default().fg(theme.colors.accent().to_ratatui()),
                ),
                Span::styled(
                    "Searching across sources in parallel…",
                    Style::default().fg(theme.colors.accent().to_ratatui()),
                ),
            ]));
            lines.push(Line::from(""));

            collect_searching_sources(&mut lines, disabled, state, theme, spinner);
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }
        let msg = "no results yet — press Enter to search";
        frame.render_widget(Paragraph::new(empty_line(msg, theme)), area);
        return;
    }
    let vis = area.height as usize;
    let has_scrollbar = state.results.len() > vis;
    let width = if has_scrollbar {
        (area.width as usize).saturating_sub(1)
    } else {
        area.width as usize
    };
    let start = scroll_start(state.results.len(), state.selected, vis);
    let mut lines: Vec<Line> = Vec::new();
    for (rel_idx, (i, result)) in state
        .results
        .iter()
        .enumerate()
        .skip(start)
        .take(vis.max(1))
        .enumerate()
    {
        let row_y = area.y + rel_idx as u16;
        let hovered =
            mouse_pos.is_some_and(|(mx, my)| mx >= area.x && mx < area.right() && my == row_y);
        lines.push(result_line(
            result,
            i == state.selected,
            hovered,
            state,
            theme,
            width,
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
    if has_scrollbar {
        let max_scroll = state.results.len().saturating_sub(vis);
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

fn collect_searching_sources<'a>(
    lines: &mut Vec<Line<'a>>,
    disabled: &HashSet<SourceId>,
    state: &'a SearchState,
    theme: &'a Theme,
    spinner: &'a str,
) {
    for (group_name, sources) in SIDEBAR {
        for (id, label) in *sources {
            if !disabled.contains(id) {
                lines.push(searching_source_line(
                    *id, label, group_name, state, theme, spinner,
                ));
            }
        }
    }
}

/// One result row: name (accent when selected or hovered), then right-aligned size,
/// seeders, leechers, the quality chip (only when the title names one),
/// and the source chip. Both chips stay dim until their source reports a
/// count — the staggered pop-in as sources answer (design §2.2).
fn result_line(
    result: &TorrentResult,
    selected: bool,
    hovered: bool,
    state: &SearchState,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let colors = &theme.colors;
    let quality = quality_tag(&result.name).map(|tag| format!("[{tag}]"));
    let chip = format!("[{}]", chip_label(result.source));
    let size = fmt_size(result.size_bytes);
    let seeds = swarm_cell(result.seeders, reports_health(result.source));
    let leeches = swarm_cell(result.leechers, reports_health(result.source));
    let name_width = width.saturating_sub(SUFFIX_TOTAL_W + 1);
    let name = marquee_text(&result.name, name_width, selected || hovered, clock());
    let pad = width.saturating_sub(name.chars().count() + SUFFIX_TOTAL_W);
    let name_fg = if selected || hovered {
        colors.accent()
    } else {
        colors.text()
    };
    let seed_fg = if result.seeders > 0 {
        health_color(result.seeders, colors)
    } else {
        colors.dim()
    };
    let leech_fg = if result.leechers > 0 {
        health_color(result.leechers, colors)
    } else {
        colors.dim()
    };
    let chip_fg = if state.source_counts.contains_key(&result.source) {
        colors.text()
    } else {
        colors.dim()
    };
    let size_col = format!("{:<COL_SIZE_W$}", truncate(&size, COL_SIZE_W));
    let seed_col = format!("{:>COL_SEEDS_W$}", truncate(&seeds, COL_SEEDS_W));
    let leech_col = format!("{:>4} ", truncate(&leeches, COL_LEECH_W - 1));
    let (quality_col, quality_fg) = match quality {
        Some(ref q) => (format!("{:<COL_QUAL_W$}", truncate(q, COL_QUAL_W)), chip_fg),
        None => (" ".repeat(COL_QUAL_W), colors.dim()),
    };
    let chip_col = format!("{:<COL_SOURCE_W$}", truncate(&chip, COL_SOURCE_W));

    let spans = vec![
        Span::styled(name, Style::default().fg(name_fg.to_ratatui())),
        Span::raw(" ".repeat(pad)),
        // FR-23: size is a primary column. muted() on selectedBg is ~1.8:1
        // and reads as a blank cell (#69); match the name's text() token.
        Span::styled(size_col, Style::default().fg(colors.text().to_ratatui())),
        Span::styled(seed_col, Style::default().fg(seed_fg.to_ratatui())),
        Span::styled(leech_col, Style::default().fg(leech_fg.to_ratatui())),
        Span::styled(quality_col, Style::default().fg(quality_fg.to_ratatui())),
        Span::styled(chip_col, Style::default().fg(chip_fg.to_ratatui())),
    ];
    row_line(spans, selected, hovered, colors)
}

fn marquee_text(text: &str, width: usize, active: bool, elapsed: Duration) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if !active {
        return truncate(text, width);
    }
    let overflow = count.saturating_sub(width);
    let total_steps = overflow + 8;
    let step = (elapsed.as_millis() / 200) as usize % total_steps;
    let offset = if step < 4 {
        0
    } else if step <= 4 + overflow {
        step - 4
    } else {
        overflow
    };
    text.chars().skip(offset).take(width).collect()
}

/// Whether this site's feed carries trustworthy swarm counts.
///
/// FitGirl is a blog and SubsPlease RSS has no seed fields — harbour-indexer
/// sets `reports_health` false, so `seeders: 0` means *unknown*, not *dead*.
fn reports_health(id: SourceId) -> bool {
    !matches!(id, SourceId::FitGirl | SourceId::SubsPlease)
}

/// Format a seeder/leecher count (FR-24). Unknown health is an em dash, never
/// a guessed swarm band. A source that reports health renders a real 0.
fn swarm_cell(count: u32, reports_health: bool) -> String {
    if count > 0 {
        count.to_string()
    } else if reports_health {
        "0".into()
    } else {
        "—".into()
    }
}

/// Health tier for a seeder/leecher count (FR-24): >=100 is a healthy swarm
/// (success), 1–99 is alive but thin (warning), 0 is unseeded (dim). Same
/// three tiers for both columns, so a row's health reads at a glance.
fn health_color(count: u32, colors: &ThemeColors) -> Color {
    if count >= 100 {
        colors.success()
    } else if count > 0 {
        colors.warning()
    } else {
        colors.dim()
    }
}

/// Quality tag from a release name — the second chip on a result row.
///
/// Case-insensitive, first match wins in priority order (4k before 1080p,
/// resolution before hdr/dv, quality before codec). A token is only a tag
/// when no digit sits immediately before or after it: "2021" must never
/// read as "720", while "1080p" still matches "1080".
fn quality_tag(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    const TAGS: &[(&[&str], &str)] = &[
        (&["4k", "2160"], "4k"),
        (&["1080"], "1080p"),
        (&["720"], "720p"),
        (&["hdr"], "hdr"),
        (&["dv", "dovi"], "dv"),
        (&["remux"], "remux"),
        (&["bluray"], "bluray"),
        (&["web-dl"], "web-dl"),
        (&["x265", "hevc"], "x265"),
    ];
    for (needles, tag) in TAGS {
        if needles.iter().any(|n| has_tag_token(&lower, n)) {
            return Some(tag);
        }
    }
    None
}

/// True when `needle` occurs in `hay` with no digit immediately before or
/// after it. Byte offsets are safe: every needle is ASCII, and UTF-8
/// continuation bytes are never ASCII digits.
fn has_tag_token(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = i.checked_sub(1).and_then(|j| hay.as_bytes().get(j));
        let after = hay.as_bytes().get(i + needle.len());
        !before.is_some_and(u8::is_ascii_digit) && !after.is_some_and(u8::is_ascii_digit)
    })
}

/// Narrow chip text per source — shorter than the sidebar label so names
/// keep their width (design's `[x1337-m]` style).
fn chip_label(id: SourceId) -> &'static str {
    // The two sources that appear in both a Movies and a TV row share a chip:
    // the row already says which category it is.
    match id {
        SourceId::Indexer => "indexer",
        SourceId::FitGirl => "fitgirl",
        SourceId::Yts => "yts",
        SourceId::TpbMovies | SourceId::TpbTv => "tpb",
        SourceId::X1337Movies | SourceId::X1337Tv => "x1337",
        SourceId::Bittorrented => "bttr",
        SourceId::Eztv => "eztv",
        SourceId::Nyaa => "nyaa",
        SourceId::SubsPlease => "subs",
        SourceId::AnimeTosho => "tosho",
        SourceId::ShowRss => "showrss",
    }
}

/// Human-readable size in binary units (B/KiB/MiB/GiB/TiB, one decimal from
/// 1 KiB up), mirroring the downloads view's `human_bytes` exactly so the two
/// views speak one size language; 0 bytes renders '—' — sources like
/// SubsPlease report no size (docs/sources.md §3.7), so a fake number would
/// be worse than none.
fn fmt_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".to_string();
    }
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use std::collections::HashSet;

    use crate::core::types::{SourceId, TorrentResult};

    /// A deterministic YTS result — the name is the only varying field.
    fn result(name: &str) -> TorrentResult {
        TorrentResult {
            info_hash: "test-hash".into(),
            name: name.to_string(),
            size_bytes: 5 * 1024 * 1024 * 1024,
            seeders: 120,
            leechers: 5,
            num_files: Some(3),
            source: SourceId::Yts,
            magnet: Some("magnet:?xt=urn:btih:test".into()),
            added: Some(1_786_000_000),
        }
    }

    #[test]
    fn results_hint_names_the_player_picker() {
        let state = SearchState {
            focus: false,
            ..SearchState::default()
        };
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), &state, &HashSet::new(), &theme, None))
            .expect("draw");
        let buf = terminal.backend().buffer();
        let text: String = (0..24)
            .flat_map(|y| (0..80).map(move |x| buf[(x, y)].symbol().to_string()))
            .collect();
        assert!(
            text.contains("shift+P") && text.contains("player"),
            "results footer must name the picker, got: {text}"
        );
    }

    #[test]
    fn quality_tag_matches_every_tag() {
        let cases = [
            ("The Movie 4K", "4k"),
            ("The Movie 2160p", "4k"),
            ("The Movie 1080p", "1080p"),
            ("The Movie 720p", "720p"),
            ("The Movie HDR", "hdr"),
            ("The Movie DV", "dv"),
            ("The Movie DoVi", "dv"),
            ("The Movie REMUX", "remux"),
            ("The Movie BluRay", "bluray"),
            ("The Movie WEB-DL", "web-dl"),
            ("The Movie x265", "x265"),
            ("The Movie HEVC", "x265"),
        ];
        for (name, want) in cases {
            assert_eq!(
                quality_tag(name),
                Some(want),
                "title `{name}` should read as {want}"
            );
        }
    }

    #[test]
    fn quality_tag_priority_first_match_wins() {
        let cases = [
            ("The Movie 2160p REMUX", "4k"),
            ("The Movie 1080p HDR DV", "1080p"),
            ("The Movie 720p WEB-DL", "720p"),
            ("The Movie DV 4K", "4k"),
            ("The Movie x265 4K", "4k"),
            ("The Movie 1080p x265", "1080p"),
        ];
        for (name, want) in cases {
            assert_eq!(
                quality_tag(name),
                Some(want),
                "`{name}` should read as {want}, not a lower-priority tag"
            );
        }
    }

    #[test]
    fn quality_tag_ignores_digit_surrounded_false_positives() {
        // A year must never read as a resolution, and "720" inside "1720"
        // is a longer number, not a tag.
        assert_eq!(quality_tag("The Movie 2021 1080p"), Some("1080p"));
        assert_eq!(quality_tag("The Movie 2021"), None);
        assert_eq!(quality_tag("The Movie 1720"), None);
        assert_eq!(quality_tag("The Movie 2024"), None);
        // The digit rule cuts both ways: a digit right after the token also
        // disqualifies it (HDR10 is a branding, not our "hdr" tag).
        assert_eq!(quality_tag("The Movie hdr10"), None);
        assert_eq!(quality_tag("The Movie 1080"), Some("1080p"));
        assert_eq!(quality_tag("The Movie"), None);
    }

    #[test]
    fn fmt_size_renders_binary_units_with_one_decimal() {
        // FR-23: B/KiB/MiB/GiB/TiB, one decimal from 1 KiB up; 0 is a dash
        // (sources that don't report a size), never a fake "0 B".
        assert_eq!(fmt_size(0), "—");
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1024), "1.0 KiB");
        assert_eq!(fmt_size(1536), "1.5 KiB");
        assert_eq!(fmt_size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(fmt_size(987 * 1024 * 1024), "987.0 MiB");
        assert_eq!(fmt_size(48 * 1024 * 1024 * 1024), "48.0 GiB");
        assert_eq!(fmt_size(2 * 1024 * 1024 * 1024 * 1024), "2.0 TiB");
    }

    /// The fg color of the first span whose text equals `needle`.
    fn span_color(line: &Line, needle: &str) -> ratatui::style::Color {
        line.spans
            .iter()
            .find(|s| s.content.trim() == needle)
            .expect("span present")
            .style
            .fg
            .expect("span has a color")
    }

    #[test]
    fn seeder_and_leecher_tiers_color_by_swarm_size() {
        // FR-24: >=100 success, 1-99 warning, 0 dim — for both columns.
        let theme = Theme::titanium();
        let state = SearchState::default();
        let success = theme.colors.success().to_ratatui();
        let warning = theme.colors.warning().to_ratatui();
        let dim = theme.colors.dim().to_ratatui();

        let healthy = TorrentResult {
            seeders: 500,
            leechers: 300,
            ..result("Dune")
        };
        let line = result_line(&healthy, false, false, &state, &theme, 60);
        assert_eq!(
            span_color(&line, "500"),
            success,
            "seeders >=100 is success"
        );
        assert_eq!(
            span_color(&line, "300"),
            success,
            "leechers >=100 is success"
        );

        let thin = TorrentResult {
            seeders: 50,
            leechers: 7,
            ..result("Dune")
        };
        let line = result_line(&thin, false, false, &state, &theme, 60);
        assert_eq!(span_color(&line, "50"), warning, "seeders 1-99 is warning");
        assert_eq!(span_color(&line, "7"), warning, "leechers 1-99 is warning");

        let dead = TorrentResult {
            seeders: 0,
            leechers: 0,
            ..result("Dune")
        };
        let line = result_line(&dead, false, false, &state, &theme, 60);
        // YTS reports swarm health, so a real 0 is the digit 0, dim.
        assert_eq!(span_color(&line, "0"), dim, "reported zero seeders is dim");
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn unreported_health_is_an_em_dash_never_a_guessed_band() {
        // FR-24: FitGirl/SubsPlease carry no swarm counts. seeders: 0 means
        // unknown — an em dash — never a made-up 50+/10+ band.
        let theme = Theme::titanium();
        let state = SearchState::default();
        for source in [SourceId::FitGirl, SourceId::SubsPlease] {
            let row = TorrentResult {
                seeders: 0,
                leechers: 0,
                source,
                ..result("Cyberpunk 2077")
            };
            let text = line_text(&result_line(&row, false, false, &state, &theme, 80));
            assert!(
                !text.contains("50+") && !text.contains("10+"),
                "{source:?} must not invent a swarm band: {text}"
            );
            assert!(
                text.contains('—'),
                "{source:?} unknown health is an em dash: {text}"
            );
            assert_eq!(
                span_color(&result_line(&row, false, false, &state, &theme, 80), "—"),
                theme.colors.dim().to_ratatui(),
                "{source:?} unknown health is dim"
            );
        }
    }

    #[test]
    fn reported_zero_swarm_renders_zero() {
        let theme = Theme::titanium();
        let state = SearchState::default();
        for source in [SourceId::Yts, SourceId::Eztv] {
            let row = TorrentResult {
                seeders: 0,
                leechers: 0,
                source,
                ..result("Dune")
            };
            let text = line_text(&result_line(&row, false, false, &state, &theme, 80));
            assert!(
                text.contains('0'),
                "{source:?} reports health, so 0 is 0: {text}"
            );
            assert!(
                !text.contains("10+") && !text.contains("50+") && !text.contains('—'),
                "{source:?} must not guess a band or dash a real 0: {text}"
            );
        }
    }

    #[test]
    fn six_digit_seed_counts_are_not_clipped() {
        let theme = Theme::titanium();
        let state = SearchState::default();
        let row = TorrentResult {
            seeders: 999_999,
            leechers: 1,
            ..result("Dune")
        };
        let text = line_text(&result_line(&row, false, false, &state, &theme, 80));
        assert!(
            text.contains("999999"),
            "six visible seed digits must fit: {text}"
        );
        assert!(!text.contains('…'), "999999 must not clip: {text}");
    }

    #[test]
    fn result_line_puts_quality_chip_before_source_chip() {
        let mut state = SearchState::default();
        state.source_counts.insert(SourceId::Yts, 3);
        let theme = Theme::titanium();
        let line = result_line(
            &result("Dune 2021 1080p BLURAY"),
            false,
            false,
            &state,
            &theme,
            60,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let q = text.find("[1080p]").expect("quality chip rendered");
        let c = text.find("[yts]").expect("source chip rendered");
        assert!(q < c, "quality chip precedes the source chip: {text}");
    }

    #[test]
    fn result_line_without_quality_tag_has_no_second_chip() {
        let mut state = SearchState::default();
        state.source_counts.insert(SourceId::Yts, 3);
        let theme = Theme::titanium();
        let line = result_line(&result("Dune"), false, false, &state, &theme, 60);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("[1080p]"), "no quality chip: {text}");
        assert!(text.contains("[yts]"), "source chip still rendered: {text}");
    }

    #[test]
    fn header_renders_column_labels_and_count() {
        let state = SearchState {
            query: "dune".into(),
            results: vec![result("Dune: Part Two")],
            ..SearchState::default()
        };
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw_header(f, f.area(), &state, &theme, None))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let text: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(text.contains("name"), "name column labeled: {text}");
        assert!(text.contains("size"), "size column labeled: {text}");
        assert!(text.contains("seed"), "seed column labeled: {text}");
        assert!(text.contains("leech"), "leech column labeled: {text}");
        assert!(text.contains("quality"), "quality column labeled: {text}");
        assert!(text.contains("source"), "source column labeled: {text}");
        assert!(
            text.contains("1 results from 1 sources"),
            "count line kept: {text}"
        );
    }

    #[test]
    fn header_and_result_line_columns_align_perfectly() {
        let state = SearchState {
            query: "dune".into(),
            results: vec![result("Dune: Part Two 1080p")],
            ..SearchState::default()
        };
        let theme = Theme::titanium();
        let width = 80;
        let backend = TestBackend::new(width as u16, 2);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| {
                draw_header(f, Rect::new(0, 0, width as u16, 1), &state, &theme, None);
                let line = result_line(&state.results[0], false, false, &state, &theme, width);
                f.render_widget(Paragraph::new(line), Rect::new(0, 1, width as u16, 1));
            })
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let header: String = (0..width)
            .map(|x| buf[(x as u16, 0)].symbol().to_string())
            .collect();
        let row: String = (0..width)
            .map(|x| buf[(x as u16, 1)].symbol().to_string())
            .collect();

        // Suffix starts at column index 42 (80 - 38)
        let size_hdr_idx = header.find("size").expect("size in header");
        let size_row_idx = row.find("5.0 GiB").expect("size in row");
        assert_eq!(size_hdr_idx, size_row_idx, "size column alignment");

        let seed_hdr_idx = header.find("seed").expect("seed in header");
        let seed_row_idx = row.find("120").expect("seed in row");
        assert_eq!(
            seed_hdr_idx - 1,
            seed_row_idx - 2,
            "seed column start alignment"
        );

        let leech_hdr_idx = header.find("leech").expect("leech in header");
        let leech_row_idx = row.find("   5 ").expect("leech in row");
        assert_eq!(leech_hdr_idx, leech_row_idx, "leech column alignment");

        let qual_hdr_idx = header.find("quality").expect("quality in header");
        let qual_row_idx = row.find("[1080p]").expect("quality in row");
        assert_eq!(qual_hdr_idx, qual_row_idx, "quality column alignment");

        let src_hdr_idx = header.rfind("source").expect("source in header");
        let src_row_idx = row.find("[yts]").expect("source in row");
        assert_eq!(src_hdr_idx, src_row_idx, "source column alignment");
    }

    fn two_gib() -> TorrentResult {
        TorrentResult {
            size_bytes: 2_147_483_648,
            ..result("Dune: Part Two")
        }
    }

    /// Paint one result row into a TestBackend and return the symbol line.
    fn paint_result_row(hit: &TorrentResult, width: u16) -> String {
        let theme = Theme::titanium();
        let state = SearchState::default();
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| {
                let line = result_line(hit, false, false, &state, &theme, width as usize);
                f.render_widget(Paragraph::new(line), f.area());
            })
            .expect("draw");
        let buf = terminal.backend().buffer();
        (0..width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    #[test]
    fn result_row_shows_human_size_at_width_100_and_60() {
        // #69 / FR-23: a known 2 GiB hit must paint `2.0 GiB`, not a blank.
        let hit = two_gib();
        for width in [100_u16, 60] {
            let row = paint_result_row(&hit, width);
            assert!(
                row.contains("2.0 GiB"),
                "FR-23 size at width {width}, got: {row:?}"
            );
        }
    }

    #[test]
    fn result_size_uses_text_color_not_muted() {
        // Muted on selectedBg is ~1.8:1 — the size column reads as empty (#69).
        let theme = Theme::titanium();
        let state = SearchState::default();
        let line = result_line(&two_gib(), false, false, &state, &theme, 80);
        let fg = span_color(&line, "2.0 GiB");
        assert_eq!(fg, theme.colors.text().to_ratatui(), "size uses text()");
        assert_ne!(fg, theme.colors.muted().to_ratatui(), "size is not muted()");
    }

    fn source_status_text(health: SourceStatus, count: usize) -> String {
        let mut state = SearchState::default();
        state.source_health.insert(SourceId::Yts, health);
        if count > 0 {
            state.source_counts.insert(SourceId::Yts, count);
        }
        let line = searching_source_line(
            SourceId::Yts,
            "YTS",
            &SourceGroup::Movies,
            &state,
            &Theme::titanium(),
            "⠋",
        );
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn checking_source_is_not_connecting() {
        let text = source_status_text(SourceStatus::Checking, 0);
        assert!(
            text.contains("checking source…"),
            "Checking is source health, got: {text}"
        );
        assert!(
            !text.contains("connecting"),
            "connecting belongs to BitTorrent, got: {text}"
        );
    }

    #[test]
    fn online_source_reports_result_count() {
        let text = source_status_text(SourceStatus::Online, 3);
        assert!(
            text.contains("3 results"),
            "answered sources show a count, got: {text}"
        );
    }
}
