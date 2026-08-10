//! Application shell: terminal lifecycle, the 30fps event/draw loop, and the
//! boot splash (UI slice 1).
//!
//! Later slices swap the splash view for search/downloads; this module owns
//! the parts that stay — entering/leaving the terminal safely on every exit
//! path and the tick-coalesced render loop.

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

/// Base render cadence (docs/design.md §Animation): the loop redraws at most
/// once per tick; a burst of input within one tick coalesces into one frame.
const FPS: u32 = 30;

/// Logo draw-in window: anchor rows are revealed over this span.
const DRAW_IN: Duration = Duration::from_millis(600);

/// After this much time the splash status line flips to "ready".
const READY_AFTER: Duration = Duration::from_millis(1600);

/// Status spinner cadence (docs/design.md §Animation): one frame per 80ms.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Splash logo: a stylized anchor drawn with the ⚓ glyph. Rows are revealed
/// top-down over [`DRAW_IN`] and colored accent → text across the drawn
/// portion, so the reveal reads as a gradient sweep. Each row is internally
/// symmetric; `Paragraph`'s per-line centering keeps the whole logo centered.
const ANCHOR_ART: &[&str] = &[
    "   ⚓⚓⚓⚓⚓⚓⚓   ",
    "  ⚓         ⚓  ",
    "  ⚓    ⚓    ⚓  ",
    "  ⚓    ⚓    ⚓  ",
    "   ⚓   ⚓⚓   ⚓   ",
    "    ⚓  ⚓⚓  ⚓    ",
    "     ⚓ ⚓⚓ ⚓     ",
    "      ⚓⚓⚓⚓      ",
    "     ⚓⚓⚓⚓⚓⚓     ",
];

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

/// Boot splash state. Kept in a struct so a later slice can swap in a
/// different view without touching the loop.
struct SplashState {
    start: Instant,
    spinner: Spinner,
}

impl SplashState {
    fn new(theme: &Theme) -> Self {
        Self {
            start: Instant::now(),
            spinner: Spinner::new(theme.symbols.spinner_frames.clone()),
        }
    }
}

/// Linear interpolation accent → text for the logo gradient. Titanium's
/// tokens are all RGB; `Index`/`Default` endpoints (custom themes) fall back
/// to the accent unchanged rather than guessing at a ramp.
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

/// Renders one splash frame: the rounded panel, the partially drawn-in
/// anchor logo, tagline, version, and the status spinner + label.
fn draw_splash(frame: &mut Frame, theme: &Theme, splash: &SplashState, now: Instant) {
    let elapsed = now.duration_since(splash.start);
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();
    let text = colors.text().to_ratatui();
    let muted = colors.muted().to_ratatui();
    let success = colors.success().to_ratatui();

    // Fill the whole screen first so the splash sits on the theme's bg
    // instead of the terminal default (a Block styles its entire area).
    frame.render_widget(
        Block::default().style(Style::default().bg(bg)),
        frame.area(),
    );

    // --- content lines -------------------------------------------------
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::default()); // top padding

    // Draw-in: reveal rows top-down over DRAW_IN. The gradient maps the
    // *drawn* portion accent → text, so the accent→text boundary sweeps
    // downward with the reveal instead of sitting fixed.
    let progress = (elapsed.as_secs_f64() / DRAW_IN.as_secs_f64()).clamp(0.0, 1.0);
    let visible = (progress * ANCHOR_ART.len() as f64).round() as usize;
    for (i, row) in ANCHOR_ART.iter().enumerate() {
        if i < visible {
            let t = if visible > 1 {
                i as f64 / (visible - 1) as f64
            } else {
                0.0
            };
            let color = lerp_color(colors.accent(), colors.text(), t).to_ratatui();
            lines.push(Line::from(Span::styled(*row, Style::default().fg(color))));
        } else {
            // Blank placeholder keeps the panel geometry stable while the
            // logo draws in — the panel doesn't "grow" under the user.
            lines.push(Line::default());
        }
    }

    lines.push(Line::default()); // gap before the tagline
    lines.push(Line::from(Span::styled(
        "curated torrents straight from your terminal",
        Style::default().fg(text),
    )));
    lines.push(Line::from(Span::styled(
        "harbour v0.1.0",
        Style::default().fg(muted),
    )));

    lines.push(Line::default()); // gap before the status line

    // Status line: spinner + label. The label flips muted "loading…" →
    // success "ready — press q to quit" once the engine would have
    // initialized; spinner and label share a color so the line reads as one
    // transitioning element.
    let ready = elapsed >= READY_AFTER;
    let (label, status_color) = if ready {
        ("ready — press q to quit", success)
    } else {
        ("loading…", muted)
    };
    let mut status = Line::default();
    status.push_span(Span::styled(
        splash.spinner.current().to_string(),
        Style::default().fg(status_color),
    ));
    status.push_span(Span::styled(
        format!(" {label}"),
        Style::default().fg(status_color),
    ));
    lines.push(status);

    lines.push(Line::default()); // bottom padding

    // --- centered rounded panel ---------------------------------------
    // Sized to the content so the panel hugs the splash; clamped to the
    // frame so a tiny terminal clips instead of panicking on Rect math.
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let content_height = lines.len();
    let area = frame.area();
    let panel_w = (content_width + 2).min(area.width as usize) as u16;
    let panel_h = (content_height + 2).min(area.height as usize) as u16;
    let x = area.x + area.width.saturating_sub(panel_w) / 2;
    let y = area.y + area.height.saturating_sub(panel_h) / 2;
    let panel_area = Rect::new(x, y, panel_w, panel_h);

    // Border glyphs come from the theme (the unicode preset is the rounded
    // ╭╮╰╯ set); the vertical glyph is used on both sides.
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
        .title(Span::styled(" harbour ", Style::default().fg(accent)))
        .style(Style::default().bg(bg));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().bg(bg)),
        panel_area,
    );
}

/// Runs the TUI: enters the terminal (raw mode + alternate screen + hidden
/// cursor), renders the animated splash at 30fps until the user quits, and
/// restores the terminal on every exit path via [`TerminalGuard`] (normal
/// quit, errors, and panics — Drop runs during unwinding).
pub fn run(theme: Theme) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = TerminalGuard::enter()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    let mut splash = SplashState::new(&theme);

    loop {
        // Block until the next frame slot or the first pending input; then
        // drain the whole burst so N events in one tick produce one draw.
        if event::poll(ticker.next())? && drain_events()? {
            break;
        }
        let now = Instant::now();
        splash.spinner.advance(now, SPINNER_INTERVAL);
        // Each frame is one synchronized write — no flicker/tearing between
        // the border, logo, and status line (docs/design.md §2).
        anim::with_sync_output(|| {
            terminal.draw(|frame| draw_splash(frame, &theme, &splash, now))?;
            Ok(())
        })?;
    }
    Ok(())
}
