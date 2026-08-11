//! UI views (phase 2): search, downloads, status line, help.
//!
//! Each view is a pure `draw(frame, area, state, theme)` function — no input
//! handling, no state mutation. The app loop (app.rs) owns keybind dispatch
//! and the 30fps tick; views just paint. All colors come from the theme's
//! curated subset (bg, text, accent, border, success, error, warning, muted,
//! dim, selected_bg).

pub mod downloads;
pub mod help;
pub mod search;
pub mod status;

use std::collections::HashMap;
use std::time::Duration;

use ratatui::symbols::border::Set as BorderSet;

use crate::theme::Theme;
use crate::types::InfoHash;

/// Per-frame animation inputs, computed by the app loop and handed to the
/// pure view draws so they never read a clock (design.md §3, §9 fixed ticks).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameVars<'a> {
    /// Time since the app started — drives shimmer phases and colorizers.
    pub elapsed: Duration,
    /// Current status-spinner frame (80ms cadence, advanced by the loop).
    pub spinner: Option<&'a str>,
}

/// Eased display values computed by the app loop (design.md §3: bars ease
/// toward target, never jump). Views read only these; they never own easing.
#[derive(Debug, Clone, Default)]
pub(crate) struct DisplayState {
    /// Smoothed progress per queue item (0..=1), keyed by info_hash.
    pub progress: HashMap<InfoHash, f64>,
    /// Smoothed fraction of sources that have answered a search (0..=1).
    pub answered: f64,
}

/// Builds the themed rounded border set (design.md §1: `╭╮╰╯` + tee
/// junctions) from the theme's symbol overrides, so custom themes control
/// the whole border language. Lives as long as `theme`.
pub(crate) fn border_set(theme: &Theme) -> BorderSet<'_> {
    BorderSet {
        top_left: theme.symbols.border_tl.as_ref(),
        top_right: theme.symbols.border_tr.as_ref(),
        bottom_left: theme.symbols.border_bl.as_ref(),
        bottom_right: theme.symbols.border_br.as_ref(),
        vertical_left: theme.symbols.border_v.as_ref(),
        vertical_right: theme.symbols.border_v.as_ref(),
        horizontal_top: theme.symbols.border_h.as_ref(),
        horizontal_bottom: theme.symbols.border_h.as_ref(),
    }
}

/// Formats a byte count for result rows and queue items ("48.2 GB").
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Formats seconds as `mm:ss` (under an hour) or `HH:MM:SS`.
pub(crate) fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// Formats a MiB/s speed as "3.1 MB/s" — the design art's unit, kept for the
/// user even though the engine contract is MiB/s (notes-for-ishan.md B2).
pub(crate) fn human_speed(mib: f64) -> String {
    if mib < 0.001 {
        "0.0 MB/s".to_string()
    } else {
        format!("{mib:.1} MB/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(48_200_000_000), "44.9 GB");
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(human_duration(0), "00:00");
        assert_eq!(human_duration(65), "01:05");
        assert_eq!(human_duration(3724), "01:02:04");
    }
}
