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
use ratatui::widgets::{Block, Borders, Paragraph};

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
const HINT: &str = "type to search · enter run/browse · tab downloads · ctrl+c quit";
/// The results pane owns the keyboard: plain keys act on the selected row.
const RESULTS_HINT: &str =
    "enter watch now · d download · s settings · ? help · type to refine · esc input";
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
    draw_header(frame, rows[1], state, theme);
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

/// Results header: the count line on the left plus dim column labels
/// ("name", "size", "s", "l", "source") right-aligned over the suffix
/// block `result_line` draws — a new user can read a row without guessing
/// which number is which. One row, not two: `input.rs`'s mouse mapping
/// derives the results top from `SEARCH_BAR_H + 1`.
fn draw_header(frame: &mut Frame, area: Rect, state: &SearchState, theme: &Theme) {
    let colors = &theme.colors;
    let latency_str = state
        .latency_ms
        .map_or(String::new(), |ms| format!(" in {:.1}s", ms as f64 / 1000.0));
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
    // Same cells and single-space gaps as the row suffix, so the labels sit
    // over their columns; the block's right edge is the row's right edge.
    let suffix = ["size", "s", "l", "source"].join(" ");
    let suffix_w = suffix.chars().count();
    let width = area.width as usize;
    // "name" labels the left column; the count text follows it, truncated
    // before it can collide with the right-aligned labels.
    let count_w = width.saturating_sub(suffix_w + 1 + 5); // 5: "name" + gap
    let count = truncate(&count, count_w);
    let pad = width.saturating_sub(5 + count.chars().count() + suffix_w);
    let spans = vec![
        Span::styled("name", Style::default().fg(colors.dim().to_ratatui())),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" ".repeat(pad)),
        Span::styled(suffix, Style::default().fg(colors.dim().to_ratatui())),
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

/// Result list: one row per result, scrolled so the selection stays visible.
/// The empty state names the next action instead of sitting blank.
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

            for (group_name, sources) in SIDEBAR {
                for (id, label) in *sources {
                    if disabled.contains(id) {
                        continue;
                    }
                    let health = state
                        .source_health
                        .get(id)
                        .copied()
                        .unwrap_or(SourceStatus::Unknown);
                    let count = state.source_counts.get(id).copied().unwrap_or(0);
                    let (dot, status_str, style) = match health {
                        SourceStatus::Online => {
                            let text = if count > 0 {
                                format!("{count} results found")
                            } else {
                                "ready".to_string()
                            };
                            ("●", text, Style::default().fg(theme.colors.success().to_ratatui()))
                        }
                        SourceStatus::Checking => (
                            spinner,
                            "connecting…".to_string(),
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
                    lines.push(Line::from(vec![
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
                    ]));
                }
            }
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }
        let msg = "no results yet — press Enter to search";
        frame.render_widget(Paragraph::new(empty_line(msg, theme)), area);
        return;
    }
    let width = area.width as usize;
    let vis = area.height as usize;
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
    let seeds = if result.seeders > 0 {
        result.seeders.to_string()
    } else if result.source == SourceId::FitGirl || result.source == SourceId::SubsPlease {
        "50+".into()
    } else if result.source == SourceId::Eztv {
        "10+".into()
    } else {
        "—".into()
    };
    let leeches = if result.leechers > 0 {
        result.leechers.to_string()
    } else if result.source == SourceId::FitGirl || result.source == SourceId::SubsPlease {
        "10+".into()
    } else {
        "—".into()
    };
    let quality_w = quality.as_ref().map_or(0, |q| q.chars().count());
    let suffix_w = size.chars().count()
        + seeds.chars().count()
        + leeches.chars().count()
        + quality_w
        + chip.chars().count()
        + 3
        + usize::from(quality.is_some());
    let name_width = width.saturating_sub(suffix_w + 1);
    let name = marquee_text(&result.name, name_width, selected || hovered, clock());
    let pad = width.saturating_sub(name.chars().count() + suffix_w);
    let name_fg = if selected || hovered {
        colors.accent()
    } else {
        colors.text()
    };
    let seed_fg = if result.seeders > 0 {
        health_color(result.seeders, colors)
    } else if result.source == SourceId::FitGirl
        || result.source == SourceId::SubsPlease
        || result.source == SourceId::Eztv
    {
        colors.success()
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
    let mut spans = vec![
        Span::styled(name, Style::default().fg(name_fg.to_ratatui())),
        Span::raw(" ".repeat(pad)),
        Span::styled(size, Style::default().fg(colors.muted().to_ratatui())),
        Span::raw(" "),
        Span::styled(seeds, Style::default().fg(seed_fg.to_ratatui())),
        Span::raw(" "),
        Span::styled(leeches, Style::default().fg(leech_fg.to_ratatui())),
        Span::raw(" "),
    ];
    if let Some(q) = quality {
        spans.push(Span::styled(q, Style::default().fg(chip_fg.to_ratatui())));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        chip,
        Style::default().fg(chip_fg.to_ratatui()),
    ));
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
        SourceId::SubsPlease => "subsplease",
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
            .find(|s| s.content.as_ref() == needle)
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
        // Both columns render the '—' dash (unreported health) in dim.
        assert_eq!(span_color(&line, "—"), dim, "zero seeders is dim");
        assert_eq!(span_color(&line, "—"), dim, "zero leechers is dim");
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
        let backend = TestBackend::new(56, 1);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw_header(f, f.area(), &state, &theme))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let text: String = (0..56).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(text.contains("name"), "name column labeled: {text}");
        assert!(text.contains("size"), "size column labeled: {text}");
        assert!(text.contains("source"), "source column labeled: {text}");
        assert!(
            text.contains("1 results from 1 sources"),
            "count line kept: {text}"
        );
    }
}
