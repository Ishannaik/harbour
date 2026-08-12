//! UI views: splash (app.rs), search, downloads, status bar, help overlay,
//! and the now-playing watch screen.
//!
//! Each view is a pure `draw(frame, area, state, theme)` function — no input
//! handling, no state mutation. The app loop (app.rs) owns keybind dispatch
//! and the 30fps tick; views just paint. All colors come from the theme's
//! curated subset (bg, text, accent, border, success, error, warning, muted,
//! dim, selected_bg).

//! `dead_code` is allowed for this subtree until `app.rs` dispatches to
//! these views (integration is in flight); mirrors `theme.rs`'s staged-API
//! allow. The lint scope covers the child view modules too, so it lives
//! here rather than being repeated in each. Remove it as the wiring lands.

#![allow(dead_code)]

use std::collections::HashMap;

use crate::core::types::{CompletedItem, ItemView, SourceId, SourceStatus, TorrentResult};

/// Which screen the TUI is showing.
///
/// Lives here rather than in `core`: the engine and the sources have no opinion
/// about screens, and the freeze is deliberately limited to the contract all
/// three tracks share (`docs/plan-engine.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    Splash,
    /// The landing screen (`FR-01`). The splash is a timed intro the app leaves,
    /// not a resting state, so `Search` is the default.
    #[default]
    Search,
    Downloads,
    Help,
    /// The settings overlay (2.5): a modal over the current screen, so
    /// closing it returns the user exactly where they were. The app loop
    /// tracks it with `settings_open`; this variant exists for the shared
    /// screen contract and the exhaustive matches.
    Settings,
    /// Watch mode (FR-57): an external player owns the screen until it exits.
    NowPlaying,
}

/// Search-view state.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<TorrentResult>,
    pub selected: usize,
    pub searching: bool,
    /// Per-source dot. A source absent from the map has never been probed and
    /// renders as unknown — not as offline.
    pub source_health: HashMap<SourceId, SourceStatus>,
    pub source_counts: HashMap<SourceId, usize>,
}

/// Downloads-view state.
///
/// `items` are [`ItemView`]s — the durable queue item joined with whatever live
/// statistics exist — so the view never has to know that the two halves are
/// stored differently.
#[derive(Debug, Clone, Default)]
pub struct DownloadsState {
    pub items: Vec<ItemView>,
    /// Recently downloaded, derived from the ledger rather than stored twice.
    pub history: Vec<CompletedItem>,
    pub selected: usize,
    pub show_seeding: bool,
}

/// Top-level UI state, mutated by input handlers and engine events, rendered at
/// the fixed frame cadence.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub screen: Screen,
    pub search: SearchState,
    pub downloads: DownloadsState,
    /// The single channel for engine and config errors (`UR-13`).
    pub error_banner: Option<String>,
    /// The item being watched, if watch mode is active (FR-57).
    pub now_playing: Option<NowPlaying>,
}

/// Watch-mode state (FR-57..FR-59): what the now-playing screen shows while an
/// external player streams the item.
#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub id: String,
    pub name: String,
    pub stream_url: String,
    /// True for a watch-now session (2.3): the torrent was added straight to
    /// the engine (no queue item, no ledger) and its cache dir is deleted
    /// when the session ends — the stream-and-delete contract.
    pub ephemeral: bool,
}

pub mod downloads;
pub mod help;
pub mod now_playing;
pub mod player;
pub mod search;
pub mod settings;
pub mod status;

#[cfg(test)]
mod tests;
