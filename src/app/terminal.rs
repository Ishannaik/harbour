//! Terminal lifecycle: raw mode, the alternate screen, and cursor/mouse
//! capture — entered in one step and restored (best-effort) on every exit
//! path, including unwinds.

use std::io;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Restores the terminal on drop: show cursor, leave alternate screen, then
/// disable raw mode — in that order, best-effort. Mouse capture is released
/// with the alternate screen it was enabled on. Drop cannot return errors,
/// and a partial restore still beats leaving the user's shell unusable.
pub(crate) struct TerminalGuard;

impl TerminalGuard {
    /// Enters the TUI: raw mode first, then the alternate screen with the
    /// cursor hidden and mouse capture on. If the alternate-screen entry
    /// fails, raw mode is disabled before the error propagates so a
    /// half-set-up terminal is never leaked to the caller.
    pub(crate) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen, Hide, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Cursor/alt-screen/mouse must be restored while still in raw mode;
        // raw mode is lifted last so no escape codes leak to the shell.
        let _ = execute!(
            io::stdout(),
            Show,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = disable_raw_mode();
    }
}
