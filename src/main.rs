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
mod ensure_indexer;
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

use std::io;
use std::path::{Path, PathBuf};
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

/// Splits a panic payload into the message and location a crash log wants.
///
/// Panic payloads are `&dyn Any`; the two string shapes cover every real
/// panic (`panic!("x")`, `panic!(String)`) and anything else falls back to a
/// placeholder — the report must always be written.
fn panic_parts(info: &std::panic::PanicHookInfo) -> (String, Option<String>) {
    let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    };
    let location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()));
    (message, location)
}

/// Writes a crash report to `<root>/crash.log` (FR-09) and returns the path.
///
/// Pure — no panic hook, no terminal — so tests drive it with a temp root
/// while the hook just extracts strings via [`panic_parts`]. The directory is
/// created if missing: a crash can land before the app ever wrote its state
/// dir, and losing the report over a missing folder would defeat the point.
fn write_crash_log(root: &Path, message: &str, location: Option<&str>) -> io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let path = root.join("crash.log");
    let mut report = String::from("harbour crashed\n");
    report.push_str("message: ");
    report.push_str(message);
    report.push('\n');
    report.push_str("location: ");
    report.push_str(location.unwrap_or("unknown"));
    report.push('\n');
    std::fs::write(&path, report)?;
    Ok(path)
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
        // FR-09: a crash is a bug report, so persist it to the state dir —
        // terminal scrollback is not a log. The hook must never panic (a
        // panic inside a panic hook aborts instead of unwinding), so any
        // failure here is only an eprintln.
        let (message, location) = panic_parts(info);
        match write_crash_log(
            &crate::core::paths::state_dir(),
            &message,
            location.as_deref(),
        ) {
            Ok(path) => eprintln!("crash log written to {}", path.display()),
            Err(err) => eprintln!("could not write crash log: {err}"),
        }
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
    ensure_indexer::ensure_local_indexer().await;
    let code = match app::run(theme, initial).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The terminal has already been restored by the guard at this point,
            // so this reaches a usable shell rather than a wrecked alt-screen.
            eprintln!("harbour: {err}");
            ExitCode::FAILURE
        }
    };
    ensure_indexer::stop_local_indexer().await;
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch state dir unique to this test — parallel tests each get
    /// their own, so the crash log can never collide.
    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("harbour-crashlog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn crash_log_writes_message_and_location() {
        let root = temp_root("main");
        let path = write_crash_log(&root, "boom at the seam", Some("src/main.rs:42"))
            .expect("write crash log");
        assert_eq!(path, root.join("crash.log"));
        let content = std::fs::read_to_string(&path).expect("read crash log");
        assert!(content.contains("harbour crashed"), "header present");
        assert!(content.contains("boom at the seam"), "message present");
        assert!(content.contains("src/main.rs:42"), "location present");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_log_writes_without_a_location() {
        let root = temp_root("noloc");
        let path = write_crash_log(&root, "panic!", None).expect("write crash log");
        let content = std::fs::read_to_string(&path).expect("read crash log");
        assert!(content.contains("panic!"), "message present");
        assert!(content.contains("location: unknown"), "unknown stated");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_log_creates_the_state_dir_when_missing() {
        // A crash can land before the app ever wrote its state dir; the
        // writer must create it rather than failing.
        let root = temp_root("nested");
        let nested = root.join("deep").join("state");
        let path = write_crash_log(&nested, "early crash", None).expect("write crash log");
        assert!(path.is_file(), "crash log exists under the created dir");
        let _ = std::fs::remove_dir_all(&root);
    }
}
