//! harbour entry point: CLI, terminal lifecycle, theme loading, and animation loop.

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use harbour::anim::{Cadence, Spinner};
use harbour::cli::Cli;
use harbour::term::{self, TerminalGuard};
use harbour::theme::{self, ThemeWatcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. CLI parsing (--help, --version handled automatically by clap)
    let cli = Cli::parse_args();
    // Positional target is stored for phase 4, unused in phase 1 skeleton.
    let _ = cli.target;

    // 2. Install panic hook restoring terminal and logging to file before panic prints
    term::install_panic_hook();

    // 3. Detect color mode from environment
    let color_mode = theme::detect_color_mode();

    // 4. Load initial theme (titanium fallback on any failure)
    let (mut current_theme, _) = theme::load_theme_with_fallback("titanium", color_mode);
    let mut theme_watcher = ThemeWatcher::new(&current_theme.name);

    // Verify terminal capability if interactive
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("harbour: interactive TUI requires a terminal TTY");
        return Ok(());
    }

    // 5. Enter terminal (raw mode, alternate screen, hidden cursor)
    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // 6. Animation cadence and spinner initialization
    let mut cadence = Cadence::new();
    let mut spinner = Spinner::new(current_theme.symbols.spinner_frames.clone());

    let mut last_tick = Instant::now();
    let mut should_exit = false;
    // Last live-reload failure, shown in the status line: stderr is unusable in raw mode.
    let mut theme_warning: Option<String> = None;

    // Fast wake-up interval for polling inputs and advancing animation
    let mut tick_timer = tokio::time::interval(Duration::from_millis(8));

    // Initial render request
    cadence.request_render();

    // 7. Event & Render loop
    while !should_exit {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                should_exit = true;
            }
            _ = tick_timer.tick() => {
                let now = Instant::now();
                let dt = now.duration_since(last_tick);
                last_tick = now;

                // Handle keyboard inputs non-blockingly
                while crossterm::event::poll(Duration::ZERO)? {
                    if let crossterm::event::Event::Key(key) = crossterm::event::read()?
                        && (key.code == crossterm::event::KeyCode::Char('q')
                            || (key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                && key.code == crossterm::event::KeyCode::Char('c')))
                    {
                        should_exit = true;
                        break;
                    }
                }

                if should_exit {
                    break;
                }

                // Poll for live theme reload (~1s cadence)
                if let Some(reload_res) = theme_watcher.poll(dt, color_mode) {
                    match reload_res {
                        Ok(new_theme) => {
                            current_theme = new_theme;
                            theme_warning = None;
                            spinner = Spinner::new(current_theme.symbols.spinner_frames.clone());
                            cadence.request_render();
                        }
                        Err(err) => {
                            theme_warning = Some(format!("theme reload failed: {err}"));
                            cadence.request_render();
                        }
                    }
                }

                // Advance spinner and request render
                if spinner.tick(dt) {
                    cadence.request_render();
                }

                // Always request continuous animation during phase 1
                cadence.request_render();

                // Draw frame when cadence tick elapses
                if cadence.tick(dt) {
                    let draw_start = Instant::now();

                    let mut stdout = io::stdout();
                    term::begin_sync_update(&mut stdout)?;

                    terminal.draw(|f| {
                        let area = f.area();

                        // Canvas background
                        let bg_block = Block::default().style(Style::default().bg(current_theme.bg()));
                        f.render_widget(bg_block, area);

                        // Status line at the bottom
                        if area.height > 0 {
                            let status_area = Rect {
                                x: area.x,
                                y: area.y + area.height - 1,
                                width: area.width,
                                height: 1,
                            };

                            let stats = cadence.stats();
                            let status_style = Style::default()
                                .bg(current_theme.status_line_bg())
                                .fg(current_theme.text());

                            let spinner_style = Style::default().fg(current_theme.accent());

                            let mut spans = vec![
                                Span::raw(" "),
                                Span::styled(spinner.current_frame(), spinner_style),
                                Span::raw("  "),
                                Span::styled(
                                    "harbour",
                                    Style::default()
                                        .fg(current_theme.text())
                                        .add_modifier(Modifier::BOLD),
                                ),
                                Span::raw("  "),
                                Span::styled(
                                    format!("{:.1} fps", stats.fps),
                                    Style::default().fg(current_theme.muted()),
                                ),
                            ];
                            if let Some(warning) = &theme_warning {
                                spans.push(Span::raw("  "));
                                spans.push(Span::styled(
                                    warning.as_str(),
                                    Style::default().fg(current_theme.warning()),
                                ));
                            }
                            let line = Line::from(spans);

                            let status_bar = Paragraph::new(line).style(status_style);
                            f.render_widget(status_bar, status_area);
                        }
                    })?;

                    term::end_sync_update(&mut stdout)?;

                    let draw_cost = draw_start.elapsed();
                    cadence.record_frame(draw_cost);
                }
            }
        }
    }

    // 8. Unconditional terminal restore
    guard.restore()?;

    Ok(())
}
