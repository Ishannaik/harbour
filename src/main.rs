mod anim;
mod app;
mod cli;
mod core;
mod engine;
mod fake;
mod persist;
mod queue;
mod theme;
mod theme_watch;
mod ui;

use std::process::ExitCode;

/// Parses the command line, then hands control to the TUI.
///
/// Returns [`ExitCode`] rather than `Result` so a bad argument exits non-zero
/// with a readable message instead of a `Debug`-formatted error (`FR-02`), and
/// `--help`/`--version` exit 0 without ever entering the alt-screen
/// (`FR-03`/`FR-04`).
fn main() -> ExitCode {
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
        // The initial action is parsed here and handed to the app; wiring it to
        // an actual enqueue lands with the engine adapter (E2).
        cli::Command::Run | cli::Command::RunWithMagnet(_) | cli::Command::RunWithTorrent(_) => {
            run_tui()
        }
    }
}

fn run_tui() -> ExitCode {
    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection) — fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    // The theme lives behind an `Arc<Mutex<Theme>>` for the process lifetime:
    // the theme-watcher thread swaps themes at runtime, so the render loop and
    // every view lock the same shared handle instead of caching a copy.
    let theme = std::sync::Arc::new(std::sync::Mutex::new(theme::Theme::titanium()));
    match app::run(theme) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The terminal has already been restored by the guard at this point,
            // so this reaches a usable shell rather than a wrecked alt-screen.
            eprintln!("harbour: {err}");
            ExitCode::FAILURE
        }
    }
}
