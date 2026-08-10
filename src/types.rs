//! Shared domain types — the working freeze (implementing Sarthak's approved
//! contract per `docs/notes-for-ishan.md` A3/B1/B2).
//!
//! NOTE: per decision-1, the *normative* freeze is owned by the engine track
//! (Sarthak, phase 1A). This file implements his contract so the UI and
//! sources tracks can build now; when his `types.rs` lands, keep his.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Lowercase 40-hex BitTorrent infohash — canonical id for results/items.
pub type InfoHash = String;

/// Stable source id (see `docs/sources.md`).
pub type SourceId = &'static str;

/// Content categories shown in the sidebar (torlink group order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceGroup {
    Games,
    Movies,
    Tv,
    Anime,
}

impl SourceGroup {
    pub fn label(self) -> &'static str {
        match self {
            SourceGroup::Games => "Games",
            SourceGroup::Movies => "Movies",
            SourceGroup::Tv => "TV",
            SourceGroup::Anime => "Anime",
        }
    }
}

/// One search hit from one source. Same shape as torlink's `TorrentResult`;
/// magnet is built by the magnet builder, never taken from the page.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentResult {
    pub info_hash: InfoHash,
    pub name: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    pub num_files: Option<u32>,
    pub source: SourceId,
    pub magnet: String,
    pub added: Option<i64>,
}

/// Per-source health for the sidebar dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Online,
    Empty,
    Offline,
}

/// Options for one search round-trip; sources honor `timeout` and fail fast.
#[derive(Debug, Clone, Copy)]
pub struct SearchOptions {
    pub timeout: Option<Duration>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(10)),
        }
    }
}

/// The source contract. One impl per source; `search` runs on its own tokio
/// task so a slow source never blocks the others.
pub trait Source: Send + Sync {
    fn def(&self) -> &'static SourceDef;
    fn search(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> impl std::future::Future<Output = Result<Vec<TorrentResult>, String>> + Send;
}

/// Static source metadata (sidebar + registry read only this).
#[derive(Debug, Clone, Copy)]
pub struct SourceDef {
    pub id: SourceId,
    pub label: &'static str,
    pub groups: &'static [SourceGroup],
    pub homepage: &'static str,
    pub reports_health: bool,
}

/// Queue lifecycle — the six statuses (Sarthak's A3): one `Paused` for both
/// paused downloads and paused seeds, disambiguated by `QueueItem::finished`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueStatus {
    Queued,
    Downloading,
    Paused,
    Failed,
    Seeding,
    Missing,
}

impl QueueStatus {
    pub fn label(self) -> &'static str {
        match self {
            QueueStatus::Queued => "queued",
            QueueStatus::Downloading => "downloading",
            QueueStatus::Paused => "paused",
            QueueStatus::Failed => "failed",
            QueueStatus::Seeding => "seeding",
            QueueStatus::Missing => "missing",
        }
    }
}

/// One download/seed in the queue. Persisted to `~/.harbour/downloads.json`.
///
/// Contract (notes-for-ishan.md B1/B2): `peers` and `eta_secs` are `Option`
/// because librqbit genuinely cannot report them while a torrent is paused,
/// initializing, or errored — the UI renders `—`, never `0`. Speeds are
/// `f64` MiB/s: the engine adapter converts once, so no second converter
/// exists anywhere in the UI.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueItem {
    pub id: InfoHash,
    pub name: String,
    pub source: Option<SourceId>,
    pub magnet: String,
    pub dir: PathBuf,
    pub status: QueueStatus,
    /// True once the item ever completed (download → seed). Disambiguates a
    /// paused seed from a paused download (A3).
    pub finished: bool,
    pub progress: f64,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub speed_mib: f64,
    pub upload_speed_mib: f64,
    pub uploaded_bytes: u64,
    pub peers: Option<u32>,
    pub eta_secs: Option<u64>,
    pub error: Option<String>,
    pub added_at_epoch_ms: i64,
}

/// One completed download for "recently downloaded" (cap 500, SPEC FR-53).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryItem {
    pub id: InfoHash,
    pub name: String,
    pub size_bytes: u64,
    pub source: Option<SourceId>,
    pub completed_at_epoch_ms: i64,
}

/// Engine stats snapshot per poll. Units: `progress` 0..=1, speeds in MiB/s
/// (converted once by the engine adapter), `eta` Option (None = unknown).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineStats {
    pub progress: f64,
    pub downloaded: u64,
    pub total: u64,
    pub speed_mib: f64,
    pub upload_speed_mib: f64,
    pub uploaded: u64,
    pub peers: Option<u32>,
    pub time_remaining: Option<Duration>,
}

/// Events the engine pushes to the queue (mpsc → queue → UI state).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Metadata arrived: capture name/size/files and save `.torrent` bytes
    /// for instant re-seed (torlink's metadata capture).
    Metadata {
        id: InfoHash,
        name: String,
        total: u64,
        files: u32,
    },
    /// Periodic stats (500ms while downloading; ~5s when all settled).
    Progress { id: InfoHash, stats: EngineStats },
    /// Download completed; the queue moves the item to seeding.
    Done { id: InfoHash },
    /// Engine failure (item → Failed; seed → Missing).
    Error { id: InfoHash, message: String },
}

/// Search/browse state for the search view.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<TorrentResult>,
    pub selected: usize,
    pub searching: bool,
    pub source_health: HashMap<SourceId, SourceStatus>,
    pub source_counts: HashMap<SourceId, usize>,
}

/// Downloads screen state.
#[derive(Debug, Clone, Default)]
pub struct DownloadsState {
    pub items: Vec<QueueItem>,
    pub history: Vec<HistoryItem>,
    pub selected: usize,
    pub show_seeding: bool,
}

/// Phase-6 watch state (libmpv + stream URL).
#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub id: InfoHash,
    pub name: String,
    pub stream_url: String,
    pub progress: f64,
}

/// Top-level UI state, mutated by input handlers and engine/source events,
/// rendered at 30fps.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub search: SearchState,
    pub downloads: DownloadsState,
    pub now_playing: Option<NowPlaying>,
    pub error_banner: Option<String>,
}

/// Screen the TUI is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Splash,
    Search,
    Downloads,
    Help,
}

impl Default for Screen {
    fn default() -> Self {
        Screen::Search
    }
}
