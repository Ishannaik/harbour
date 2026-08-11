//! Application shell: terminal lifecycle, the 30fps event/draw loop, the boot
//! splash, and input dispatch for the phase-2 views (search / downloads /
//! help) against fake data.
//!
//! The loop owns everything that stays when the engine lands: entering and
//! leaving the terminal safely on every exit path, the tick-coalesced render
//! loop, and the keybind dispatch (docs/design.md §Keybinds). Views are pure
//! paint (`ui/*`); they never read input or mutate state. The engine and
//! sources tracks land later, so search results, the queue, and history come
//! from the deterministic fake generator (`fake.rs`) until then.
//!
//! The splash is deliberately over the top (omp-grade energy): a block-letter
//! HARBOUR logo that converges with a CRT-style flicker, a shimmer band that
//! sweeps across it, twinkling particles, a scrolling harbor wave, a breathing
//! border, and staggered tagline/status fades — everything still live at 30fps
//! until the app auto-advances to search (FR-01: the splash is a timed intro,
//! not a resting state). All color comes from the theme's curated subset
//! (accent/text/success/muted/border/bg), so custom themes keep working.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

use crate::anim::{self, Spinner, Ticker};
use crate::fake;
use crate::queue_store;
use crate::theme::{Color, Theme};
use crate::types::{
    AppState, NowPlaying, QueueItem, QueueStatus, Screen, SourceStatus, TorrentResult,
};
use crate::ui;
use crate::watch;

/// Base render cadence (docs/design.md §Animation): the loop redraws at most
/// once per tick; a burst of input within one tick coalesces into one frame.
const FPS: u32 = 30;

/// Logo convergence window: rows flicker in over this span.
const DRAW_IN: Duration = Duration::from_millis(700);

/// After this much time the splash status line flips to "ready".
const READY_AFTER: Duration = Duration::from_millis(1600);

/// Splash is a timed intro (FR-01): it auto-advances to search this long
/// after boot; any key skips the wait.
const SPLASH_DURATION: Duration = Duration::from_millis(2400);

/// Status spinner cadence (docs/design.md §Animation): one frame per 80ms.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

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

/// Fake search latency (MVP): long enough for the streaming shimmer/spinner
/// to read as "streaming" (design §2.2), short enough to stay snappy. The
/// engine track replaces this with real per-source answers.
const SEARCH_LATENCY: Duration = Duration::from_millis(400);

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

/// A tiny xorshift64* PRNG. Seeded at splash construction so every run shows
/// the same particle field and glitch pattern — deterministic, testable.
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

/// Quit keys: `q`, `Esc`, and `Ctrl+C` — the conventional TUI escape hatches
/// plus the splash's explicit hint. Esc doubles as "close the help overlay"
/// when help is open (handled before this is consulted).
fn is_quit_key(key: &KeyEvent) -> bool {
    matches!(
        key,
        KeyEvent {
            code: KeyCode::Char('q'),
            ..
        } | KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } | KeyEvent {
            code: KeyCode::Esc,
            ..
        }
    )
}

/// Drains every input event currently queued and reports whether the user
/// asked to quit. Draining in a loop coalesces a burst of events into one
/// frame (docs/design.md §Animation): the outer loop renders at most once
/// per tick no matter how many keys arrived within it. Key events route
/// through `app.handle_key`, which owns the per-screen dispatch.
fn drain_events(app: &mut App) -> io::Result<bool> {
    loop {
        match event::read()? {
            Event::Key(key) => {
                if app.handle_key(&key) {
                    return Ok(true);
                }
            }
            // Resize needs no state fix-up here: the next frame redraws
            // against the new area, and every view already lays out from the
            // area it is handed.
            Event::Resize(..) => {}
            _ => {}
        }
        if !event::poll(Duration::ZERO)? {
            return Ok(false);
        }
    }
}

/// A particle's fixed position (as area fractions) and twinkle phase.
struct Particle {
    fx: f64,
    fy: f64,
    phase: f64,
}

/// Boot splash state. Kept in a struct so the loop can swap views without
/// touching the splash's internals.
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

/// Linear interpolation between two colors. Only RGB endpoints blend;
/// `Index`/`Default` endpoints (custom themes) fall back to `a` unchanged
/// rather than guessing at a ramp.
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
        "ready — press q to quit"
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

/// Top-level app state for the loop: which screen is showing, the view state,
/// and the loop-owned clocks (search latency, spinners). Input dispatch and
/// the per-frame pump live here so keybind tests drive the same code path
/// the loop does.
struct App {
    screen: Screen,
    /// The screen underneath the help overlay; restored when it closes.
    help_base: Screen,
    state: AppState,
    /// Where the queue ledger lives — every mutation saves here (FR-48).
    queue_path: std::path::PathBuf,
    /// When a fake search resolves (`pump_search`), if one is in flight.
    search_deadline: Option<Instant>,
    /// The active watch session (FR-57), if any — server + player child.
    watch: Option<watch::WatchSession>,
    /// Screen to return to when the player exits.
    watch_base: Screen,
    splash: SplashState,
    status_spinner: Spinner,
}

impl App {
    /// Boots on the splash. The queue is loaded from the ledger at
    /// `state_dir/downloads.json` when present; first run seeds the fake
    /// queue and persists it, so pause/enqueue state survives restarts.
    fn new(theme: &Theme, state_dir: &std::path::Path) -> Self {
        let queue_path = state_dir.join("downloads.json");
        let mut state = AppState::default();
        state.downloads.items = match queue_store::load(&queue_path) {
            queue_store::Load::Ok(items) => items,
            _ => {
                let items = fake::fake_queue();
                queue_store::save(&queue_path, &items);
                items
            }
        };
        state.downloads.history = fake::fake_history();
        Self {
            screen: Screen::Splash,
            help_base: Screen::Search,
            state,
            queue_path,
            search_deadline: None,
            watch: None,
            watch_base: Screen::Downloads,
            splash: SplashState::new(theme),
            status_spinner: Spinner::new(theme.symbols.spinner_frames.clone()),
        }
    }

    /// Persists the queue after any mutation (FR-48: write on status change).
    fn save_queue(&self) {
        queue_store::save(&self.queue_path, &self.state.downloads.items);
    }

    /// Handles one key event; returns true when the app should quit.
    ///
    /// Only key *presses* (and OS repeats) are actions. On Windows the
    /// console reports both halves of a tap — Press and Release — and
    /// crossterm's parser emits `KeyEventKind::Release` for most keys
    /// (modifier keys aside), so handling it too would register every key
    /// twice. Repeat is kept so held keys still repeat when keyboard
    /// enhancement is enabled.
    ///
    /// Dispatch order: the help overlay eats every key while open (`?`
    /// toggles, Esc closes, `q`/Ctrl+C still quit), then global quit keys,
    /// then the per-screen handlers. The splash is the exception: any key
    /// (other than quit) skips the intro.
    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return false;
        }
        match self.screen {
            // Watch mode: q/Esc stop the session and return to the TUI
            // (FR-59) — they never quit the app.
            Screen::NowPlaying => {
                self.end_watch();
                false
            }
            Screen::Help => {
                // `q`/Ctrl+C quit even with help open; Esc or any other key
                // closes the overlay (docs/design.md §Keybinds).
                if matches!(key.code, KeyCode::Char('q'))
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return true;
                }
                self.screen = self.help_base;
                false
            }
            _ => {
                if is_quit_key(key) {
                    return true;
                }
                match self.screen {
                    Screen::Splash => self.screen = Screen::Search,
                    Screen::Search => self.search_key(key),
                    Screen::Downloads => self.downloads_key(key),
                    Screen::Help | Screen::NowPlaying => unreachable!("handled above"),
                }
                false
            }
        }
    }

    /// Search-screen keys (docs/design.md §Keybinds + Tab screen cycle).
    fn search_key(&mut self, key: &KeyEvent) {
        let search = &mut self.state.search;
        match key.code {
            KeyCode::Char('?') => self.open_help(),
            // `d` and shift+d both enqueue to the default folder for now:
            // the folder picker is engine-track work (FR-29, phase 4). Must
            // precede the generic Char arm or 'd' would type into the query.
            KeyCode::Char('d' | 'D') if !search.results.is_empty() => self.download_selected(),
            // Tab cycles screens; Left/Right are the downloads tabs' keys.
            KeyCode::Tab => self.screen = Screen::Downloads,
            KeyCode::Enter => self.start_search(),
            KeyCode::Backspace => {
                search.query.pop();
            }
            // Plain printable characters edit the query; modifier chords
            // (Ctrl/Alt) are left for future actions, not typed into it.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                search.query.push(c);
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            _ => {}
        }
    }

    /// Downloads-screen keys: tabs (Left/Right), selection (Up/Down),
    /// pause/resume (`p`), Tab cycles back to search.
    fn downloads_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Tab => self.screen = Screen::Search,
            KeyCode::Left => self.state.downloads.show_seeding = false,
            KeyCode::Right => self.state.downloads.show_seeding = true,
            KeyCode::Up => self.downloads_move(-1),
            KeyCode::Down => self.downloads_move(1),
            KeyCode::Char('p') => self.toggle_pause(),
            // `w` watches the selected item (FR-57): stream its primary
            // media file to an external player (mpv/VLC).
            KeyCode::Char('w') => self.start_watch(),
            _ => {}
        }
    }

    fn open_help(&mut self) {
        self.help_base = self.screen;
        self.screen = Screen::Help;
    }

    /// Kicks off a (fake) search: the shimmer/spinner reads while
    /// `search_deadline` counts down, then `pump_search` applies the rows.
    fn start_search(&mut self) {
        self.state.search.searching = true;
        self.search_deadline = Some(Instant::now() + SEARCH_LATENCY);
    }

    /// Applies a pending fake search once its latency deadline passes —
    /// called every frame so a slow frame cannot miss the transition.
    fn pump_search(&mut self, now: Instant) {
        let Some(deadline) = self.search_deadline else {
            return;
        };
        if now < deadline {
            return;
        }
        self.search_deadline = None;
        let query = self.state.search.query.clone();
        apply_results(&mut self.state.search, &query);
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.state.search.results.len();
        if len == 0 {
            return;
        }
        let i = self.state.search.selected as isize + delta;
        // FR-27: arrows wrap at the ends.
        self.state.search.selected = i.rem_euclid(len as isize) as usize;
    }

    /// `d` on the selected result enqueues a download (FR-29; real enqueue is
    /// engine-track, phase 4). FR-56 dedupe: an info_hash already in the
    /// queue is *focused*, never copied — mashing `d` cannot pile up
    /// duplicates. The focus is visible on the Downloads tab's selection.
    fn download_selected(&mut self) {
        let Some(result) = self
            .state
            .search
            .results
            .get(self.state.search.selected)
            .cloned()
        else {
            return;
        };
        let downloads = &mut self.state.downloads;
        if let Some(pos) = downloads
            .items
            .iter()
            .position(|it| it.id == result.info_hash)
        {
            downloads.selected = pos;
            return;
        }
        let item = queue_item_from_result(&result);
        downloads.items.push(item);
        downloads.selected = downloads.items.len() - 1;
        self.save_queue();
    }

    /// Moves the downloads selection within the *visible* tab's rows, so the
    /// highlighted row always matches what the view paints.
    fn downloads_move(&mut self, delta: isize) {
        let visible: Vec<usize> = self
            .state
            .downloads
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                let seeding = matches!(it.status, QueueStatus::Seeding | QueueStatus::Missing);
                seeding == self.state.downloads.show_seeding
            })
            .map(|(i, _)| i)
            .collect();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|&i| i == self.state.downloads.selected)
            .unwrap_or(0) as isize;
        let next = (pos + delta).rem_euclid(visible.len() as isize) as usize;
        self.state.downloads.selected = visible[next];
    }

    /// `p` toggles pause/resume (FR-43, OQ-1: pause-only — a paused seed
    /// stays in the Seeding tab, disambiguated by `finished`).
    fn toggle_pause(&mut self) {
        let downloads = &mut self.state.downloads;
        let Some(item) = downloads.items.get_mut(downloads.selected) else {
            return;
        };
        let mut changed = false;
        match item.status {
            QueueStatus::Downloading | QueueStatus::Queued => {
                item.status = QueueStatus::Paused;
                item.speed_mib = 0.0;
                changed = true;
            }
            QueueStatus::Paused if item.finished => item.status = QueueStatus::Seeding,
            QueueStatus::Paused => item.status = QueueStatus::Downloading,
            QueueStatus::Seeding => {
                item.status = QueueStatus::Paused;
                item.upload_speed_mib = 0.0;
                changed = true;
            }
            QueueStatus::Failed | QueueStatus::Missing => {} // no-op: nothing to resume
        }
        if changed {
            self.save_queue();
        }
    }

    /// `w` on the selected item: find its media file, find a player
    /// (mpv → VLC), serve the file over loopback with Range support, and
    /// launch the player (FR-57..FR-61). Every failure path is a loud error
    /// banner — never a silent no-op. With fake data there are no files, so
    /// this is exactly the honest error until the engine lands.
    fn start_watch(&mut self) {
        let Some(item) = self
            .state
            .downloads
            .items
            .get(self.state.downloads.selected)
        else {
            return;
        };
        let Some(player) = watch::find_player() else {
            self.state.error_banner =
                Some("watch: no player found — install mpv or VLC and add it to PATH".into());
            return;
        };
        let Some(file) = watch::primary_media(&item.dir) else {
            self.state.error_banner = Some(format!(
                "watch: no media file for '{}' (fake data — engine lands in phase 4)",
                item.name
            ));
            return;
        };
        match watch::WatchSession::start(&file, player) {
            Ok(session) => {
                self.watch_base = self.screen;
                self.state.now_playing = Some(NowPlaying {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    stream_url: session.url.clone(),
                    progress: item.progress,
                });
                self.watch = Some(session);
                self.screen = Screen::NowPlaying;
            }
            Err(e) => {
                self.state.error_banner = Some(format!("watch: cannot start player: {e}"));
            }
        }
    }

    /// Ends the session and returns to the previous screen (FR-59: player
    /// exit or `q`/esc). Kills the player and stops the stream server.
    fn end_watch(&mut self) {
        if let Some(mut session) = self.watch.take() {
            session.stop();
        }
        self.state.now_playing = None;
        self.screen = self.watch_base;
    }

    /// Paints the current screen. The splash owns the full frame; every
    /// other screen carves the status bar (and error banner) off the bottom
    /// and paints the view + status line, with the help modal on top when
    /// help is open.
    fn draw(&mut self, frame: &mut Frame, theme: &Theme, now: Instant) {
        if self.screen == Screen::Splash {
            draw_splash(frame, theme, &mut self.splash, now);
            return;
        }
        let area = frame.area();
        let banner_h = self
            .state
            .error_banner
            .as_ref()
            .map_or(0, |msg| 2 + msg.lines().count().clamp(1, 2) as u16);
        let status_h = 1 + banner_h;
        let view_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(status_h),
        );
        let status_area = Rect::new(
            area.x,
            area.y + view_area.height,
            area.width,
            status_h.min(area.height),
        );

        let base = if self.screen == Screen::Help {
            self.help_base
        } else {
            self.screen
        };
        match base {
            Screen::Search => ui::search::draw(frame, view_area, &self.state.search, theme),
            Screen::Downloads => {
                ui::downloads::draw(frame, view_area, &self.state.downloads, theme)
            }
            Screen::NowPlaying => {
                if let Some(np) = &self.state.now_playing {
                    ui::now_playing::draw(frame, view_area, np, theme);
                }
            }
            Screen::Splash | Screen::Help => {} // base is never one of these
        }

        let glyph = self.status_spinner.current().to_string();
        ui::status::draw(frame, status_area, self.screen, &self.state, theme, &glyph);

        if self.screen == Screen::Help {
            ui::help::draw(frame, area, theme);
        }
    }
}

/// Applies fake results for `query` to the search state: rows, sidebar
/// health dots, and per-source counts (the groups' staggered pop-in). Kept
/// separate from `pump_search` so tests can apply results synchronously.
fn apply_results(search: &mut crate::types::SearchState, query: &str) {
    let results = fake::fake_results(query);
    let mut source_health = HashMap::new();
    let mut source_counts = HashMap::new();
    for r in &results {
        source_health.insert(r.source, SourceStatus::Online);
        *source_counts.entry(r.source).or_insert(0) += 1;
    }
    search.results = results;
    search.selected = 0;
    search.searching = false;
    search.source_health = source_health;
    search.source_counts = source_counts;
}

/// Builds a fake `QueueItem` from a search result — the MVP stand-in for
/// the engine's enqueue (FR-29/FR-39, phase 4).
fn queue_item_from_result(r: &TorrentResult) -> QueueItem {
    QueueItem {
        id: r.info_hash.clone(),
        name: r.name.clone(),
        source: Some(r.source.to_owned()),
        magnet: r.magnet.clone(),
        dir: PathBuf::from("~/harbour/downloads"),
        status: QueueStatus::Downloading,
        finished: false,
        progress: 0.05,
        total_bytes: r.size_bytes,
        downloaded_bytes: 0,
        speed_mib: 6.0,
        upload_speed_mib: 0.0,
        uploaded_bytes: 0,
        peers: Some(24),
        eta_secs: Some(3_600),
        error: None,
        added_at_epoch_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    }
}

/// Runs the TUI: enters the terminal (raw mode + alternate screen + hidden
/// cursor), renders at 30fps, and restores the terminal on every exit path
/// via [`TerminalGuard`] (normal quit, errors, and panics — Drop runs during
/// unwinding).
///
/// Takes the shared `Arc<Mutex<Theme>>` rather than an owned `Theme` so the
/// theme-watcher thread can swap themes underneath a running render loop
/// (docs/theming.md §Custom themes). The lock is taken once per frame and
/// released before the next `poll`, so a watcher swap never blocks input.
pub fn run(
    theme: Arc<Mutex<Theme>>,
    state_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = TerminalGuard::enter()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    // Splash state is seeded from the theme active at startup; a later swap
    // repaints on the next frame (colors are read from the theme per draw,
    // spinner glyphs are re-synced below).
    let mut app = {
        let active = lock_theme(&theme);
        App::new(&active, state_dir)
    };

    loop {
        // Block until the next frame slot or the first pending input; then
        // drain the whole burst so N events in one tick produce one draw.
        if event::poll(ticker.next())? && drain_events(&mut app)? {
            break;
        }
        let now = Instant::now();
        let active = lock_theme(&theme);
        app.pump_search(now);
        // Spinner glyphs come from the theme; re-sync after a live reload so
        // a swap does not keep spinning the old frames. No-op unless changed.
        app.splash
            .spinner
            .set_frames(&active.symbols.spinner_frames);
        app.status_spinner
            .set_frames(&active.symbols.spinner_frames);
        app.splash.spinner.advance(now, SPINNER_INTERVAL);
        app.status_spinner.advance(now, SPINNER_INTERVAL);
        // FR-01: the splash is a timed intro — leave it even if the user
        // never presses a key.
        if app.screen == Screen::Splash && now.duration_since(app.splash.start) >= SPLASH_DURATION {
            app.screen = Screen::Search;
        }
        // Each frame is one synchronized write — no flicker/tearing between
        // the border, logo, and status line (docs/design.md §2).
        // FR-59: when the player exits, the watch session ends and the TUI
        // returns to the previous screen.
        if app.screen == Screen::NowPlaying
            && app
                .watch
                .as_mut()
                .is_some_and(|session| session.player_exited())
        {
            app.end_watch();
        }
        anim::with_sync_output(|| {
            terminal.draw(|frame| app.draw(frame, &active, now))?;
            Ok(())
        })?;
        // Drop before the next poll so the watcher thread can swap themes
        // while we are blocked waiting for input.
        drop(active);
    }
    Ok(())
}

/// Recover from a poisoned theme lock instead of panicking: a watcher thread
/// that panicked mid-swap must not take the render loop down with it. The
/// last value written is still a fully-formed `Theme`, so it is safe to use.
fn lock_theme(theme: &Arc<Mutex<Theme>>) -> std::sync::MutexGuard<'_, Theme> {
    theme
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A unique temp state dir per call — every `app()` boots with its own
    /// ledger, so tests never share (or clobber) queue state.
    fn temp_state_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("harbour-app-test-{nanos}"))
    }

    fn app() -> App {
        App::new(&Theme::titanium(), &temp_state_dir())
    }

    #[test]
    fn pause_survives_restart() {
        // First session: pause the seeding item, then "restart" by building
        // a fresh App against the same state dir.
        let dir = temp_state_dir();
        let mut a = App::new(&Theme::titanium(), &dir);
        a.screen = Screen::Downloads;
        a.state.downloads.selected = 2; // the Seeding item
        tap(&mut a, KeyCode::Char('p'));
        assert_eq!(a.state.downloads.items[2].status, QueueStatus::Paused);

        let b = App::new(&Theme::titanium(), &dir);
        assert_eq!(
            b.state.downloads.items[2].status,
            QueueStatus::Paused,
            "paused seed must stay paused across restart"
        );
        assert!(b.state.downloads.items[2].finished);
    }

    #[test]
    fn enqueued_download_survives_restart() {
        let dir = temp_state_dir();
        let mut a = App::new(&Theme::titanium(), &dir);
        a.screen = Screen::Search;
        apply_results(&mut a.state.search, "dune");
        tap(&mut a, KeyCode::Char('d'));
        let added = a.state.downloads.items.last().unwrap().clone();

        let b = App::new(&Theme::titanium(), &dir);
        assert!(
            b.state.downloads.items.iter().any(|it| it.id == added.id),
            "enqueued download must survive restart"
        );
    }

    // --- watch mode ---

    #[test]
    fn w_with_fake_data_errors_loudly_not_silently() {
        let mut a = app();
        a.screen = Screen::Downloads;
        a.state.downloads.selected = 2; // the Seeding item (fake dir)
        tap(&mut a, KeyCode::Char('w'));
        if a.state.error_banner.is_none() {
            // A real player might be installed; the fake item still has no
            // media file, so the banner must be set either way.
            assert!(
                a.state.error_banner.is_some(),
                "w on fake data must surface an error banner"
            );
        }
        assert_eq!(a.screen, Screen::Downloads, "no session started");
        assert!(a.watch.is_none());
    }

    #[test]
    fn q_on_now_playing_returns_not_quits() {
        // No real session here — just pin the contract: q on the watch
        // screen calls end_watch (returns false, never quits the app).
        let mut a = app();
        a.screen = Screen::NowPlaying;
        a.watch_base = Screen::Downloads;
        assert!(!a.handle_key(&key(KeyCode::Char('q'))));
        assert_eq!(a.screen, Screen::Downloads);
        assert!(a.state.now_playing.is_none());
    }

    /// Shorthand for "handle a key and assert we didn't quit".
    fn tap(app: &mut App, code: KeyCode) {
        assert!(!app.handle_key(&key(code)), "key {code:?} must not quit");
    }

    // --- global quit ---

    #[test]
    fn quit_keys_work_on_every_screen() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut a = app();
            a.screen = Screen::Search;
            assert!(a.handle_key(&key(code)));
        }
        // Ctrl+C specifically needs the CONTROL modifier; a bare `c` is a
        // normal character and must NOT quit.
        let mut a = app();
        a.screen = Screen::Search;
        assert!(a.handle_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('c'));
        assert_eq!(a.screen, Screen::Search, "bare c types into the query");
    }

    // --- splash ---

    #[test]
    fn splash_any_key_advances_to_search() {
        let mut a = app();
        assert_eq!(a.screen, Screen::Splash);
        tap(&mut a, KeyCode::Char('x'));
        assert_eq!(a.screen, Screen::Search);
    }

    // --- search input ---

    #[test]
    fn search_types_and_backspaces_query() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('d'));
        tap(&mut a, KeyCode::Char('u'));
        tap(&mut a, KeyCode::Char('n'));
        assert_eq!(a.state.search.query, "dun");
        tap(&mut a, KeyCode::Backspace);
        assert_eq!(a.state.search.query, "du");
    }

    #[test]
    fn key_release_events_are_ignored() {
        // A physical tap on Windows is Press then Release; handling the
        // Release would double every keystroke (crossterm emits
        // KeyEventKind::Release for most keys on the Windows console).
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('x'));
        assert_eq!(a.state.search.query, "x");
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!a.handle_key(&release));
        assert_eq!(a.state.search.query, "x", "release must not re-register");

        // Releases must not navigate or quit either.
        let mut a = app();
        a.screen = Screen::Search;
        let down =
            KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(!a.handle_key(&down));
        assert_eq!(a.state.search.selected, 0);
        let quit = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!a.handle_key(&quit), "q release must not quit");
    }

    #[test]
    fn enter_starts_search_and_pump_applies_results() {
        let mut a = app();
        a.screen = Screen::Search;
        a.state.search.query = "dune".into();
        tap(&mut a, KeyCode::Enter);
        assert!(a.state.search.searching, "searching flag on");
        assert!(a.search_deadline.is_some());

        // Before the deadline: still searching, no results.
        let t0 = Instant::now();
        a.pump_search(t0 + SEARCH_LATENCY - Duration::from_millis(1));
        assert!(a.state.search.searching);

        // At/after the deadline: deterministic rows + sidebar counts.
        a.pump_search(t0 + SEARCH_LATENCY);
        assert!(!a.state.search.searching);
        assert!(a.search_deadline.is_none());
        assert!(!a.state.search.results.is_empty());
        // "dune" hits the movie catalog, so the row carries the real title
        // (case-sensitive "dune" would miss "Dune: Part Two").
        assert!(
            a.state.search.results[0]
                .name
                .to_ascii_lowercase()
                .contains("dune"),
            "row: {}",
            a.state.search.results[0].name
        );
        assert!(a.state.search.source_counts.contains_key("yts"));
        // Same query, same rows (deterministic fake data).
        let first: Vec<String> = a
            .state
            .search
            .results
            .iter()
            .map(|r| r.info_hash.clone())
            .collect();
        let mut b = app();
        b.screen = Screen::Search;
        b.state.search.query = "dune".into();
        apply_results(&mut b.state.search, "dune");
        let second: Vec<String> = b
            .state
            .search
            .results
            .iter()
            .map(|r| r.info_hash.clone())
            .collect();
        assert_eq!(first, second);
    }

    #[test]
    fn empty_enter_browses_curated_library() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Enter);
        let t0 = Instant::now();
        a.pump_search(t0 + SEARCH_LATENCY);
        assert!(!a.state.search.results.is_empty(), "browse returns rows");
        assert!(a.state.search.query.is_empty());
    }

    #[test]
    fn arrows_wrap_selection_at_ends() {
        let mut a = app();
        a.screen = Screen::Search;
        apply_results(&mut a.state.search, "dune");
        let len = a.state.search.results.len();
        assert!(len > 1);
        tap(&mut a, KeyCode::Up);
        assert_eq!(a.state.search.selected, len - 1, "up wraps to bottom");
        tap(&mut a, KeyCode::Down);
        assert_eq!(a.state.search.selected, 0, "down wraps to top");
    }

    #[test]
    fn d_enqueues_selected_result() {
        let mut a = app();
        a.screen = Screen::Search;
        apply_results(&mut a.state.search, "dune");
        let before = a.state.downloads.items.len();
        tap(&mut a, KeyCode::Char('d'));
        let items = &a.state.downloads.items;
        assert_eq!(items.len(), before + 1);
        let added = items.last().unwrap();
        assert_eq!(added.status, QueueStatus::Downloading);
        assert_eq!(added.name, a.state.search.results[0].name);
        assert_eq!(added.id, a.state.search.results[0].info_hash);
        assert_eq!(a.state.downloads.selected, items.len() - 1);
    }

    #[test]
    fn d_dedupes_on_info_hash_fr56() {
        let mut a = app();
        a.screen = Screen::Search;
        apply_results(&mut a.state.search, "dune");
        let before = a.state.downloads.items.len();
        let first = a.state.search.results[0].info_hash.clone();

        // Two `d` presses on the same row: one item, never a duplicate.
        tap(&mut a, KeyCode::Char('d'));
        tap(&mut a, KeyCode::Char('d'));
        let items = &a.state.downloads.items;
        assert_eq!(items.len(), before + 1, "FR-56: no duplicate enqueue");
        assert_eq!(
            items.iter().filter(|it| it.id == first).count(),
            1,
            "exactly one queue item for the hash"
        );
        // The second press focused the existing item instead.
        let pos = items.iter().position(|it| it.id == first).unwrap();
        assert_eq!(a.state.downloads.selected, pos);

        // A different row still enqueues a new item.
        a.state.search.selected = 1;
        tap(&mut a, KeyCode::Char('d'));
        assert_eq!(a.state.downloads.items.len(), before + 2);
    }

    #[test]
    fn d_release_does_not_enqueue() {
        let mut a = app();
        a.screen = Screen::Search;
        apply_results(&mut a.state.search, "dune");
        let before = a.state.downloads.items.len();
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!a.handle_key(&release));
        assert_eq!(
            a.state.downloads.items.len(),
            before,
            "release half of a d tap must not download"
        );
    }

    #[test]
    fn d_with_no_results_is_a_noop() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('d'));
        assert_eq!(a.state.downloads.items.len(), 3, "fake queue untouched");
    }

    // --- screen navigation ---

    #[test]
    fn tab_cycles_screens_both_ways() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Tab);
        assert_eq!(a.screen, Screen::Downloads);
        tap(&mut a, KeyCode::Tab);
        assert_eq!(a.screen, Screen::Search);
    }

    // --- downloads ---

    #[test]
    fn arrows_switch_downloads_tabs() {
        let mut a = app();
        a.screen = Screen::Downloads;
        assert!(!a.state.downloads.show_seeding);
        tap(&mut a, KeyCode::Right);
        assert!(a.state.downloads.show_seeding);
        tap(&mut a, KeyCode::Left);
        assert!(!a.state.downloads.show_seeding);
    }

    #[test]
    fn downloads_arrows_move_visible_selection() {
        let mut a = app();
        a.screen = Screen::Downloads;
        // Active tab shows 2 of the 3 fake items (Downloading + Paused);
        // the Seeding item is filtered out.
        let active: Vec<usize> = (0..a.state.downloads.items.len())
            .filter(|&i| !matches!(a.state.downloads.items[i].status, QueueStatus::Seeding))
            .collect();
        tap(&mut a, KeyCode::Down);
        assert_eq!(a.state.downloads.selected, active[1]);
        tap(&mut a, KeyCode::Down);
        assert_eq!(a.state.downloads.selected, active[0], "wraps");
    }

    #[test]
    fn p_toggles_pause_per_status() {
        let mut a = app();
        a.screen = Screen::Downloads;
        // Select the Downloading item (index 0), pause it.
        a.state.downloads.selected = 0;
        tap(&mut a, KeyCode::Char('p'));
        assert_eq!(a.state.downloads.items[0].status, QueueStatus::Paused);
        assert_eq!(a.state.downloads.items[0].speed_mib, 0.0);
        // Resume it.
        tap(&mut a, KeyCode::Char('p'));
        assert_eq!(a.state.downloads.items[0].status, QueueStatus::Downloading);

        // Select the Seeding item (index 2), pause the seed — it stays
        // `finished == true` so it still lives on the Seeding tab.
        a.state.downloads.selected = 2;
        tap(&mut a, KeyCode::Char('p'));
        assert_eq!(a.state.downloads.items[2].status, QueueStatus::Paused);
        assert!(a.state.downloads.items[2].finished);
        tap(&mut a, KeyCode::Char('p'));
        assert_eq!(a.state.downloads.items[2].status, QueueStatus::Seeding);
    }

    // --- help overlay ---

    #[test]
    fn help_toggles_and_esc_closes_without_quitting() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('?'));
        assert_eq!(a.screen, Screen::Help);
        assert_eq!(a.help_base, Screen::Search);
        // Esc closes the overlay instead of quitting.
        tap(&mut a, KeyCode::Esc);
        assert_eq!(a.screen, Screen::Search);
        // Any key closes it too (toggling behavior, UR-10).
        tap(&mut a, KeyCode::Char('?'));
        assert_eq!(a.screen, Screen::Help);
        tap(&mut a, KeyCode::Enter);
        assert_eq!(a.screen, Screen::Search);
    }

    #[test]
    fn help_from_downloads_restores_downloads() {
        let mut a = app();
        a.screen = Screen::Downloads;
        tap(&mut a, KeyCode::Char('?'));
        assert_eq!(a.screen, Screen::Help);
        assert_eq!(a.help_base, Screen::Downloads);
        tap(&mut a, KeyCode::Esc);
        assert_eq!(a.screen, Screen::Downloads);
    }

    #[test]
    fn q_quits_even_with_help_open() {
        let mut a = app();
        a.screen = Screen::Search;
        tap(&mut a, KeyCode::Char('?'));
        assert!(a.handle_key(&key(KeyCode::Char('q'))));
    }

    // --- splash buffer snapshot ---

    #[test]
    fn splash_snapshot_after_convergence() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let theme = Theme::titanium();
        let mut splash = SplashState::new(&theme);
        // At exactly DRAW_IN the logo has converged and `converging` is
        // false, so no rng draws happen — the frame is fully deterministic
        // (particles derive from the fixed seed, tagline/version fades are 0,
        // status is still "raising anchor…" before READY_AFTER).
        let now = splash.start + DRAW_IN;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw_splash(f, &theme, &mut splash, now))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let lines: Vec<String> = (0..24)
            .map(|y| {
                let mut l: String = (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect();
                while l.ends_with(' ') {
                    l.pop();
                }
                l
            })
            .collect();
        for (i, l) in lines.iter().enumerate() {
            eprintln!("{i:>2}|{l}");
        }
        // The logo's letters are present (logo_row skips the art's spacing
        // columns, so rows pack: "HHAAARRRR…"), and the timed elements have
        // painted. Particles can replace a single char anywhere, so assert
        // on stable fragments, not full lines.
        assert!(lines.iter().any(|l| l.contains("HHAAARRRR")));
        assert!(lines.iter().any(|l| l.contains("anchor")));
        assert!(lines.iter().any(|l| l.contains("terminal")));
        assert!(lines.iter().any(|l| l.contains("v0")));
    }
}
