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
    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection) — fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    // The theme lives behind an `Arc<Mutex<Theme>>` for the process lifetime:
    // the theme-watcher thread swaps themes at runtime, so the render loop and
    // every view lock the same shared handle instead of caching a copy.
    let theme = std::sync::Arc::new(std::sync::Mutex::new(theme::Theme::titanium()));
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
