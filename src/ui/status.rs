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
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::core::types::{ItemView, QueueStatus};
use crate::theme::Theme;
use crate::ui::{AppState, FolderPrompt, FolderPromptMode, Screen};

/// The minimum terminal size harbour promises (UR-12): below this the views
/// cannot lay out, so the status bar says so instead of pretending.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

/// True when the terminal is smaller than the 80x24 minimum (UR-12). Pure so
/// the resize-hint test needs no terminal.
pub fn needs_resize_hint(width: u16, height: u16) -> bool {
    width < MIN_WIDTH || height < MIN_HEIGHT
}

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

/// Tab buttons shown in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTab {
    Search,
    Downloads,
    Settings,
    Help,
}

pub struct StatusButtonDef {
    pub tab: StatusTab,
    pub text: &'static str,
    pub width: u16,
}

pub const STATUS_BUTTONS: &[StatusButtonDef] = &[
    StatusButtonDef {
        tab: StatusTab::Search,
        text: "[ 🔍 Search ]",
        width: 13,
    },
    StatusButtonDef {
        tab: StatusTab::Downloads,
        text: "[ ⬇ Downloads ]",
        width: 15,
    },
    StatusButtonDef {
        tab: StatusTab::Settings,
        text: "[ ⚙ Settings ]",
        width: 14,
    },
    StatusButtonDef {
        tab: StatusTab::Help,
        text: "[ ? Help ]",
        width: 10,
    },
];

pub const BUTTON_GAP: u16 = 1;
pub const TOTAL_BUTTONS_WIDTH: u16 = 13 + 1 + 15 + 1 + 14 + 1 + 10; // 55

/// Column ranges `(start_x, end_x)` for status buttons at `total_width`.
pub fn status_button_ranges(total_width: u16) -> Vec<(StatusTab, &'static str, u16, u16)> {
    if total_width < TOTAL_BUTTONS_WIDTH + 4 {
        return Vec::new();
    }
    let start_x = total_width.saturating_sub(TOTAL_BUTTONS_WIDTH + 2);
    let mut current_x = start_x;
    let mut ranges = Vec::with_capacity(STATUS_BUTTONS.len());
    for btn in STATUS_BUTTONS {
        let end_x = current_x + btn.width;
        ranges.push((btn.tab, btn.text, current_x, end_x));
        current_x = end_x + BUTTON_GAP;
    }
    ranges
}

/// Hit-tests a column in the status bar against the tab buttons.
pub fn status_button_at(col: u16, total_width: u16) -> Option<StatusTab> {
    for (tab, _, start_x, end_x) in status_button_ranges(total_width) {
        if col >= start_x && col < end_x {
            return Some(tab);
        }
    }
    None
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
    // UR-12: below 80x24 the views cannot lay out, so the status bar swaps
    // its context for a resize hint instead of showing a broken screen.
    let too_small = needs_resize_hint(frame.area().width, frame.area().height);
    if too_small {
        let (label, raw_context) = (
            "resize",
            "terminal too small — need at least 80x24".to_string(),
        );
        let sep = format!(" {} ", theme.symbols.border_v);
        let line = Line::from(vec![
            Span::styled(label, Style::default().fg(colors.accent().to_ratatui())),
            Span::styled(sep, Style::default().fg(colors.border().to_ratatui())),
            Span::styled(
                raw_context,
                Style::default().fg(colors.warning().to_ratatui()),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(status_bg)),
            status_area,
        );
    } else {
        let (label, raw_context) = segments(screen, state);
        let sep = format!(" {} ", theme.symbols.border_v);
        let spinner_w = spinner_glyph.chars().count();
        let avail = status_area.width as usize;

        let ranges = status_button_ranges(status_area.width);
        let buttons_start = if ranges.is_empty() {
            avail.saturating_sub(spinner_w + 1)
        } else {
            ranges[0].2 as usize
        };

        let left_prefix_w = label.chars().count() + sep.chars().count();
        let max_context_w = buttons_start.saturating_sub(left_prefix_w + 1);
        let context = truncate_to(&raw_context, max_context_w);
        let left_used = left_prefix_w + context.chars().count();
        let fill_left = buttons_start.saturating_sub(left_used);

        let mut spans = vec![
            Span::styled(label, Style::default().fg(colors.accent().to_ratatui())),
            Span::styled(sep, Style::default().fg(colors.border().to_ratatui())),
            Span::styled(context, Style::default().fg(colors.muted().to_ratatui())),
            Span::raw(" ".repeat(fill_left)),
        ];

        for (i, (tab, text, start_x, end_x)) in ranges.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" ".repeat(BUTTON_GAP as usize)));
            }
            let is_active = match tab {
                StatusTab::Search => screen == Screen::Search,
                StatusTab::Downloads => screen == Screen::Downloads,
                StatusTab::Settings => screen == Screen::Settings,
                StatusTab::Help => screen == Screen::Help,
            };
            let is_hovered = state
                .mouse_pos
                .is_some_and(|(mx, my)| my == status_area.y && mx >= start_x && mx < end_x);

            let style = if is_active {
                Style::default()
                    .fg(colors.accent().to_ratatui())
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else if is_hovered {
                Style::default()
                    .fg(colors.text().to_ratatui())
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(colors.muted().to_ratatui())
            };

            spans.push(Span::styled(text, style));
        }

        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            spinner_glyph.to_string(),
            Style::default().fg(colors.accent().to_ratatui()),
        ));

        let line = Line::from(spans);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(status_bg)),
            status_area,
        );
    }

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

        let dismiss_w = 13u16;
        let is_dismiss_hovered = state.mouse_pos.is_some_and(|(mx, my)| {
            my == banner_area.y
                && mx >= banner_area.right().saturating_sub(dismiss_w + 2)
                && mx < banner_area.right()
        });
        let is_banner_hovered = state.mouse_pos.is_some_and(|(mx, my)| {
            mx >= banner_area.x
                && mx < banner_area.right()
                && my >= banner_area.y
                && my < banner_area.bottom()
        });
        let dismiss_fg = if is_dismiss_hovered {
            colors.error().to_ratatui()
        } else if is_banner_hovered {
            colors.text().to_ratatui()
        } else {
            colors.accent().to_ratatui()
        };
        let dismiss_style = if is_dismiss_hovered {
            Style::default()
                .fg(dismiss_fg)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(dismiss_fg)
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors.error().to_ratatui()))
            .title(Span::styled(
                " error ",
                Style::default().fg(colors.error().to_ratatui()),
            ))
            .title(
                Line::from(Span::styled(" [✕ dismiss] ", dismiss_style))
                    .alignment(ratatui::layout::Alignment::Right),
            )
            .style(Style::default().bg(colors.selected_bg().to_ratatui()));
        frame.render_widget(
            Paragraph::new(content)
                .block(block)
                .style(Style::default().bg(colors.selected_bg().to_ratatui())),
            banner_area,
        );
    }

    // --- folder prompt (FR-29/40) ---------------------------------------
    // A modal inline text input (shift+D / o), painted over the whole frame
    // so it reads as an overlay wherever the user opened it. It cannot
    // coexist with help/picker/settings — each overlay owns every key while
    // it is up, so none of them can open under the prompt.
    if state.folder_prompt.open {
        draw_folder_prompt(frame, &state.folder_prompt, theme);
    }
}

/// Block cursor glyph at the end of the typed path — the input's focus
/// marker, the same one the settings inline edit uses.
const FOLDER_CURSOR: &str = "▌";

/// Draws the folder-prompt modal (FR-29/40) centred over the frame, in the
/// same panel style as the help/settings overlays: a title naming what Enter
/// commits, one line of "path: <buffer>▌", and a hint row.
fn draw_folder_prompt(frame: &mut Frame, prompt: &FolderPrompt, theme: &Theme) {
    let area = frame.area();
    let colors = &theme.colors;
    let title = match prompt.mode {
        FolderPromptMode::DownloadTo => " download to folder ",
        FolderPromptMode::SetDefault => " default download folder ",
    };

    let width = 62.min(area.width.saturating_sub(4).max(30));
    let height = 7.min(area.height);
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height,
    };

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

    // The path is truncated to the panel with the cursor glued to the end, so
    // a long path reads as clipped, never chopped mid-glyph.
    let prefix = "path: ";
    let value_w = (panel.width as usize).saturating_sub(prefix.chars().count() + 4);
    let mut value: String = prompt.edit_buffer.chars().take(value_w).collect();
    value.push_str(FOLDER_CURSOR);
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                prefix.to_string(),
                Style::default().fg(colors.muted().to_ratatui()),
            ),
            Span::styled(value, Style::default().fg(colors.accent().to_ratatui())),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter confirm · esc cancel".to_string(),
            Style::default().fg(colors.muted().to_ratatui()),
        )),
    ];

    // Clear first: without it the screen underneath shows through the panel.
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_set(border)
                    .border_style(Style::default().fg(colors.border().to_ratatui()))
                    .title(Span::styled(
                        title.to_string(),
                        Style::default().fg(colors.accent().to_ratatui()),
                    ))
                    .style(Style::default().bg(colors.bg().to_ratatui())),
            )
            .style(Style::default().bg(colors.bg().to_ratatui())),
        panel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_hint_triggers_below_the_minimum() {
        // UR-12: the app promises an 80x24 minimum; anything smaller shows
        // the resize hint, the minimum itself is fine.
        assert!(!needs_resize_hint(80, 24));
        assert!(!needs_resize_hint(120, 40));
        assert!(needs_resize_hint(79, 24), "too narrow");
        assert!(needs_resize_hint(80, 23), "too short");
        assert!(needs_resize_hint(0, 0));
    }

    #[test]
    fn a_tiny_terminal_renders_the_resize_hint_in_the_status_bar() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let state = AppState::default();
        let theme = Theme::titanium();
        let backend = TestBackend::new(60, 20); // below the 80x24 minimum
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), Screen::Search, &state, &theme, "⠋"))
            .expect("draw must succeed");
        let symbols: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            symbols.contains("80x24"),
            "the hint must name the minimum, got: {symbols}"
        );
    }

    #[test]
    fn a_full_size_terminal_keeps_the_normal_status_context() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::default();
        state.search.query = "dune".into();
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), Screen::Search, &state, &theme, "⠋"))
            .expect("draw must succeed");
        let symbols: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(symbols.contains("dune"), "query context shown");
        assert!(!symbols.contains("80x24"), "no resize hint at the minimum");
    }

    #[test]
    fn the_folder_prompt_renders_its_title_and_buffer() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::default();
        state.folder_prompt.open = true;
        state.folder_prompt.mode = FolderPromptMode::DownloadTo;
        state.folder_prompt.edit_buffer = "D:\\media".into();
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), Screen::Search, &state, &theme, "⠋"))
            .expect("draw must succeed");
        let symbols: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            symbols.contains("download to folder"),
            "the panel title names the mode"
        );
        assert!(symbols.contains("D:\\media"), "the seeded buffer is shown");
    }

    #[test]
    fn the_folder_prompt_set_default_mode_names_it() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::default();
        state.folder_prompt.open = true;
        state.folder_prompt.mode = FolderPromptMode::SetDefault;
        state.folder_prompt.edit_buffer = "C:\\dl".into();
        let theme = Theme::titanium();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), Screen::Search, &state, &theme, "⠋"))
            .expect("draw must succeed");
        let symbols: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            symbols.contains("default download folder"),
            "set-default mode names the commit target"
        );
    }
}
