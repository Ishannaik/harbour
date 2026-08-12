// Code-quality policy (FR-62): test code keeps its unwraps; shipped code
// cannot panic on a recoverable error. Scoped to non-test builds so the
// suite's legitimate `unwrap`/`expect` assertions keep compiling.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::dbg_macro,
        clippy::todo
    )
)]

mod anim;
mod app;
mod cli;
mod core;
mod engine;
mod fake;
mod input;
mod persist;
mod queue;
mod search;
mod sources;
mod theme;
mod theme_watch;
mod ui;
mod watch;

use std::process::ExitCode;

use crate::app::InitialAction;

/// Parses the command line, then hands control to the TUI.
///
/// Returns [`ExitCode`] rather than `Result` so a bad argument exits non-zero
/// with a readable message instead of a `Debug`-formatted error (`FR-02`), and
/// `--help`/`--version` exit 0 without ever entering the alt-screen
/// (`FR-03`/`FR-04`).
#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        cli::Command::Help => {
            print!("{}", cli::HELP);
            ExitCode::SUCCESS
        }
        cli::Command::Version => {
            println!("{}", cli::version_line());
            ExitCode::SUCCESS
        }
        cli::Command::Invalid { message } => {
            eprintln!("harbour: {message}\n");
            eprintln!("try `harbour --help`");
            ExitCode::FAILURE
        }
        cli::Command::Run => run_tui(InitialAction::None).await,
        cli::Command::RunWithMagnet(magnet) => run_tui(InitialAction::Magnet(magnet)).await,
        cli::Command::RunWithTorrent(path) => run_tui(InitialAction::TorrentFile(path)).await,
    }
}

async fn run_tui(initial: InitialAction) -> ExitCode {
    // A TUI panic must not leave the user's shell in raw mode with no
    // cursor. The TerminalGuard also restores on unwind, but the hook runs
    // first so the panic message itself is readable and the shell is
    // declared intact — a crash is a bug report, not a broken terminal.
    std::panic::set_hook(Box::new(|info| {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        let _ = crossterm::terminal::disable_raw_mode();
        eprintln!("\nharbour crashed: {info}");
        eprintln!("your terminal was restored — please report this line");
    }));

    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection) — fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    // The theme lives behind an `Arc<Mutex<Theme>>` for the process lifetime:
    // the theme-watcher thread swaps themes at runtime, so the render loop and
    // every view lock the same shared handle instead of caching a copy.
    let theme = std::sync::Arc::new(std::sync::Mutex::new(theme::Theme::titanium()));
    // Live theme reload (docs/theming.md §Custom themes): edits to the active
    // theme file under the themes dir swap in at the next render frame.
    theme_watch::spawn_theme_watcher(theme.clone());
    match app::run(theme, initial).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The terminal has already been restored by the guard at this point,
            // so this reaches a usable shell rather than a wrecked alt-screen.
            eprintln!("harbour: {err}");
            ExitCode::FAILURE
        }
    }
}
