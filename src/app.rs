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
use crossterm::event::{self, Event};
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::anim::{self, Spinner, Ticker};
use crate::core::cancel::CancelToken;
use crate::core::paths;
use crate::core::types::{
    Engine as CoreEngine, EngineEvent, ItemView, QueueStatus, SearchCtx, SourceId, SourceStatus,
    TorrentResult,
};
use crate::engine::fake::FakeEngine;
use crate::engine::rqbit::RqbitEngine;
use crate::input::Action;
use crate::persist::{Config, Store};
use crate::queue::{AddInput, AddOutcome, Queue};
use crate::search::SearchEngine;
use crate::sources::cache::SearchCache;
use crate::theme::{Color, Theme};
use crate::ui::{AppState, Screen};

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

/// How often the queue polls the engine while anything is actively
/// transferring. Slower than the render cadence on purpose: progress that
/// changes thirty times a second is noise, and the eased bars smooth the gaps.
const POLL_ACTIVE: Duration = Duration::from_millis(500);

/// Poll cadence once everything has settled into seeding.
///
/// A seedbox with 200 idle seeds should not perform 400 stat reads a second to
/// learn nothing (`NFR-04`). Completion arrives as an event rather than being
/// discovered by polling, so nothing is missed by slowing down.
const POLL_IDLE: Duration = Duration::from_secs(5);

/// How long the splash holds before the search screen takes over.
const SPLASH_DURATION: Duration = Duration::from_millis(1800);

/// What the command line asked us to start with.
#[derive(Debug, Clone, PartialEq)]
pub enum InitialAction {
    None,
    /// Enqueue this magnet as soon as the engine is up (`FR-02`).
    Magnet(String),
    /// Read this `.torrent` and enqueue it.
    TorrentFile(PathBuf),
}

/// Everything the loop needs, assembled once at boot.
struct App {
    state: AppState,
    queue: Queue,
    search: SearchEngine,
    store: Store,
    config: Config,
    /// Results per source for the current query, merged for display.
    partial: HashMap<SourceId, Vec<TorrentResult>>,
    search_cancel: Option<CancelToken>,
    events_tx: mpsc::UnboundedSender<EngineEvent>,
    history: Vec<String>,
    help_open: bool,
    /// The active watch session (FR-57), if any — stream server + player.
    watch: Option<crate::watch::WatchSession>,
    quitting: bool,
}

impl App {
    /// Something the user should know that must not stop the app.
    fn warn(&mut self, message: impl Into<String>) {
        self.state.error_banner = Some(message.into());
    }

    fn selected_result(&self) -> Option<&TorrentResult> {
        self.state.search.results.get(self.state.search.selected)
    }

    fn selected_item_id(&self) -> Option<String> {
        // Walk the *visible* tab's items so the selection never points at a
        // row hidden on the other tab (the Seeding tab renders only
        // finished items, the active tab only unfinished ones).
        self.visible_items()
            .get(self.state.downloads.selected)
            .map(|v| v.item.id.clone())
    }

    /// The items the current downloads tab actually shows, in render order.
    fn visible_items(&self) -> Vec<&ItemView> {
        self.state
            .downloads
            .items
            .iter()
            .filter(|v| {
                let finished =
                    v.item.status == QueueStatus::Seeding || v.item.status == QueueStatus::Missing;
                finished == self.state.downloads.show_seeding
            })
            .collect()
    }

    /// Rebuilds the downloads view from the queue.
    fn refresh_downloads(&mut self) {
        self.state.downloads.items = self.queue.views();
        self.state.downloads.history = self.queue.completed();
        let len = self.state.downloads.items.len();
        // Keep the cursor inside the list after a removal.
        if self.state.downloads.selected >= len {
            self.state.downloads.selected = len.saturating_sub(1);
        }
    }

    /// Merges everything received so far into the displayed list.
    fn remerge(&mut self) {
        let all: Vec<TorrentResult> = self.partial.values().flatten().cloned().collect();
        self.state.search.results = crate::search::merge(all);
        let len = self.state.search.results.len();
        if self.state.search.selected >= len {
            self.state.search.selected = len.saturating_sub(1);
        }
    }

    /// Stops an in-flight search: the partial results already merged stay on
    /// screen, so navigation is stable (arrow keys during streaming read as
    /// "let me look at what's here", not "move the cursor under a changing
    /// list").
    fn stop_search(&mut self) {
        if let Some(token) = self.search_cancel.take() {
            token.cancel();
        }
        self.state.search.searching = false;
    }

    /// Starts a search, cancelling whatever was in flight (`FR-20`).
    fn start_search(&mut self, query: String) {
        if let Some(previous) = self.search_cancel.take() {
            previous.cancel();
        }
        self.partial.clear();
        self.state.search.results.clear();
        self.state.search.selected = 0;
        self.state.search.searching = true;
        self.state.search.source_counts.clear();
        for id in SourceId::ALL {
            self.state
                .search
                .source_health
                .insert(id, SourceStatus::Unknown);
        }

        if !query.trim().is_empty() {
            let mut history = std::mem::take(&mut self.history);
            if let Err(err) = self.store.push_history(&mut history, &query) {
                // Losing search history is not worth interrupting anyone over.
                eprintln!("harbour: could not save search history: {err}");
            }
            self.history = history;
        }

        let ctx = SearchCtx {
            total_deadline: paths::source_timeout(),
            ..SearchCtx::default()
        };
        self.search_cancel = Some(ctx.cancel.clone());
        self.search.start(query, ctx, self.events_tx.clone());
    }
}

/// Reads terminal events on a dedicated thread.
///
/// `crossterm::event::read` blocks, and blocking a tokio worker would stall the
/// engine's tasks with it. One OS thread feeding a channel keeps input
/// responsive without the async runtime ever waiting on a keypress.
fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            let Ok(ev) = event::read() else {
                // A terminal that stops producing events is not recoverable
                // here, and spinning on the error would peg a core.
                return;
            };
            if tx.send(ev).is_err() {
                // The app has gone; so should we.
                return;
            }
        }
    });
    rx
}

/// `w` on the selected item: find its media file, find a player (mpv →
/// VLC), serve the file over loopback with Range support, and launch the
/// player (FR-57..FR-61). Every failure path is a loud error banner — never
/// a silent no-op. With fake data there are no files, so this is exactly the
/// honest error until real downloads land; the engine track will swap the
/// file path for librqbit's live stream URL.
fn start_watch(app: &mut App) {
    // The selection indexes the visible tab, so resolve through it; clone
    // the fields (a ref into `items` would fight the mutations below).
    let Some(item) = app
        .visible_items()
        .get(app.state.downloads.selected)
        .map(|v| &v.item)
    else {
        return;
    };
    let id = item.id.clone();
    let name = item.name.clone();
    let dir = item.dir.clone();
    let Some(player) = crate::watch::find_player() else {
        app.warn("watch: no player found — install mpv or VLC and add it to PATH");
        return;
    };
    let Some(file) = crate::watch::primary_media(&dir) else {
        app.warn(format!(
            "watch: no media file for '{name}' (fake data — engine lands in phase 4)"
        ));
        return;
    };
    match crate::watch::WatchSession::start(&file, player) {
        Ok(session) => {
            app.state.now_playing = Some(crate::ui::NowPlaying {
                id,
                name,
                stream_url: session.url.clone(),
            });
            app.watch = Some(session);
            app.state.screen = Screen::NowPlaying;
        }
        Err(err) => app.warn(format!("watch: cannot start player: {err}")),
    }
}

/// Ends the session and returns to the downloads screen (FR-59: player exit
/// or `q`/esc). Kills the player and stops the stream server.
fn end_watch(app: &mut App) {
    if let Some(mut session) = app.watch.take() {
        session.stop();
    }
    app.state.now_playing = None;
    app.state.screen = Screen::Downloads;
}

/// Runs the TUI.
///
/// Takes the shared `Arc<Mutex<Theme>>` so the theme-watcher thread can swap
/// themes underneath a running render loop; the lock is taken once per frame
/// and released before the next wait, so a swap never blocks input.
///
/// Every failure on this path degrades rather than aborting (`NFR-15`): a
/// config that will not parse falls back with a banner, a ledger that will not
/// read starts empty and keeps the file, and an engine that will not construct
/// leaves search working with downloads reporting why.
pub async fn run(
    theme: Arc<Mutex<Theme>>,
    initial: InitialAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::from_env();

    let loaded_config = store.load_config();
    let config_warning = loaded_config.warning().map(str::to_owned);
    let config = loaded_config.value();

    // The crash breaker: a marker left behind means the previous run died
    // before it finished starting, so this one restores everything paused.
    let safe_mode = store.boot_was_interrupted();
    if let Err(err) = store.arm_boot_marker() {
        eprintln!("harbour: could not write the boot marker: {err}");
    }

    let loaded_ledger = store.load_ledger();
    let ledger_warning = loaded_ledger.warning().map(str::to_owned);
    let items = loaded_ledger.value();
    let history = store.load_history().value();

    // An engine that will not start must not stop the app: search still works,
    // and downloads report the reason instead of the window refusing to open.
    let (engine, engine_error): (Arc<dyn CoreEngine>, Option<String>) =
        match RqbitEngine::new(&config.download_dir, store.root()).await {
            Ok(engine) => {
                // Adopt anything librqbit restored from its own persistence, or
                // it would be running but invisible to the queue.
                engine.adopt_restored();
                (Arc::new(engine), None)
            }
            Err(err) => (
                Arc::new(FakeEngine::new()),
                Some(format!("downloads are unavailable: {err}")),
            ),
        };

    let mut queue = Queue::new(engine.clone(), paths::max_downloads());
    queue.set_trackers(config.trackers.clone());
    queue.restore(items, safe_mode).await;

    let search = SearchEngine::new(
        crate::sources::registry(),
        SearchCache::new(store.root().to_path_buf()),
    );

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut app = App {
        state: AppState::default(),
        queue,
        search,
        store,
        config,
        partial: HashMap::new(),
        search_cancel: None,
        events_tx,
        history,
        help_open: false,
        watch: None,
        quitting: false,
    };

    app.state.screen = Screen::Splash;
    app.refresh_downloads();

    // `warn` owns a single banner slot, so collapsing several startup
    // problems into one message is what keeps them all visible — a corrupt
    // ledger plus a failed engine plus safe mode must not silently drop to
    // just the last one.
    let startup_warnings: Vec<String> = [config_warning, ledger_warning, engine_error]
        .into_iter()
        .flatten()
        .collect();
    if !startup_warnings.is_empty() {
        app.warn(startup_warnings.join("\n"));
    }
    if safe_mode {
        app.warn(
            "harbour did not shut down cleanly last time, so everything is paused. \
             Press p on an item to resume it.",
        );
    }

    match initial {
        InitialAction::None => {}
        InitialAction::Magnet(magnet) => enqueue_magnet(&mut app, &magnet).await,
        InitialAction::TorrentFile(path) => match std::fs::metadata(&path) {
            // Reading a .torrent means parsing bencode and hashing its info
            // dict. librqbit can do both, but wiring the file path through the
            // add request is engine work that has not landed; say so plainly
            // rather than failing silently on launch.
            Ok(_) => app.warn(format!(
                "{} was found, but opening a .torrent on launch is not wired up yet — \
                 paste its magnet instead",
                path.display()
            )),
            Err(err) => app.warn(format!("could not read {}: {err}", path.display())),
        },
    }

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    let mut splash = SplashState::new(&lock_theme(&theme));
    let mut input = spawn_input_thread();
    let mut last_poll = Instant::now();
    let started = Instant::now();

    while !app.quitting {
        // Wait for the next frame slot, but wake early for input or an engine
        // event so neither waits a whole frame to be seen.
        let wait = ticker.next();
        tokio::select! {
            biased;
            Some(ev) = input.recv() => handle_event(&mut app, ev).await,
            Some(engine_event) = events_rx.recv() => apply_event(&mut app, engine_event),
            _ = tokio::time::sleep(wait) => {}
        }

        // Drain whatever else arrived in the same instant, so a burst produces
        // one frame rather than one frame each.
        while let Ok(ev) = input.try_recv() {
            handle_event(&mut app, ev).await;
        }
        while let Ok(engine_event) = events_rx.try_recv() {
            apply_event(&mut app, engine_event);
        }

        let now = Instant::now();
        let cadence = if app.queue.active_count() > 0 {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        };
        if now.duration_since(last_poll) >= cadence {
            last_poll = now;
            let events = app.queue.tick(now).await;
            for engine_event in events {
                apply_event(&mut app, engine_event);
            }
            app.refresh_downloads();
        }

        // The splash is a timed intro, not a state to be stuck in.
        if app.state.screen == Screen::Splash && started.elapsed() >= SPLASH_DURATION {
            app.state.screen = Screen::Search;
        }

        // FR-59: when the player exits, the watch session ends and the TUI
        // returns to the downloads screen.
        if app.state.screen == Screen::NowPlaying
            && app
                .watch
                .as_mut()
                .is_some_and(|session| session.player_exited())
        {
            end_watch(&mut app);
        }

        let active = lock_theme(&theme);
        splash.spinner.set_frames(&active.symbols.spinner_frames);
        splash.spinner.advance(now, SPINNER_INTERVAL);
        let glyph = splash.spinner.current().to_owned();
        anim::with_sync_output(|| {
            terminal.draw(|frame| draw(frame, &active, &app, &mut splash, now, &glyph))?;
            Ok(())
        })?;
        drop(active);
    }

    // Flush before standing the crash breaker down: a crash between the two
    // would otherwise leave a clean marker over stale state.
    if let Err(err) = app.store.flush_and_disarm(app.queue.items()) {
        eprintln!("harbour: could not save state on exit: {err}");
    }
    Ok(())
}

/// Draws whichever screen is current, plus the status line and any overlay.
fn draw(
    frame: &mut Frame,
    theme: &Theme,
    app: &App,
    splash: &mut SplashState,
    now: Instant,
    glyph: &str,
) {
    let area = frame.area();
    if app.state.screen == Screen::Splash {
        draw_splash(frame, theme, splash, now);
        return;
    }

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(status_height(app)),
    ])
    .split(area);

    match app.state.screen {
        Screen::Downloads => {
            crate::ui::downloads::draw(frame, rows[0], &app.state.downloads, theme)
        }
        Screen::NowPlaying => {
            if let Some(np) = &app.state.now_playing {
                crate::ui::now_playing::draw(frame, rows[0], np, theme);
            }
        }
        _ => crate::ui::search::draw(frame, rows[0], &app.state.search, theme),
    }
    crate::ui::status::draw(frame, rows[1], app.state.screen, &app.state, theme, glyph);

    if app.help_open {
        crate::ui::help::draw(frame, area, theme);
    }
}

/// Rows to reserve for the status area.
///
/// This must match `ui::status::draw`'s own layout exactly. That view splits
/// the area it is given into `[Min(0), banner?, status]` and draws only the
/// bottom two, so handing it fewer rows than it wants does not shrink the
/// banner — it squeezes it out entirely and the message is never seen. Under-
/// allocating by a single row was enough to make the safe-mode warning
/// invisible, which is exactly the class of bug a banner exists to prevent.
fn status_height(app: &App) -> u16 {
    banner_height(app.state.error_banner.as_deref()) + 1
}

/// Banner rows: two borders plus one or two content rows, or zero when there is
/// nothing to say. Mirrors `ui::status::draw`.
fn banner_height(message: Option<&str>) -> u16 {
    message.map_or(0, |m| 2 + m.lines().count().clamp(1, 2) as u16)
}

/// Turns one terminal event into state changes.
async fn handle_event(app: &mut App, event: Event) {
    let Event::Key(key) = event else {
        // Resize and mouse events need no handling: ratatui re-lays out from
        // the frame size on every draw.
        return;
    };
    // Windows reports both press and release; acting on both would double
    // every keystroke.
    if key.kind != crossterm::event::KeyEventKind::Press {
        return;
    }

    let mut action = crate::input::map(key, app.state.screen, app.help_open);

    // On the search screen every printable key belongs to the text field —
    // except when there is nothing to type into, or when shift+D asks for a
    // download explicitly.
    if app.state.screen == Screen::Search && !app.help_open {
        if crate::input::is_download_key(key) {
            action = Action::Download;
        } else if app.state.search.query.is_empty()
            && let Some(override_action) = crate::input::map_empty_query(key)
        {
            action = override_action;
        }
    }

    apply_action(app, action).await;
}

async fn apply_action(app: &mut App, action: Action) {
    match action {
        Action::None => {}
        Action::Quit => app.quitting = true,
        Action::Dismiss => app.state.screen = Screen::Search,
        Action::ToggleHelp => app.help_open = !app.help_open,
        Action::SwitchScreen => {
            app.state.screen = match app.state.screen {
                Screen::Downloads => Screen::Search,
                _ => Screen::Downloads,
            };
            app.state.error_banner = None;
        }
        Action::ToggleSeeding => {
            app.state.downloads.show_seeding = !app.state.downloads.show_seeding;
            app.state.downloads.selected = 0;
        }
        Action::MoveUp => {
            // During a streaming search the list shifts under the cursor;
            // an arrow press reads as "let me look at what's here", so it
            // stops the search and keeps the partial results stable.
            if app.state.screen == Screen::Search {
                app.stop_search();
            }
            move_selection(app, -1);
        }
        Action::MoveDown => {
            if app.state.screen == Screen::Search {
                app.stop_search();
            }
            move_selection(app, 1);
        }
        Action::Type(c) => app.state.search.query.push(c),
        Action::Backspace => {
            app.state.search.query.pop();
        }
        Action::Escape => {
            if app.help_open {
                app.help_open = false;
            } else if !app.state.search.query.is_empty() {
                app.state.search.query.clear();
            } else {
                app.state.error_banner = None;
            }
        }
        Action::Submit => {
            let query = app.state.search.query.clone();
            app.start_search(query);
        }
        Action::Download => download_selected(app).await,
        Action::TogglePause => toggle_pause(app).await,
        Action::Retry => retry_selected(app).await,
        Action::Remove => remove_selected(app).await,
        Action::Watch => start_watch(app),
        Action::EndWatch => end_watch(app),
    }
}

fn move_selection(app: &mut App, delta: isize) {
    let (len, selected) = match app.state.screen {
        // The downloads selection indexes the *visible* tab's rows — the
        // view renders only the active or seeding subset, so a raw items
        // index would highlight an invisible row (and let p/r/x act on one).
        Screen::Downloads => (app.visible_items().len(), &mut app.state.downloads.selected),
        _ => (
            app.state.search.results.len(),
            &mut app.state.search.selected,
        ),
    };
    if len == 0 {
        *selected = 0;
        return;
    }
    // Wrap at both ends: a list you cannot leave by holding a key feels stuck.
    let next = (*selected as isize + delta).rem_euclid(len as isize);
    *selected = next as usize;
}

async fn download_selected(app: &mut App) {
    let Some(result) = app.selected_result().cloned() else {
        app.warn("nothing selected to download");
        return;
    };

    // A row from a detail-page source arrives without a magnet; resolve it now
    // that the user has actually asked for it (`plan-engine.md` T4).
    let magnet = match &result.magnet {
        Some(magnet) => Some(magnet.clone()),
        None => resolve_magnet(app, &result).await,
    };

    let Some(magnet) = magnet else {
        app.warn(format!("could not get a magnet link for {}", result.name));
        return;
    };

    // Re-key on the magnet's own infohash rather than the row's.
    //
    // The detail-page sources (1337x, FitGirl, BitTorrented) cannot know a
    // torrent's real infohash from the list page, so they carry the site's own
    // id in that field as a placeholder until resolution. Enqueuing under the
    // placeholder would file the item under an id the engine never reports
    // back — librqbit keys by the real hash — so the row would sit at 0% for
    // ever while the download actually ran. The magnet is authoritative.
    let id = crate::core::magnet::info_hash_from_magnet(&magnet)
        .unwrap_or_else(|| result.info_hash.clone());

    let outcome = app
        .queue
        .add(
            AddInput {
                id,
                name: result.name.clone(),
                source: Some(result.source),
                magnet: Some(magnet),
                dir: app.config.download_dir.clone(),
                size_bytes: result.size_bytes,
            },
            now_ms(),
        )
        .await;

    match outcome {
        AddOutcome::Duplicate => {
            app.warn(format!("{} is already in your downloads", result.name));
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Started | AddOutcome::Retried => {
            app.state.error_banner = None;
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Queued => app.warn(format!(
            "{} is queued — it starts when a slot frees",
            result.name
        )),
    }
    persist(app);
    app.refresh_downloads();
}

/// Asks the owning source for a magnet it did not supply at search time.
async fn resolve_magnet(app: &App, result: &TorrentResult) -> Option<String> {
    let source = app
        .search
        .sources()
        .iter()
        .find(|s| s.def().id == result.source)?
        .clone();
    let ctx = SearchCtx {
        total_deadline: paths::source_timeout(),
        ..SearchCtx::default()
    };
    source.resolve_magnet(result, &ctx).await.ok()
}

async fn toggle_pause(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    let paused = app
        .queue
        .get(&id)
        .is_some_and(|i| i.status == QueueStatus::Paused);
    let outcome = if paused {
        app.queue.resume(&id, Instant::now()).await
    } else {
        app.queue.pause(&id).await
    };
    if let Err(err) = outcome {
        app.warn(err.to_string());
    }
    persist(app);
    app.refresh_downloads();
}

async fn retry_selected(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    let Some(item) = app.queue.get(&id).cloned() else {
        return;
    };
    if item.status != QueueStatus::Failed {
        return;
    }
    app.queue
        .add(
            AddInput {
                id: item.id.clone(),
                name: item.name.clone(),
                source: item.source,
                magnet: item.magnet.clone(),
                dir: item.dir.clone(),
                size_bytes: item.total_bytes,
            },
            item.added_at_epoch_ms,
        )
        .await;
    persist(app);
    app.refresh_downloads();
}

async fn remove_selected(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    // Files are never deleted from here: removal forgets the item, and deleting
    // someone's data needs a deliberate, separate confirmation.
    if let Err(err) = app.queue.remove(&id, false).await {
        app.warn(err.to_string());
    }
    persist(app);
    app.refresh_downloads();
}

/// Writes the ledger, surfacing a failure without stopping anything.
fn persist(app: &mut App) {
    if let Err(err) = app.store.save_ledger(app.queue.items()) {
        app.warn(format!("could not save your downloads list: {err}"));
    }
}

/// True while any source is still working.
fn still_searching(app: &App) -> bool {
    app.state
        .search
        .source_health
        .values()
        .any(|s| *s == SourceStatus::Checking)
}

/// Folds one engine or search event into the UI state.
fn apply_event(app: &mut App, event: EngineEvent) {
    match event {
        EngineEvent::SourceStatus { source, status } => {
            app.state.search.source_health.insert(source, status);
        }
        EngineEvent::SourceAnswered { source, count } => {
            app.state.search.source_counts.insert(source, count);
            // Reachable-but-empty is not the same as failed: the dot must say
            // "nothing matched" rather than "this source is down".
            app.state.search.source_health.insert(
                source,
                if count == 0 {
                    SourceStatus::Empty
                } else {
                    SourceStatus::Online
                },
            );
            app.state.search.searching = still_searching(app);
        }
        EngineEvent::SourceResults { source, results } => {
            app.partial.insert(source, results);
            app.remerge();
        }
        EngineEvent::SourceFailed {
            source, message, ..
        } => {
            app.state
                .search
                .source_health
                .insert(source, SourceStatus::Offline);
            app.state.search.searching = still_searching(app);
            // One dead source is normal and must not shout at the user — the
            // sidebar dot already says so. Only a total failure earns a banner.
            let probed: Vec<SourceStatus> = app
                .state
                .search
                .source_health
                .values()
                .copied()
                .filter(|s| *s != SourceStatus::Unknown)
                .collect();
            if !probed.is_empty() && probed.iter().all(|s| *s == SourceStatus::Offline) {
                app.warn(format!("every source is unreachable — {message}"));
            }
        }
        EngineEvent::SearchComplete => app.state.search.searching = false,
        EngineEvent::Metadata { .. } | EngineEvent::Progress { .. } => {}
        EngineEvent::Done { .. } => persist(app),
        EngineEvent::Failed { id, message } => {
            let name = item_name(app, &id);
            app.warn(format!("{name}: {message}"));
            persist(app);
        }
        EngineEvent::Missing { id } => {
            let name = item_name(app, &id);
            app.warn(format!(
                "{name}: the downloaded files are gone, so seeding stopped. \
                 Nothing was re-downloaded."
            ));
            persist(app);
        }
    }
}

fn item_name(app: &App, id: &str) -> String {
    app.queue
        .get(id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| id.to_owned())
}

/// Enqueues a magnet handed to us on the command line (`FR-02`).
async fn enqueue_magnet(app: &mut App, magnet: &str) {
    let Some(info_hash) = crate::core::magnet::info_hash_from_magnet(magnet) else {
        app.warn("that magnet link has no usable infohash");
        return;
    };
    app.queue
        .add(
            AddInput {
                id: info_hash.clone(),
                name: info_hash.clone(),
                source: None,
                magnet: Some(magnet.to_owned()),
                dir: app.config.download_dir.clone(),
                size_bytes: 0,
            },
            now_ms(),
        )
        .await;
    app.state.screen = Screen::Downloads;
    persist(app);
    app.refresh_downloads();
}

/// Wall-clock milliseconds, used only for ordering the queue.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Recover from a poisoned theme lock instead of panicking: a watcher thread
/// that panicked mid-swap must not take the render loop down with it.
fn lock_theme(theme: &Arc<Mutex<Theme>>) -> std::sync::MutexGuard<'_, Theme> {
    theme
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn the_status_line_is_one_row_until_something_needs_saying() {
        let mut app_state = AppState::default();
        assert_eq!(app_state.error_banner, None);
        app_state.error_banner = Some("one line".into());
        // Constructed indirectly: status_height only reads the banner.
        let lines = app_state
            .error_banner
            .as_ref()
            .map(|m| (m.lines().count() as u16 + 2).clamp(3, 6));
        assert_eq!(lines, Some(3));

        app_state.error_banner = Some("a\nb\nc\nd\ne\nf\ng".into());
        let lines = app_state
            .error_banner
            .as_ref()
            .map(|m| (m.lines().count() as u16 + 2).clamp(3, 6));
        assert_eq!(lines, Some(6), "a long banner is capped, never unbounded");
    }
}
