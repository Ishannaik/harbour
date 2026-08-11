//! Application shell: terminal lifecycle, the 30fps event/draw loop, the
//! boot splash, keybind dispatch, and the fake engine/source streaming
//! (roadmap Phase 2).
//!
//! Views (src/ui/*) are pure paint; this module owns the parts that change —
//! entering/leaving the terminal safely on every exit path, turning keys into
//! actions, delivering fake source answers as their simulated latencies
//! expire, simulating queue progress, and easing every displayed value so
//! bars never jump (design.md §3).
//!
//! The splash is deliberately over the top (omp-grade energy): a block-letter
//! HARBOUR logo that converges with a CRT-style flicker, a shimmer band that
//! sweeps across it, twinkling particles, a scrolling harbor wave, a breathing
//! border, and staggered tagline/status fades — everything still live at 30fps
//! until the user advances. All color comes from the theme's curated subset
//! (accent/text/success/muted/border/bg), so custom themes keep working.

use std::io;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

use crate::anim::{self, EasedValue, Spinner, Ticker};
use crate::fake::{FakeEngine, SOURCES, SearchPlan};
use crate::theme::{Color, Theme, lerp_color};
use crate::types::{AppState, HistoryItem, QueueItem, QueueStatus, Screen, TorrentResult};
use crate::ui::{self, DisplayState, FrameVars};

/// Base render cadence (docs/design.md §Animation): the loop redraws at most
/// once per tick; a burst of input within one tick coalesces into one frame.
const FPS: u32 = 30;

/// Logo convergence window: rows flicker in over this span.
const DRAW_IN: Duration = Duration::from_millis(700);

/// After this much time the splash status line flips to "ready".
const READY_AFTER: Duration = Duration::from_millis(1600);

/// Status spinner cadence (docs/design.md §Animation): one frame per 80ms.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Easing time constant for every displayed value (design.md §3, tau=200ms).
const EASE_TAU: Duration = Duration::from_millis(200);

/// Shimmer band sweep period (a bright highlight crosses the logo once per
/// period).
const SHIMMER_PERIOD: Duration = Duration::from_millis(2200);

/// Harbor wave scroll period — the water line under the logo.
const WAVE_PERIOD: Duration = Duration::from_millis(1300);

/// Border breathing period (border → accent → border).
const BREATH_PERIOD: Duration = Duration::from_millis(3000);

/// Tagline/version fade-in windows, staggered after the logo converges.
const TAGLINE_FADE_AT: Duration = Duration::from_millis(950);
const TAGLINE_FADE_DUR: Duration = Duration::from_millis(350);
const VERSION_FADE_AT: Duration = Duration::from_millis(1300);
const VERSION_FADE_DUR: Duration = Duration::from_millis(350);

/// White-hot highlight used by the shimmer band and the ready-flash — the one
/// literal color in the splash (a highlight, not a theme choice).
const HOT: Color = Color::Rgb(255, 255, 255);

/// Fake concurrency cap (HARBOUR_MAX_DOWNLOADS=2) until the engine lands.
const MAX_DOWNLOADS: usize = 2;

/// Fake wall clock base: history timestamps are `BASE_EPOCH_MS + runtime`,
/// so they are deterministic for tests and show a plausible clock in demos.
const BASE_EPOCH_MS: i64 = 1_780_000_000_000;

/// Block-letter HARBOUR logo, 5 rows x 41 columns. Hand-drawn so each letter
/// is exactly 5 cells wide + 1 separator: crisp monospace output, no emoji
/// width surprises. Rows converge top-down with a CRT-style flicker.
const LOGO_ART: &[&str] = &[
    "H   H  AAA   RRRR   BBBB    OOO   U   U  RRRR ",
    "H   H A   A  R   R  B   B  O   O  U   U  R   R",
    "HHHHH AAAAA  RRRR   BBBB   O   O  U   U  RRRR ",
    "H   H A   A  R  R   B   B  O   O  U   U  R  R ",
    "H   H A   A  R   R  BBBB    OOO    UUU   R   R",
];

/// Number of particles twinkling around the logo (positioned by seeded PRNG).
const PARTICLE_COUNT: usize = 14;

/// A tiny xorshift64* PRNG for the splash. Seeded at construction so every
/// run shows the same particle field and glitch pattern — deterministic,
/// testable. (The fake engine has its own; this one is splash-only.)
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Restores the terminal on drop: show cursor, leave alternate screen, then
/// disable raw mode — in that order, best-effort. Drop cannot return errors,
/// and a partial restore still beats leaving the user's shell unusable.
struct TerminalGuard;

impl TerminalGuard {
    /// Enters the TUI: raw mode first, then the alternate screen with the
    /// cursor hidden. If the alternate-screen entry fails, raw mode is
    /// disabled before the error propagates so a half-set-up terminal is
    /// never leaked to the caller.
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Cursor/alt-screen must be restored while still in raw mode; raw
        // mode is lifted last so no escape codes leak to the shell.
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// A particle's fixed position (as area fractions) and twinkle phase.
struct Particle {
    fx: f64,
    fy: f64,
    phase: f64,
}

/// Boot splash state. Kept in a struct so the app can swap in the search
/// screen without touching the loop.
struct SplashState {
    start: Instant,
    spinner: Spinner,
    rng: Rng,
    particles: Vec<Particle>,
}

impl SplashState {
    fn new(theme: &Theme) -> Self {
        // Fixed seed (0x68617262 == "harb") so the particle field and the
        // convergence glitch are identical across runs — the smoke test can
        // depend on the shape without freezing a screenshot.
        let mut rng = Rng(0x6861_7262_0000_0000);
        let particles = (0..PARTICLE_COUNT)
            .map(|_| Particle {
                fx: 0.08 + rng.next_f64() * 0.84,
                fy: 0.08 + rng.next_f64() * 0.84,
                phase: rng.next_f64() * std::f64::consts::TAU,
            })
            .collect();
        Self {
            start: Instant::now(),
            spinner: Spinner::new(theme.symbols.spinner_frames.clone()),
            rng,
            particles,
        }
    }
}

/// Appends `text` to `out` as one styled span, merging with the previous span
/// when the color matches — per-char styling would otherwise emit hundreds of
/// spans per frame; run-length merging keeps the frame buffer small.
fn push_run(out: &mut Vec<Span<'static>>, text: &str, color: Color) {
    let fg = color.to_ratatui();
    if let Some(last) = out.last_mut().filter(|l| l.style.fg == Some(fg)) {
        last.content.to_mut().push_str(text);
        return;
    }
    out.push(Span::styled(text.to_string(), Style::default().fg(fg)));
}

/// The shimmer band: for a given column, how much white-hot highlight to add
/// (0.0..=1.0), peaking at the band center and falling off on both sides.
fn shimmer_intensity(column: usize, width: usize, elapsed: Duration) -> f64 {
    let cycle = elapsed.as_secs_f64() / SHIMMER_PERIOD.as_secs_f64();
    let center = (cycle % 1.0) * width as f64;
    let d = (column as f64 - center).abs();
    // Gaussian falloff; the band is ~4 columns wide at half height.
    (-(d * d) / (2.0 * 1.7 * 1.7)).exp()
}

/// Renders one logo row: gradient accent → text across the columns, the
/// shimmer band overlaid on top, and (during convergence) a seeded CRT
/// flicker that drops random rows to dim for a single frame.
fn logo_row(
    row: &str,
    row_index: usize,
    converging: bool,
    elapsed: Duration,
    splash: &mut SplashState,
    colors: &crate::theme::ThemeColors,
) -> Line<'static> {
    let width = row.chars().count();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run: String = String::new();
    let mut run_color = Color::Default;

    let flicker = converging && splash.rng.next_f64() < 0.12;

    for (col, ch) in row.chars().enumerate() {
        if ch == ' ' {
            continue;
        }
        let base = lerp_color(
            colors.accent(),
            colors.text(),
            col as f64 / width.max(1) as f64,
        );
        let color = if flicker {
            colors.dim() // CRT flicker: the row dims for one frame
        } else {
            let band = shimmer_intensity(col, width, elapsed);
            if band > 0.02 {
                lerp_color(base, HOT, 0.8 * band)
            } else {
                base
            }
        };
        if color != run_color {
            if !run.is_empty() {
                push_run(&mut spans, &run, run_color);
            }
            run.clear();
            run_color = color;
        }
        run.push(ch);
    }
    if !run.is_empty() {
        push_run(&mut spans, &run, run_color);
    }
    let _ = row_index; // reserved for per-row effects (e.g. ripple delay)
    Line::from(spans)
}

/// The scrolling harbor wave under the logo: glyph phase advances with time
/// and column, the leading crest rendered in success. Decorative line — a
/// minor glyph-width drift here is cosmetic and acceptable.
fn wave_line(elapsed: Duration, colors: &crate::theme::ThemeColors, width: usize) -> Line<'static> {
    const GLYPHS: &[char] = &['~', '≈', '⌇', '≈', '~'];
    let phase = elapsed.as_secs_f64() / WAVE_PERIOD.as_secs_f64() * std::f64::consts::TAU;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for col in 0..width {
        let g = GLYPHS[((col as f64 * 0.9 + phase * 2.0).round() as usize) % GLYPHS.len()];
        let wave = ((col as f64 / width.max(1) as f64) * std::f64::consts::TAU + phase).sin();
        let color = if wave > 0.65 {
            colors.success() // crest
        } else if wave < -0.65 {
            colors.text()
        } else {
            colors.accent()
        };
        push_run(&mut spans, &g.to_string(), color);
    }
    Line::from(spans)
}

/// One frame of the splash: breathing border, converging logo with shimmer,
/// twinkling particles, the wave, staggered tagline/version fades, and the
/// spinner status line (with a success flash on ready).
fn draw_splash(frame: &mut Frame, theme: &Theme, splash: &mut SplashState, now: Instant) {
    let elapsed = now.duration_since(splash.start);
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();

    // Fill the whole screen first so the splash sits on the theme's bg
    // instead of the terminal default (a Block styles its entire area).
    frame.render_widget(
        Block::default().style(Style::default().bg(bg)),
        frame.area(),
    );

    let converging = elapsed < DRAW_IN;
    let draw_progress = (elapsed.as_secs_f64() / DRAW_IN.as_secs_f64()).clamp(0.0, 1.0);
    let visible_rows = (draw_progress * LOGO_ART.len() as f64).round() as usize;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::default()); // top padding

    // Logo, row by row, top-down convergence with per-row flicker.
    for (i, row) in LOGO_ART.iter().enumerate() {
        if i < visible_rows {
            lines.push(logo_row(row, i, converging, elapsed, splash, colors));
        } else {
            // Blank placeholder keeps panel geometry stable while converging.
            lines.push(Line::default());
        }
    }

    lines.push(Line::default()); // gap

    // The harbor wave — the logo sits on water.
    let logo_width = LOGO_ART[0].chars().count();
    lines.push(wave_line(elapsed, colors, logo_width));

    lines.push(Line::default()); // gap

    // Tagline fades in, staggered after convergence.
    let tagline_t = fade_t(elapsed, TAGLINE_FADE_AT, TAGLINE_FADE_DUR);
    lines.push(Line::from(Span::styled(
        "curated torrents straight from your terminal",
        Style::default().fg(lerp_color(colors.dim(), colors.text(), tagline_t).to_ratatui()),
    )));

    // Version follows the tagline.
    let version_t = fade_t(elapsed, VERSION_FADE_AT, VERSION_FADE_DUR);
    lines.push(Line::from(Span::styled(
        "harbour v0.1.0",
        Style::default().fg(lerp_color(colors.bg(), colors.muted(), version_t).to_ratatui()),
    )));

    lines.push(Line::default()); // gap

    // Status line: spinner + label. "raising anchor…" (muted) until ready,
    // then a brief white-hot flash that settles into success.
    let ready = elapsed >= READY_AFTER;
    let flash_t = if ready {
        (elapsed - READY_AFTER).as_secs_f64() / 0.4
    } else {
        0.0
    };
    let status_color = if ready {
        let flash = (flash_t * std::f64::consts::PI).sin().clamp(0.0, 1.0);
        lerp_color(colors.success(), HOT, flash)
    } else {
        colors.muted()
    };
    let label = if ready {
        "ready — press any key"
    } else {
        "raising anchor…"
    };
    let mut status = Line::default();
    status.push_span(Span::styled(
        splash.spinner.current().to_string(),
        Style::default().fg(status_color.to_ratatui()),
    ));
    status.push_span(Span::styled(
        format!(" {label}"),
        Style::default().fg(status_color.to_ratatui()),
    ));
    lines.push(status);

    lines.push(Line::default()); // bottom padding

    // --- centered rounded panel ---------------------------------------
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let content_height = lines.len();
    let area = frame.area();
    let panel_w = (content_width + 2).min(area.width as usize) as u16;
    let panel_h = (content_height + 2).min(area.height as usize) as u16;
    let x = area.x + area.width.saturating_sub(panel_w) / 2;
    let y = area.y + area.height.saturating_sub(panel_h) / 2;
    let panel_area = Rect::new(x, y, panel_w, panel_h);

    // Breathing border: border color eases toward accent and back.
    let breath =
        (elapsed.as_secs_f64() / BREATH_PERIOD.as_secs_f64() * std::f64::consts::TAU).sin();
    let border_color = lerp_color(colors.border(), colors.accent(), (breath * 0.5 + 0.5) * 0.8);

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
        .border_style(Style::default().fg(border_color.to_ratatui()))
        .title(Span::styled(" harbour ", Style::default().fg(accent)))
        .style(Style::default().bg(bg));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().bg(bg)),
        panel_area,
    );

    // --- particles -----------------------------------------------------
    // Rendered after the panel so they can sit on its border/background;
    // skipped on tiny terminals where the panel fills the whole screen.
    if panel_area.width >= 20 && panel_area.height >= 8 {
        for p in &splash.particles {
            let px = panel_area.x + (p.fx * panel_area.width as f64) as u16;
            let py = panel_area.y + (p.fy * panel_area.height as f64) as u16;
            let tw = ((elapsed.as_secs_f64() * 1.7 + p.phase).sin() * 0.5 + 0.5).powf(2.0); // fast attack, slow fall — sparkle, not sine
            let color = lerp_color(colors.muted(), colors.accent(), tw);
            if let Some(cell) = frame
                .buffer_mut()
                .cell_mut(ratatui::layout::Position::new(px, py))
            {
                cell.set_symbol("·");
                cell.set_fg(color.to_ratatui());
            }
        }
    }
}

/// Fade-in progress for a staggered element: 0 before `at`, 1 after
/// `at + dur`, linear between.
fn fade_t(elapsed: Duration, at: Duration, dur: Duration) -> f64 {
    if elapsed < at {
        0.0
    } else {
        ((elapsed - at).as_secs_f64() / dur.as_secs_f64()).clamp(0.0, 1.0)
    }
}

/// The whole interactive app: screen, UI state, fake engine, and the eased
/// display values views consume.
struct App {
    theme: Theme,
    screen: Screen,
    prev_screen: Screen,
    state: AppState,
    splash: SplashState,
    start: Instant,
    fake: FakeEngine,
    /// The active search's fan-out plan; `None` when idle or curated.
    search_plan: Option<SearchPlan>,
    /// Per-source delivery flags (index-aligned with `search_plan`).
    plan_delivered: Vec<bool>,
    /// When the current search started — latencies count from here.
    search_started: Instant,
    display: DisplayState,
    /// Eased answered-sources fraction for the status bar.
    answered_eased: EasedValue,
    /// Status spinner shared by splash and search status lines.
    spinner: Spinner,
}

impl App {
    fn new(theme: Theme) -> Self {
        let spinner = Spinner::new(theme.symbols.spinner_frames.clone());
        let mut app = Self {
            splash: SplashState::new(&theme),
            theme,
            screen: Screen::Splash,
            prev_screen: Screen::Search,
            state: AppState::default(),
            start: Instant::now(),
            fake: FakeEngine::new(),
            search_plan: None,
            plan_delivered: Vec::new(),
            search_started: Instant::now(),
            display: DisplayState::default(),
            answered_eased: EasedValue::new(0.0, EASE_TAU),
            spinner,
        };
        // The app opens in the search bar, ready to type — action keys
        // (`d`/`p`/`o`) stay literal until Enter commits a search.
        app.state.search.editing = true;
        app
    }

    /// Fake wall-clock ms for history timestamps (deterministic per runtime).
    fn epoch_ms(&self, now: Instant) -> i64 {
        BASE_EPOCH_MS + now.duration_since(self.start).as_millis() as i64
    }

    /// Handles one key event; returns `true` when the user asked to quit.
    fn handle_key(&mut self, key: KeyEvent, now: Instant) -> bool {
        // Clear transient banners on any new input (design.md §8: failures
        // are loud, but not sticky).
        self.state.error_banner = None;

        let is_quit = matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('q'),
                ..
            } | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
        );
        if is_quit {
            return true;
        }

        match self.screen {
            Screen::Splash => {
                // Any key after "ready" advances to search; q handled above.
                if now.duration_since(self.splash.start) >= READY_AFTER {
                    self.screen = Screen::Search;
                }
            }
            Screen::Help => match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.screen = self.prev_screen,
                _ => {}
            },
            Screen::Search => self.handle_search_key(key),
            Screen::Downloads => self.handle_downloads_key(key),
        }
        false
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            // Action keys first (guarded): while browsing results they fire;
            // while typing they fall through to the typing arm below, so
            // `d`, `p` and `o` stay typable in the query bar.
            KeyCode::Char('d') if !self.state.search.editing => self.download_selected(false),
            KeyCode::Char('D') if !self.state.search.editing => self.download_selected(true),
            KeyCode::Char('o') if !self.state.search.editing => {
                self.state.error_banner =
                    Some("output folder changes arrive with persistence (phase 5)".to_string());
            }
            KeyCode::Char('p') if !self.state.search.editing => {
                self.toggle_pause_for_selected_result()
            }
            KeyCode::Char(c) if c.is_ascii_graphic() || c == ' ' => {
                // Typing always targets the search bar; if the sidebar had
                // focus, typing means "back to searching".
                self.state.search.draft.push(c);
                self.state.search.editing = true;
                self.state.search.focus = crate::types::Focus::Results;
            }
            KeyCode::Backspace => {
                self.state.search.draft.pop();
                self.state.search.editing = true;
            }
            KeyCode::Enter => {
                if self.state.search.focus == crate::types::Focus::Sidebar {
                    self.apply_sidebar_entry();
                } else {
                    self.state.search.editing = false;
                    self.commit_search();
                }
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left | KeyCode::Right => {
                self.state.search.focus = match self.state.search.focus {
                    crate::types::Focus::Results => crate::types::Focus::Sidebar,
                    crate::types::Focus::Sidebar => crate::types::Focus::Results,
                };
            }
            KeyCode::Tab => {
                self.prev_screen = Screen::Search;
                self.screen = Screen::Downloads;
            }
            KeyCode::Esc => {
                // Back out of the sidebar first, then any filter, then return
                // to editing so a fresh query can be typed immediately.
                if self.state.search.focus == crate::types::Focus::Sidebar {
                    self.state.search.focus = crate::types::Focus::Results;
                } else if self.state.search.filter != crate::types::SidebarFilter::All {
                    self.state.search.filter = crate::types::SidebarFilter::All;
                } else {
                    self.state.search.editing = true;
                }
            }
            KeyCode::Char('?') => {
                self.prev_screen = Screen::Search;
                self.screen = Screen::Help;
            }
            _ => {}
        }
    }

    fn handle_downloads_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                let n = self.state.downloads.items.len();
                if n > 0 {
                    self.state.downloads.selected = self.state.downloads.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let n = self.state.downloads.items.len();
                if n > 0 {
                    self.state.downloads.selected = (self.state.downloads.selected + 1).min(n - 1);
                }
            }
            KeyCode::Left | KeyCode::Right => {
                self.state.downloads.show_seeding = !self.state.downloads.show_seeding;
            }
            KeyCode::Tab => {
                self.prev_screen = Screen::Downloads;
                self.screen = Screen::Search;
            }
            KeyCode::Esc => {}
            KeyCode::Char('?') => {
                self.prev_screen = Screen::Downloads;
                self.screen = Screen::Help;
            }
            KeyCode::Char('p') => self.toggle_pause_on_selected_item(),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let search = &mut self.state.search;
        match search.focus {
            crate::types::Focus::Results => {
                let n = crate::ui::search::visible_count(search);
                if n == 0 {
                    return;
                }
                let next = search.selected as isize + delta;
                search.selected = next.clamp(0, n as isize - 1) as usize;
            }
            crate::types::Focus::Sidebar => {
                let n = crate::ui::search::sidebar_count();
                if n == 0 {
                    return;
                }
                let next = search.sidebar_selected as isize + delta;
                search.sidebar_selected = next.clamp(0, n as isize - 1) as usize;
            }
        }
    }

    /// Applies the currently selected sidebar entry as the filter and returns
    /// focus to the results (design.md §2.2).
    fn apply_sidebar_entry(&mut self) {
        let entry = crate::ui::search::sidebar_entry_at(self.state.search.sidebar_selected);
        self.state.search.filter = match entry {
            crate::ui::search::SidebarEntryKind::All => crate::types::SidebarFilter::All,
            crate::ui::search::SidebarEntryKind::Group(g) => crate::types::SidebarFilter::Group(g),
            crate::ui::search::SidebarEntryKind::Source(id) => {
                crate::types::SidebarFilter::Source(id)
            }
        };
        self.state.search.selected = 0;
        self.state.search.focus = crate::types::Focus::Results;
    }

    /// Starts a search (Enter). Empty query → curated top lists; otherwise a
    /// fresh fan-out plan whose answers stream in as latencies expire.
    fn commit_search(&mut self) {
        let draft = self.state.search.draft.trim().to_string();
        let search = &mut self.state.search;
        search.selected = 0;
        search.sidebar_selected = 0;
        search.results.clear();
        search.tags.clear();
        search.source_health.clear();
        search.source_counts.clear();

        if draft.is_empty() {
            // Curated top lists: everything arrives at once (design.md §1).
            search.query.clear();
            search.searching = false;
            self.search_plan = None;
            self.plan_delivered.clear();
            let curated = self.fake.curated();
            self.ingest_results(curated);
            self.answered_eased.set_target(1.0);
        } else {
            search.query = draft.clone();
            search.searching = true;
            let plan = self.fake.plan(&draft);
            self.plan_delivered = vec![false; plan.per_source.len()];
            self.search_started = Instant::now();
            self.search_plan = Some(plan);
            self.answered_eased.set_target(0.0);
        }
    }

    /// Merges one source's answers into the deduped results list: a row per
    /// info_hash, every reporting source added to its tag set (design.md §6).
    fn ingest_results(&mut self, results: Vec<TorrentResult>) {
        for r in results {
            // Capture the source id before `r` moves into the results list.
            let src = r.source;
            let tags = self
                .state
                .search
                .tags
                .entry(r.info_hash.clone())
                .or_default();
            if !tags.contains(&src) {
                tags.push(src);
            }
            if !self
                .state
                .search
                .results
                .iter()
                .any(|e| e.info_hash == r.info_hash)
            {
                self.state.search.results.push(r);
            }
            *self.state.search.source_counts.entry(src).or_insert(0) += 1;
        }
    }

    /// Adds the selected result to the fake queue (design.md: `d` default
    /// folder, `Shift+d` picked folder — the picker ships with the engine).
    fn download_selected(&mut self, pick_folder: bool) {
        if pick_folder {
            self.state.error_banner =
                Some("folder picker ships with the engine (phase 4) — using default".to_string());
        }
        let Some(r) = crate::ui::search::selected_result(&self.state.search) else {
            self.state.error_banner = Some("no result selected — search first".to_string());
            return;
        };
        if self
            .state
            .downloads
            .items
            .iter()
            .any(|i| i.id == r.info_hash)
        {
            self.state.error_banner = Some("already in the queue".to_string());
            return;
        }
        let item = QueueItem {
            id: r.info_hash.clone(),
            name: r.name.clone(),
            source: Some(r.source.to_string()),
            magnet: r.magnet.clone(),
            dir: std::path::PathBuf::from("~/Downloads"),
            status: QueueStatus::Queued,
            finished: false,
            progress: 0.0,
            total_bytes: r.size_bytes,
            downloaded_bytes: 0,
            speed_mib: 0.0,
            upload_speed_mib: 0.0,
            uploaded_bytes: 0,
            peers: None,
            eta_secs: None,
            error: None,
            added_at_epoch_ms: self.epoch_ms(Instant::now()),
        };
        self.state.downloads.items.push(item);
        self.promote_queue();
    }

    /// Oldest-first promotion while a slot is free (HARBOUR_MAX_DOWNLOADS).
    fn promote_queue(&mut self) {
        let active = self
            .state
            .downloads
            .items
            .iter()
            .filter(|i| i.status == QueueStatus::Downloading)
            .count();
        if active >= MAX_DOWNLOADS {
            return;
        }
        if let Some(item) = self
            .state
            .downloads
            .items
            .iter_mut()
            .find(|i| i.status == QueueStatus::Queued)
        {
            item.status = QueueStatus::Downloading;
            item.speed_mib = self.fake.download_speed(&item.id);
            item.peers = Some(self.fake.peer_count(&item.id));
        }
    }

    /// Toggles pause/stop for the queue item matching the selected result.
    fn toggle_pause_for_selected_result(&mut self) {
        let Some(r) = crate::ui::search::selected_result(&self.state.search) else {
            return;
        };
        let Some(idx) = self
            .state
            .downloads
            .items
            .iter()
            .position(|i| i.id == r.info_hash)
        else {
            self.state.error_banner =
                Some("not in the queue — press d to download it first".to_string());
            return;
        };
        self.toggle_pause_at(idx);
    }

    /// Toggles pause/stop for the downloads-view selection (`p`).
    fn toggle_pause_on_selected_item(&mut self) {
        let idx = self.state.downloads.selected;
        if idx < self.state.downloads.items.len() {
            self.toggle_pause_at(idx);
        }
    }

    fn toggle_pause_at(&mut self, idx: usize) {
        let item = &mut self.state.downloads.items[idx];
        match (item.status, item.finished) {
            (QueueStatus::Downloading, _) => item.status = QueueStatus::Paused,
            (QueueStatus::Paused, false) => {
                item.status = QueueStatus::Downloading;
                item.speed_mib = self.fake.download_speed(&item.id);
                item.peers = Some(self.fake.peer_count(&item.id));
            }
            (QueueStatus::Seeding, _) => item.status = QueueStatus::Paused,
            (QueueStatus::Paused, true) => item.status = QueueStatus::Seeding,
            _ => {}
        }
    }

    /// Advances the simulated world by one frame: streams fake answers whose
    /// latencies have expired, moves fake downloads forward, and eases every
    /// displayed value.
    fn tick(&mut self, now: Instant, dt: Duration) {
        self.spinner.advance(now, SPINNER_INTERVAL);

        // --- stream fake source answers -----------------------------------
        // Clone the plan so we can mutate self while iterating (the plan
        // borrows `self.search_plan`, which would fight the ingest below).
        if let Some(plan) = self.search_plan.clone() {
            let since = now.duration_since(self.search_started);
            for (i, sp) in plan.per_source.iter().enumerate() {
                if self.plan_delivered[i] {
                    continue;
                }
                if (since.as_millis() as u64) < sp.latency_ms {
                    continue;
                }
                let health = match sp.status {
                    crate::types::SourceStatus::Online => {
                        let results = self.fake.results(&self.state.search.query, sp.source.id);
                        self.ingest_results(results);
                        crate::types::SourceStatus::Online
                    }
                    other => other,
                };
                self.state.search.source_health.insert(sp.source.id, health);
                self.plan_delivered[i] = true;
            }
            if self.plan_delivered.iter().all(|d| *d) {
                self.search_plan = None;
                self.state.search.searching = false;
            }
        }

        // --- fake queue progress ------------------------------------------
        let dt_secs = dt.as_secs_f64();
        // Hoisted: `self.epoch_ms` borrows all of self, which would collide
        // with the mutable borrow of the items below.
        let epoch_ms = self.epoch_ms(now);
        let mut completed: Vec<HistoryItem> = Vec::new();
        for item in &mut self.state.downloads.items {
            match item.status {
                QueueStatus::Downloading => {
                    let bytes = item.speed_mib * 1048576.0 * dt_secs;
                    item.downloaded_bytes =
                        (item.downloaded_bytes as f64 + bytes).min(item.total_bytes as f64) as u64;
                    item.progress = item.downloaded_bytes as f64 / item.total_bytes.max(1) as f64;
                    let remaining = item.total_bytes.saturating_sub(item.downloaded_bytes);
                    item.eta_secs = if item.speed_mib > 0.0 {
                        Some((remaining as f64 / (item.speed_mib * 1048576.0)).ceil() as u64)
                    } else {
                        None
                    };
                    if item.progress >= 1.0 {
                        item.status = QueueStatus::Seeding;
                        item.finished = true;
                        item.progress = 1.0;
                        item.peers = None;
                        item.eta_secs = None;
                        item.upload_speed_mib = self.fake.upload_speed(&item.id);
                        completed.push(HistoryItem {
                            id: item.id.clone(),
                            name: item.name.clone(),
                            size_bytes: item.total_bytes,
                            source: item.source.clone(),
                            completed_at_epoch_ms: epoch_ms,
                        });
                    }
                }
                QueueStatus::Seeding => {
                    item.uploaded_bytes = (item.uploaded_bytes as f64
                        + item.upload_speed_mib * 1048576.0 * dt_secs)
                        as u64;
                }
                _ => {}
            }
        }
        if !completed.is_empty() {
            self.state.downloads.history.extend(completed);
        }
        self.promote_queue();

        // --- ease displayed values ----------------------------------------
        let answered_n = self.state.search.source_health.len();
        self.answered_eased
            .set_target(answered_n as f64 / SOURCES.len() as f64);
        self.answered_eased.update(dt);
        self.display.answered = self.answered_eased.value();

        for item in &self.state.downloads.items {
            let eased = self
                .display
                .progress
                .entry(item.id.clone())
                .or_insert_with(|| item.progress);
            let target = item.progress;
            *eased = crate::anim::eased(*eased, target, dt, EASE_TAU);
        }
    }

    /// Paints the current screen plus any error banner overlay.
    fn draw(&mut self, frame: &mut Frame, now: Instant) {
        let elapsed = now.duration_since(self.start);
        let view = if self.screen == Screen::Help {
            self.prev_screen
        } else {
            self.screen
        };
        match view {
            Screen::Splash => draw_splash(frame, &self.theme, &mut self.splash, now),
            Screen::Search => {
                let vars = FrameVars {
                    elapsed,
                    spinner: Some(self.spinner.current()),
                };
                ui::search::draw(
                    frame,
                    frame.area(),
                    &self.state.search,
                    &self.display,
                    &vars,
                    &self.theme,
                );
            }
            Screen::Downloads => {
                let vars = FrameVars {
                    elapsed,
                    spinner: Some(self.spinner.current()),
                };
                ui::downloads::draw(
                    frame,
                    frame.area(),
                    &self.state.downloads,
                    &self.display,
                    &vars,
                    &self.theme,
                );
            }
            Screen::Help => {}
        }
        if self.screen == Screen::Help {
            ui::help::draw(frame, frame.area(), &self.theme);
        }
        if let Some(msg) = &self.state.error_banner {
            ui::status::draw_error(frame, frame.area(), &self.theme, msg);
        }
    }
}

/// Runs the TUI: enters the terminal (raw mode + alternate screen + hidden
/// cursor), renders at 30fps until the user quits, and restores the terminal
/// on every exit path via [`TerminalGuard`] (normal quit, errors, and panics
/// — Drop runs during unwinding).
pub fn run(theme: Theme) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = TerminalGuard::enter()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    let mut app = App::new(theme);

    loop {
        let dt = ticker.elapsed();
        let sleep = ticker.next();

        // Collect the whole input burst within this tick — N events produce
        // exactly one draw (design.md §3 coalescing).
        let mut quit = false;
        if event::poll(sleep)? {
            loop {
                if let Event::Key(key) = event::read()? {
                    let now = Instant::now();
                    if app.handle_key(key, now) {
                        quit = true;
                        break;
                    }
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if quit {
            break;
        }

        let now = Instant::now();
        app.tick(now, dt);
        // Each frame is one synchronized write — no flicker/tearing between
        // the border, logo, and status line (docs/design.md §2).
        anim::with_sync_output(|| {
            terminal.draw(|frame| app.draw(frame, now))?;
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers as KM;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KM::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)), Instant::now());
        }
    }

    fn app_on_search() -> App {
        let mut app = App::new(Theme::titanium());
        app.screen = Screen::Search;
        app
    }

    #[test]
    fn typing_and_enter_start_a_streaming_search() {
        let mut app = app_on_search();
        typed(&mut app, "dune");
        app.handle_key(key(KeyCode::Enter), Instant::now());
        assert_eq!(app.state.search.query, "dune");
        assert!(app.state.search.searching, "search is streaming");
        assert!(app.search_plan.is_some());
        assert_eq!(
            app.plan_delivered.len(),
            SOURCES.len(),
            "one slot per source"
        );
    }

    #[test]
    fn streaming_delivers_all_sources_and_dedupes() {
        let mut app = app_on_search();
        typed(&mut app, "dune");
        app.handle_key(key(KeyCode::Enter), Instant::now());
        // Fake 10s of runtime so every simulated latency has expired.
        app.search_started = Instant::now() - Duration::from_secs(10);
        app.tick(Instant::now(), Duration::from_millis(33));

        assert!(!app.state.search.searching, "all sources answered");
        assert_eq!(app.state.search.source_health.len(), SOURCES.len());
        let offline = app
            .state
            .search
            .source_health
            .values()
            .filter(|s| **s == crate::types::SourceStatus::Offline)
            .count();
        assert_eq!(offline, 1, "the plan's offline source reports offline");
        assert!(!app.state.search.results.is_empty());

        // The shared REMUX kind must be one row with ≥2 tags.
        let remux = app
            .state
            .search
            .results
            .iter()
            .find(|r| r.name.contains("REMUX"))
            .expect("remux row");
        let tags = app.state.search.tags.get(&remux.info_hash).unwrap();
        assert!(tags.len() >= 2, "staggered tags: {tags:?}");
        assert_eq!(
            app.state
                .search
                .results
                .iter()
                .filter(|r| r.info_hash == remux.info_hash)
                .count(),
            1,
            "deduped by info_hash"
        );
    }

    #[test]
    fn empty_enter_loads_curated_lists() {
        let mut app = app_on_search();
        app.handle_key(key(KeyCode::Enter), Instant::now());
        assert!(!app.state.search.searching);
        // 8 picks x 2 sources, deduped by info_hash into 8 rows with 2 tags.
        assert_eq!(app.state.search.results.len(), 8);
        for r in &app.state.search.results {
            let tags = app.state.search.tags.get(&r.info_hash).unwrap();
            assert_eq!(tags.len(), 2, "staggered pair: {tags:?}");
        }
    }

    #[test]
    fn download_adds_queued_item_then_promotes() {
        let mut app = app_on_search();
        typed(&mut app, "dune");
        app.handle_key(key(KeyCode::Enter), Instant::now());
        app.search_started = Instant::now() - Duration::from_secs(10);
        app.tick(Instant::now(), Duration::from_millis(33));

        app.handle_key(key(KeyCode::Char('d')), Instant::now());
        let items = &app.state.downloads.items;
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].status,
            QueueStatus::Downloading,
            "slot free → promoted"
        );
        assert!(items[0].speed_mib > 0.0);

        // Progress advances over fake time.
        app.tick(Instant::now(), Duration::from_secs(30));
        let item = &app.state.downloads.items[0];
        assert!(item.progress > 0.0, "download moved: {}", item.progress);
        assert_eq!(item.status, QueueStatus::Downloading);
    }

    #[test]
    fn duplicate_download_is_rejected_with_banner() {
        let mut app = app_on_search();
        typed(&mut app, "dune");
        app.handle_key(key(KeyCode::Enter), Instant::now());
        app.search_started = Instant::now() - Duration::from_secs(10);
        app.tick(Instant::now(), Duration::from_millis(33));

        app.handle_key(key(KeyCode::Char('d')), Instant::now());
        app.handle_key(key(KeyCode::Char('d')), Instant::now());
        assert_eq!(app.state.downloads.items.len(), 1);
        assert!(
            app.state
                .error_banner
                .as_deref()
                .is_some_and(|m| m.contains("already in the queue")),
            "banner: {:?}",
            app.state.error_banner
        );
    }

    #[test]
    fn pause_toggles_lifecycle() {
        let mut app = app_on_search();
        typed(&mut app, "dune");
        app.handle_key(key(KeyCode::Enter), Instant::now());
        app.search_started = Instant::now() - Duration::from_secs(10);
        app.tick(Instant::now(), Duration::from_millis(33));
        app.handle_key(key(KeyCode::Char('d')), Instant::now());

        // Pause the download from the search screen via `p`.
        app.handle_key(key(KeyCode::Char('p')), Instant::now());
        assert_eq!(app.state.downloads.items[0].status, QueueStatus::Paused);
        // Resume.
        app.handle_key(key(KeyCode::Char('p')), Instant::now());
        assert_eq!(
            app.state.downloads.items[0].status,
            QueueStatus::Downloading
        );
    }

    #[test]
    fn sidebar_enter_applies_filter() {
        let mut app = app_on_search();
        app.state.search.focus = crate::types::Focus::Sidebar;
        // Sidebar row 2 = "Games" group (all, Games, fitgirl, ...).
        app.state.search.sidebar_selected = 1;
        app.handle_key(key(KeyCode::Enter), Instant::now());
        assert_eq!(
            app.state.search.filter,
            crate::types::SidebarFilter::Group(crate::types::SourceGroup::Games)
        );
        assert_eq!(app.state.search.focus, crate::types::Focus::Results);
    }

    #[test]
    fn tab_switches_screens() {
        let mut app = app_on_search();
        app.handle_key(key(KeyCode::Tab), Instant::now());
        assert_eq!(app.screen, Screen::Downloads);
        app.handle_key(key(KeyCode::Tab), Instant::now());
        assert_eq!(app.screen, Screen::Search);
    }

    #[test]
    fn q_quits_from_any_screen() {
        let mut app = app_on_search();
        assert!(app.handle_key(key(KeyCode::Char('q')), Instant::now()));
    }
}
