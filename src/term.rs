//! Terminal lifecycle, DEC 2026 synchronized output, panic restoration,
//! and crash logging.

use std::fs;
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// DEC 2026 Begin Synchronized Update sequence.
pub const BSU: &[u8] = b"\x1b[?2026h";

/// DEC 2026 End Synchronized Update sequence.
pub const ESU: &[u8] = b"\x1b[?2026l";

/// Write DEC 2026 Begin Synchronized Update sequence.
pub fn begin_sync_update<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(BSU)?;
    w.flush()
}

/// Write DEC 2026 End Synchronized Update sequence.
pub fn end_sync_update<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(ESU)?;
    w.flush()
}

/// Enter raw mode, enter alternate screen, and hide cursor.
pub fn enter_terminal() -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;
    Ok(())
}

/// Unconditionally restore terminal state (show cursor, leave alt-screen, disable raw mode).
///
/// Safe to call multiple times, from normal exit, signals, or panic hooks.
pub fn restore_terminal() -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = stdout.flush();
    Ok(())
}

/// RAII guard ensuring terminal restoration on drop.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enter alternate screen and raw mode, acquiring the guard.
    pub fn enter() -> std::io::Result<Self> {
        enter_terminal()?;
        Ok(Self { active: true })
    }

    /// Explicitly restore terminal before drop.
    pub fn restore(&mut self) -> std::io::Result<()> {
        if self.active {
            self.active = false;
            restore_terminal()
        } else {
            Ok(())
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Write crash log details to `<logs_dir>/crash-<timestamp>.log`.
pub fn write_crash_log_file(
    logs_dir: &Path,
    payload: &str,
    location: Option<(&str, u32, u32)>,
    timestamp: u64,
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(logs_dir)?;
    let log_path = logs_dir.join(format!("crash-{timestamp}.log"));
    let loc_str = match location {
        Some((file, line, col)) => format!("{file}:{line}:{col}"),
        None => "unknown location".to_string(),
    };
    let content = format!(
        "harbour crash log\n\
         timestamp: {timestamp}\n\
         location: {loc_str}\n\
         payload: {payload}\n"
    );
    fs::write(&log_path, content)?;
    Ok(log_path)
}

/// Extract panic information and write to `<logs_dir>/crash-<unix_ts>.log`.
pub fn write_crash_log(logs_dir: &Path, info: &PanicHookInfo) -> std::io::Result<PathBuf> {
    let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
        *s
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.as_str()
    } else {
        "unknown panic payload"
    };

    let location = info.location().map(|l| (l.file(), l.line(), l.column()));

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    write_crash_log_file(logs_dir, payload, location, ts)
}

/// Install panic hook restoring the terminal and writing crash logs before panic output.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 1. Unconditionally restore terminal first
        let _ = restore_terminal();

        // 2. Write crash log to ~/.harbour/logs/crash-<unix_ts>.log
        let logs_dir = crate::theme::get_harbour_dir().join("logs");
        let _ = write_crash_log(&logs_dir, info);

        // 3. Print panic using default hook
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_update_sequences() {
        let mut buf = Vec::new();
        begin_sync_update(&mut buf).unwrap();
        assert_eq!(buf, b"\x1b[?2026h");

        buf.clear();
        end_sync_update(&mut buf).unwrap();
        assert_eq!(buf, b"\x1b[?2026l");
    }

    #[test]
    fn test_write_crash_log_file() {
        let tmp_dir = std::env::temp_dir().join("harbour_crash_test");
        let _ = fs::create_dir_all(&tmp_dir);

        let path = write_crash_log_file(
            &tmp_dir,
            "simulated panic message",
            Some(("src/test.rs", 42, 10)),
            1700000000,
        )
        .expect("crash log must write cleanly");

        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("harbour crash log"));
        assert!(content.contains("simulated panic message"));
        assert!(content.contains("src/test.rs:42:10"));
        assert!(content.contains("timestamp: 1700000000"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_panic_hook_crash_log() {
        let tmp_dir = std::env::temp_dir().join("harbour_panic_hook_test");
        let _ = fs::create_dir_all(&tmp_dir);
        let tmp_dir_clone = tmp_dir.clone();

        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = write_crash_log(&tmp_dir_clone, info);
        }));

        let _ = std::panic::catch_unwind(|| {
            panic!("test panic hook capture");
        });

        std::panic::set_hook(prev_hook);

        let files: Vec<_> = fs::read_dir(&tmp_dir).unwrap().collect();
        assert_eq!(files.len(), 1);
        let log_content = fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert!(log_content.contains("test panic hook capture"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
