//! Application shell: terminal lifecycle, the 30fps event/draw loop, and the
//! boot splash (UI slice 1).
//!
//! Later slices swap the splash view for search/downloads; this module owns
//! the parts that stay — entering/leaving the terminal safely on every exit
//! path and the tick-coalesced render loop.
//!
//! The splash is deliberately over the top (omp-grade energy): a block-letter
//! HARBOUR logo that converges with a CRT-style flicker, a shimmer band that
//! sweeps across it, twinkling particles, a scrolling harbor wave, a breathing
//! border, and staggered tagline/status fades — everything still live at 30fps
//! until the user quits. All color comes from the theme's curated subset
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

use crate::anim::{self, Spinner, Ticker};
use crate::theme::{Color, Theme};
use std::sync::{Arc, Mutex};

/// Base render cadence (docs/design.md §Animation): the loop redraws at most
/// once per tick; a burst of input within one tick coalesces into one frame.
const FPS: u32 = 30;

/// Logo convergence window: rows flicker in over this span.
const DRAW_IN: Duration = Duration::from_millis(700);

/// After this much time the splash status line flips to "ready".
const READY_AFTER: Duration = Duration::from_millis(1600);

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
/// plus the splash's explicit hint.
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
/// per tick no matter how many keys arrived within it.
fn drain_events() -> io::Result<bool> {
    loop {
        match event::read()? {
            Event::Key(key) if is_quit_key(&key) => return Ok(true),
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

/// Boot splash state. Kept in a struct so a later slice can swap in a
/// different view without touching the loop.
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

/// Runs the TUI: enters the terminal (raw mode + alternate screen + hidden
/// cursor), renders the animated splash at 30fps until the user quits, and
/// restores the terminal on every exit path via [`TerminalGuard`] (normal
/// quit, errors, and panics — Drop runs during unwinding).
///
/// Takes the shared `Arc<Mutex<Theme>>` rather than an owned `Theme` so the
/// theme-watcher thread can swap themes underneath a running render loop
/// (docs/theming.md §Custom themes). The lock is taken once per frame and
/// released before the next `poll`, so a watcher swap never blocks input.
pub fn run(theme: Arc<Mutex<Theme>>) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = TerminalGuard::enter()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    // Splash state is seeded from the theme active at startup; a later swap
    // repaints with new colors on the next frame.
    let mut splash = SplashState::new(&lock_theme(&theme));

    loop {
        // Block until the next frame slot or the first pending input; then
        // drain the whole burst so N events in one tick produce one draw.
        if event::poll(ticker.next())? && drain_events()? {
            break;
        }
        let now = Instant::now();
        splash.spinner.advance(now, SPINNER_INTERVAL);
        let active = lock_theme(&theme);
        // Each frame is one synchronized write — no flicker/tearing between
        // the border, logo, and status line (docs/design.md §2).
        anim::with_sync_output(|| {
            terminal.draw(|frame| draw_splash(frame, &active, &mut splash, now))?;
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
