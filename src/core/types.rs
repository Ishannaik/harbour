//! The shared contract — frozen (`docs/plan-engine.md` §3).
//!
//! Two shapes here look unusual and are load-bearing, so they are explained at
//! the decision site rather than in a doc nobody reads twice:
//!
//! * [`Source::search`] returns a boxed future instead of `async fn`. Both
//!   `async fn` in a trait and `-> impl Future` make the trait dyn-incompatible,
//!   and the fan-out needs `Vec<Arc<dyn Source>>` over ten heterogeneous
//!   sources. This is what `#[async_trait]` generates, without the crate.
//! * [`QueueItem`] holds only durable facts and [`EngineStats`] holds only
//!   volatile ones, joined by [`ItemView`] for rendering. Persisting live stats
//!   makes them either stale between writes or a whole-file rewrite per poll
//!   tick, and `FR-50` says resume state comes from the engine anyway.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::cancel::CancelToken;
use crate::core::error::{EngineError, SourceError};

/// Lowercase 40-hex BitTorrent infohash — the canonical id for a result, a
/// queue item, and every on-disk path derived from one.
///
/// Normalized to lowercase at the boundary ([`crate::core::magnet`]) so it can
/// be used as a join key across sources without case surprises.
pub type InfoHash = String;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// The ten curated sources (`docs/sources.md` §2).
///
/// An enum rather than a string: it makes the registry exhaustive, kills typo
/// bugs in cache paths, and — unlike `&'static str` — it can `Deserialize`,
/// which the search cache requires because it persists results verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceId {
    /// The client-side proxy source (`src/sources.rs`): every search goes to
    /// the user-run indexer over HTTP. Deliberately **not** in [`SourceId::ALL`]
    /// — `ALL` is the ten toggleable *sites* the indexer searches, and the
    /// sidebar/settings iterate it.
    #[serde(rename = "indexer")]
    Indexer,
    #[serde(rename = "gameshub")]
    GamesHub,
    #[serde(rename = "cinevault")]
    CineVault,
    #[serde(rename = "vault-movies")]
    VaultMovies,
    #[serde(rename = "vault-tv")]
    VaultTv,
    #[serde(rename = "reel-movies")]
    ReelSource,
    #[serde(rename = "reel-tv")]
    ReelTv,
    #[serde(rename = "showport")]
    ShowPort,
    #[serde(rename = "tsukibase")]
    TsukiBase,
    #[serde(rename = "fansubs")]
    FanSubs,
    #[serde(rename = "torrent-hub")]
    TorrentHub,
}

impl SourceId {
    /// Every source, in sidebar order. The registry and the source-health map
    /// both iterate this, so adding a source is one variant plus one row here.
    pub const ALL: [SourceId; 10] = [
        SourceId::GamesHub,
        SourceId::CineVault,
        SourceId::VaultMovies,
        SourceId::ReelSource,
        SourceId::TorrentHub,
        SourceId::ShowPort,
        SourceId::VaultTv,
        SourceId::ReelTv,
        SourceId::TsukiBase,
        SourceId::FanSubs,
    ];

    /// The stable wire/table id. Also the cache directory name, which is why it
    /// must stay filesystem-safe and must never change casually.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::Indexer => "indexer",
            SourceId::GamesHub => "gameshub",
            SourceId::CineVault => "cinevault",
            SourceId::VaultMovies => "vault-movies",
            SourceId::VaultTv => "vault-tv",
            SourceId::ReelSource => "reel-movies",
            SourceId::ReelTv => "reel-tv",
            SourceId::ShowPort => "showport",
            SourceId::TsukiBase => "tsukibase",
            SourceId::FanSubs => "fansubs",
            SourceId::TorrentHub => "torrent-hub",
        }
    }

    /// Parses the wire id. Used when reading a ledger or cache file written by
    /// an older build; an unknown id degrades to `None` rather than failing the
    /// whole file (`plan-engine.md` §4.2).
    pub fn parse(s: &str) -> Option<SourceId> {
        SourceId::ALL.into_iter().find(|id| id.as_str() == s)
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sidebar categories, in torlink's group order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Static metadata for one source. One `const` per source; the registry and the
/// sidebar read only this, so neither has to construct an adapter to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceDef {
    pub id: SourceId,
    pub label: &'static str,
    pub groups: &'static [SourceGroup],
    pub homepage: &'static str,
    /// False when the source's feed carries no trustworthy swarm counts (RSS
    /// without seed fields). `seeders: 0` from such a source means *unknown*,
    /// not *dead* — an alive-only filter must never drop those rows, and the UI
    /// renders a neutral dot instead of a health colour.
    pub reports_health: bool,
}

/// Live health of one source, for the sidebar dot.
///
/// `Unknown` and `Checking` are distinct on purpose: before any search a source
/// has no state, and *during* a search one that has not answered yet is not the
/// same as one that failed. Without `Checking` the 3s partial-results deadline
/// has no way to say "still waiting" and every slow source reads as dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceStatus {
    /// Never probed this session.
    #[default]
    Unknown,
    /// Search in flight, no answer yet.
    Checking,
    /// Answered with results.
    Online,
    /// Answered successfully with zero results — reachable, nothing matched.
    Empty,
    /// Failed or ran out of budget.
    Offline,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentResult {
    pub info_hash: InfoHash,
    pub name: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    /// Absent on most RSS feeds.
    pub num_files: Option<u32>,
    pub source: SourceId,
    /// `None` means "resolvable on demand", not "broken".
    ///
    /// Sources that hide the magnet behind a detail page (ReelIndex, GamesHub,
    /// TorrentHub) would otherwise have to fetch one extra page *per row* at
    /// search time — the single largest latency cost in the reference product.
    /// A displayable row never requires the magnet; the engine calls
    /// [`Source::resolve_magnet`] when the user actually presses `d`.
    pub magnet: Option<String>,
    /// Publication time, unix seconds. Unix seconds rather than a `DateTime`
    /// keeps `chrono` out of the tree for what is one integer.
    pub added: Option<i64>,
}

/// Per-search knobs handed to every source.
///
/// This is how the engine keeps sources stateless (`docs/sources.md` §1.1): the
/// deadline budget, the cancellation signal, and the sticky-host hint are all
/// session state that the *engine* owns and passes in, so a source never has to
/// remember anything between searches.
#[derive(Clone, Debug)]
pub struct SearchCtx {
    /// When the UI stops holding the search bar and renders what it has. A
    /// source past this is `Checking`, not `Offline` — late results still land.
    pub list_deadline: Duration,
    /// Hard ceiling for the whole source, including follow-up fetches.
    pub total_deadline: Duration,
    /// "Start probing at this host." The engine remembers which mirror answered
    /// last so a dead primary is not retried first on every search.
    pub host_hint: Option<String>,
    /// Sites the user disabled; the `HttpSource` sends them as the `exclude`
    /// param so the indexer never queries them. Filled by the search engine
    /// from its own disabled set before a search starts.
    pub disabled: HashSet<SourceId>,
    pub cancel: CancelToken,
}

impl Default for SearchCtx {
    fn default() -> Self {
        Self {
            list_deadline: Duration::from_secs(3),
            total_deadline: Duration::from_secs(10),
            host_hint: None,
            disabled: HashSet::new(),
            cancel: CancelToken::new(),
        }
    }
}

/// Future returned by [`Source::search`].
pub type SearchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<TorrentResult>, SourceError>> + Send + 'a>>;

/// Future returned by [`Source::resolve_magnet`].
pub type MagnetFuture<'a> = Pin<Box<dyn Future<Output = Result<String, SourceError>> + Send + 'a>>;

/// A torrent search backend. One implementation per row of the source matrix.
///
/// Stateless by contract: everything that would otherwise be remembered between
/// searches (cache, health, mirror order) lives in the engine and arrives via
/// [`SearchCtx`].
///
/// The boxed-future return is not a style choice — see the module docs.
pub trait Source: Send + Sync + 'static {
    fn def(&self) -> &'static SourceDef;

    /// Search for `query`; an empty query means the curated top list.
    ///
    /// Must be abort-safe: the engine drops the future on cancellation or when
    /// the deadline expires.
    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a>;

    /// Produce the magnet for a result whose `magnet` is `None`.
    ///
    /// The default is correct for every source that already returns one, so
    /// only the detail-page scrapers implement this.
    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        _ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        let existing = result.magnet.clone();
        Box::pin(async move {
            existing.ok_or_else(|| {
                SourceError::Parse("source returned no magnet and cannot resolve one".into())
            })
        })
    }
}

/// Registry element. `Arc` rather than `Box` so a search task can hold one
/// without borrowing the registry for the duration.
pub type ArcSource = Arc<dyn Source>;

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// Queue lifecycle (`AGENTS.md` shared vocabulary, six states).
///
/// One `Paused` covers a paused download and a paused seed; [`QueueItem::finished`]
/// tells them apart. That matches how the `p` key reads to a user and keeps the
/// enum small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueStatus {
    /// Accepted, waiting for a concurrency slot. Never handed to the engine yet.
    Queued,
    Downloading,
    Paused,
    Failed,
    Seeding,
    /// Complete, but the files are gone from disk. Reachable *only* from the
    /// file-gone detector — never from an engine error, or a flaky tracker would
    /// tell the user their data vanished.
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

    /// Whether this status occupies one of the `HARBOUR_MAX_DOWNLOADS` slots.
    pub fn is_active_download(self) -> bool {
        matches!(self, QueueStatus::Downloading)
    }
}

/// One item in the queue — **durable facts only**.
///
/// Everything here survives a restart and is worth writing to disk. Live
/// statistics live in [`EngineStats`] and are never persisted: written on every
/// status change they would be stale, and written often enough to be accurate
/// they would rewrite the whole ledger twice a second per item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: InfoHash,
    pub name: String,
    pub source: Option<SourceId>,
    /// `None` while the magnet has not been resolved yet (see
    /// [`TorrentResult::magnet`]); an item cannot leave `Queued` without one.
    pub magnet: Option<String>,
    pub dir: PathBuf,
    pub status: QueueStatus,
    /// True once the item has ever completed. Distinguishes a paused seed from
    /// a paused download, and survives restarts so a restored seed is not
    /// mistaken for an unfinished one.
    pub finished: bool,
    /// Total size once metadata has arrived; 0 before that.
    pub total_bytes: u64,
    /// Why this item is `Failed`, kept so the reason survives a restart.
    pub error: Option<String>,
    pub added_at_epoch_ms: i64,
}

impl QueueItem {
    /// A fresh queued item. Status starts `Queued`; the queue promotes it when a
    /// slot is free, so nothing else has to know about the cap.
    pub fn new(
        id: InfoHash,
        name: String,
        source: Option<SourceId>,
        magnet: Option<String>,
        dir: PathBuf,
        added_at_epoch_ms: i64,
    ) -> Self {
        Self {
            id,
            name,
            source,
            magnet,
            dir,
            status: QueueStatus::Queued,
            finished: false,
            total_bytes: 0,
            error: None,
            added_at_epoch_ms,
        }
    }

    /// A paused *seed* rather than a paused download.
    pub fn is_paused_seed(&self) -> bool {
        self.status == QueueStatus::Paused && self.finished
    }
}

/// Live statistics for one torrent. Never persisted (see [`QueueItem`]).
///
/// `peers` and `eta` are `Option` because the engine genuinely cannot report
/// them while a torrent is paused, initializing, or errored — the UI renders
/// `—`, never `0`, so "paused" is never mistaken for "nobody is connected".
///
/// Speeds are MiB/s as `f64`, converted once in the engine adapter so no second
/// converter exists anywhere in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EngineStats {
    /// 0.0..=1.0.
    pub progress: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_mib: f64,
    pub upload_speed_mib: f64,
    pub uploaded_bytes: u64,
    pub peers: Option<u32>,
    pub eta: Option<Duration>,
}

/// What the UI renders: durable item plus whatever live stats exist.
///
/// This is the seam that lets [`QueueItem`] stay persistence-shaped while the
/// views keep one struct to read. A queued or restored-but-not-started item has
/// no stats at all, which is exactly `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemView {
    pub item: QueueItem,
    pub stats: Option<EngineStats>,
}

impl ItemView {
    pub fn new(item: QueueItem, stats: Option<EngineStats>) -> Self {
        Self { item, stats }
    }

    /// Fraction complete. A finished item reads 1.0 even with no live stats, so
    /// a restored seed does not render as an empty bar.
    pub fn progress(&self) -> f64 {
        match &self.stats {
            Some(s) => s.progress.clamp(0.0, 1.0),
            None if self.item.finished => 1.0,
            None => 0.0,
        }
    }

    pub fn peers(&self) -> Option<u32> {
        self.stats.as_ref().and_then(|s| s.peers)
    }

    pub fn eta(&self) -> Option<Duration> {
        self.stats.as_ref().and_then(|s| s.eta)
    }

    pub fn speed_mib(&self) -> f64 {
        self.stats.as_ref().map_or(0.0, |s| s.speed_mib)
    }

    pub fn upload_speed_mib(&self) -> f64 {
        self.stats.as_ref().map_or(0.0, |s| s.upload_speed_mib)
    }

    pub fn total_bytes(&self) -> u64 {
        self.stats
            .as_ref()
            .map(|s| s.total_bytes)
            .filter(|b| *b > 0)
            .unwrap_or(self.item.total_bytes)
    }

    pub fn downloaded_bytes(&self) -> u64 {
        match &self.stats {
            Some(s) => s.downloaded_bytes,
            None if self.item.finished => self.item.total_bytes,
            None => 0,
        }
    }
}

/// One completed download, for the "recently downloaded" list.
///
/// Derived from the ledger rather than stored separately: a completed download
/// is a queue item with `finished == true`, so a second file would be a second
/// source of truth. `history.json` is search queries (`FR-49`), which is a
/// different thing that happens to share the word.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedItem {
    pub id: InfoHash,
    pub name: String,
    pub size_bytes: u64,
    pub source: Option<SourceId>,
}

impl From<&QueueItem> for CompletedItem {
    fn from(item: &QueueItem) -> Self {
        Self {
            id: item.id.clone(),
            name: item.name.clone(),
            size_bytes: item.total_bytes,
            source: item.source,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The engine's own view of one torrent, before it is projected onto a
/// [`QueueStatus`].
///
/// Mirrors librqbit's states so the projection lives in exactly one place
/// ([`project_status`]) instead of being re-derived at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineItemState {
    /// Checking existing files / fetching metadata.
    Initializing,
    Live,
    Paused,
    Errored,
}

/// Projects an engine state onto a queue status.
///
/// The `Initializing` split is load-bearing: after a restart a *complete* seed
/// passes through `Initializing` with `finished == true` while it verifies its
/// files. Mapping that to `Downloading` would show every restored seed as an
/// active download the moment the app opens (`FR-47`).
///
/// An engine error is always `Failed`, **never** `Missing`. `Missing` means "the
/// files are gone from disk" and is reachable only from the file-gone detector;
/// routing a transient tracker error there would tell the user their data
/// vanished, which is the exact mistake `FR-45` exists to prevent.
pub fn project_status(state: EngineItemState, finished: bool) -> QueueStatus {
    match (state, finished) {
        (EngineItemState::Errored, _) => QueueStatus::Failed,
        (EngineItemState::Paused, _) => QueueStatus::Paused,
        (EngineItemState::Initializing, true) | (EngineItemState::Live, true) => {
            QueueStatus::Seeding
        }
        (EngineItemState::Initializing, false) | (EngineItemState::Live, false) => {
            QueueStatus::Downloading
        }
    }
}

/// What the engine is asked to start.
#[derive(Debug, Clone, PartialEq)]
pub struct AddRequest {
    pub id: InfoHash,
    pub magnet: String,
    pub dir: PathBuf,
    /// Extra announce URLs appended to whatever the magnet carries.
    pub trackers: Vec<String>,
}

/// One engine observation, as read by the queue's poll.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSnapshot {
    pub id: InfoHash,
    pub state: EngineItemState,
    pub finished: bool,
    pub stats: EngineStats,
    /// Set when `state` is [`EngineItemState::Errored`].
    pub error: Option<String>,
    /// Present once metadata has arrived.
    pub name: Option<String>,
}

/// Future returned by the [`Engine`] mutators.
pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The torrent engine, behind a trait so the queue can be tested without a
/// network, a runtime full of sockets, or librqbit's several-hundred-crate tree.
///
/// Boxed futures for the same dyn-compatibility reason as [`Source`].
pub trait Engine: Send + Sync {
    fn add<'a>(&'a self, req: AddRequest) -> EngineFuture<'a, Result<(), EngineError>>;
    fn pause<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>>;
    fn resume<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>>;
    /// Drops the torrent. `delete_files` is destructive and is never the default
    /// anywhere in the UI.
    fn remove<'a>(
        &'a self,
        id: &'a str,
        delete_files: bool,
    ) -> EngineFuture<'a, Result<(), EngineError>>;
    /// Every torrent the engine currently holds. Synchronous because the poll
    /// runs on the UI cadence and must never await.
    fn snapshot(&self) -> Vec<EngineSnapshot>;
    /// A playable loopback stream URL for `id` served from the swarm while
    /// pieces arrive (FR-57: Stremio-style watch-before-download). Default
    /// is none — the file-based watch path is the fallback. Additive: the
    /// trait is frozen, so a default keeps every implementor compiling.
    fn stream_url<'a>(&'a self, _id: &'a str) -> EngineFuture<'a, Option<String>> {
        Box::pin(async move { None })
    }

    /// Live global speed limits in MiB/s (None = unlimited). Additive
    /// default no-op — the trait is frozen, so every implementor compiles;
    /// librqbit's session limiter applies instantly, no restart needed.
    fn set_speed_limits(&self, _download_mib: Option<u64>, _upload_mib: Option<u64>) {}
}

/// Events pushed from the engine and the search layer to the app state.
///
/// Search and engine events share one channel because the UI merges them into
/// one screen; splitting them would only move the join somewhere less obvious.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Metadata arrived: name and size are now known, and the `.torrent` bytes
    /// have been cached for a later local re-seed.
    Metadata {
        id: InfoHash,
        name: String,
        total_bytes: u64,
    },
    Progress {
        id: InfoHash,
        stats: EngineStats,
    },
    /// Transitioned to complete; the queue moves it to seeding.
    Done {
        id: InfoHash,
    },
    /// Engine failure for one item — always projects to `Failed`.
    Failed {
        id: InfoHash,
        message: String,
    },
    /// The file-gone detector fired. Distinct from `Failed` on purpose.
    Missing {
        id: InfoHash,
    },
    /// A source's health changed — in flight, online, empty, or offline.
    ///
    /// `Checking` is emitted the moment a source starts, so the sidebar can
    /// distinguish "still working" from "dead" while a search is in flight.
    SourceStatus {
        source: SourceId,
        status: SourceStatus,
    },
    /// One source finished successfully.
    SourceAnswered {
        source: SourceId,
        count: usize,
    },
    /// One source's rows.
    ///
    /// The app state accumulates these and runs them through `search::merge`,
    /// so deduplication and ordering live in one function with one set of tests
    /// rather than being re-derived per source.
    SourceResults {
        source: SourceId,
        results: Vec<TorrentResult>,
    },
    /// One source gave up. `class` is [`SourceError::class`].
    SourceFailed {
        source: SourceId,
        class: &'static str,
        message: String,
    },
    /// Every source has settled, or the budget expired.
    SearchComplete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids_round_trip_through_their_wire_form() {
        for id in SourceId::ALL {
            assert_eq!(
                SourceId::parse(id.as_str()),
                Some(id),
                "{id} did not round-trip"
            );
        }
        assert_eq!(SourceId::parse("nope"), None);
    }

    #[test]
    fn source_id_all_has_no_duplicates_and_covers_the_matrix() {
        let mut seen: Vec<&str> = SourceId::ALL.iter().map(|s| s.as_str()).collect();
        seen.sort_unstable();
        let len = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), len, "duplicate source id in ALL");
        assert_eq!(len, 10, "docs/sources.md §2 defines ten sources");
    }

    #[test]
    fn source_id_serde_uses_the_wire_form_not_the_variant_name() {
        let json = serde_json::to_string(&SourceId::VaultMovies).expect("serialize");
        assert_eq!(json, "\"vault-movies\"");
        let back: SourceId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, SourceId::VaultMovies);
    }

    #[test]
    fn queue_status_serde_matches_the_normative_vocabulary() {
        // The on-disk spelling must equal label(), so a ledger is readable by eye
        // and matches AGENTS.md's status list.
        for status in [
            QueueStatus::Queued,
            QueueStatus::Downloading,
            QueueStatus::Paused,
            QueueStatus::Failed,
            QueueStatus::Seeding,
            QueueStatus::Missing,
        ] {
            let json = serde_json::to_string(&status).expect("serialize");
            assert_eq!(json, format!("\"{}\"", status.label()));
        }
    }

    #[test]
    fn a_restored_complete_seed_is_never_shown_as_downloading() {
        // The whole point of splitting Initializing on `finished` (FR-47).
        assert_eq!(
            project_status(EngineItemState::Initializing, true),
            QueueStatus::Seeding
        );
        assert_eq!(
            project_status(EngineItemState::Initializing, false),
            QueueStatus::Downloading
        );
    }

    #[test]
    fn an_engine_error_is_failed_never_missing() {
        // Missing means "your files are gone" and must come only from the
        // file-gone detector — see project_status' docs.
        assert_eq!(
            project_status(EngineItemState::Errored, false),
            QueueStatus::Failed
        );
        assert_eq!(
            project_status(EngineItemState::Errored, true),
            QueueStatus::Failed,
            "an errored seed is failed, not missing"
        );
    }

    #[test]
    fn paused_projects_regardless_of_completion() {
        assert_eq!(
            project_status(EngineItemState::Paused, true),
            QueueStatus::Paused
        );
        assert_eq!(
            project_status(EngineItemState::Paused, false),
            QueueStatus::Paused
        );
    }

    fn sample_item() -> QueueItem {
        QueueItem::new(
            "a".repeat(40),
            "Example".into(),
            Some(SourceId::CineVault),
            Some("magnet:?xt=urn:btih:aaaa".into()),
            PathBuf::from("/tmp/dl"),
            1_786_000_000_000,
        )
    }

    #[test]
    fn queue_item_round_trips_including_paused() {
        let mut item = sample_item();
        item.status = QueueStatus::Paused;
        item.finished = true;
        let json = serde_json::to_string(&item).expect("serialize");
        let back: QueueItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, item);
        assert!(back.is_paused_seed(), "paused + finished is a paused seed");
    }

    #[test]
    fn queue_item_persists_no_volatile_stats() {
        // Guards the F-7 decision: if someone adds `progress` or `speed` back to
        // the ledger struct, this test is the thing that objects.
        let json = serde_json::to_value(sample_item()).expect("serialize");
        let obj = json.as_object().expect("object");
        for volatile in [
            "progress",
            "speed_mib",
            "upload_speed_mib",
            "peers",
            "eta",
            "eta_secs",
            "downloaded_bytes",
            "uploaded_bytes",
        ] {
            assert!(
                !obj.contains_key(volatile),
                "`{volatile}` is a live statistic and must not be persisted"
            );
        }
    }

    #[test]
    fn item_view_falls_back_gracefully_without_stats() {
        let mut item = sample_item();
        item.total_bytes = 1000;
        item.finished = true;
        item.status = QueueStatus::Seeding;

        // A restored seed has no live stats yet: it must still read as complete
        // rather than as an empty progress bar.
        let view = ItemView::new(item.clone(), None);
        assert_eq!(view.progress(), 1.0);
        assert_eq!(view.downloaded_bytes(), 1000);
        assert_eq!(view.peers(), None, "unknown, not zero");
        assert_eq!(view.eta(), None);
        assert_eq!(view.speed_mib(), 0.0);

        // An unfinished item with no stats reads as not started.
        item.finished = false;
        item.status = QueueStatus::Queued;
        let queued = ItemView::new(item, None);
        assert_eq!(queued.progress(), 0.0);
        assert_eq!(queued.downloaded_bytes(), 0);
    }

    #[test]
    fn item_view_prefers_live_totals_but_keeps_the_durable_one_when_unknown() {
        let mut item = sample_item();
        item.total_bytes = 500;
        let stats = EngineStats {
            total_bytes: 0, // metadata has not landed yet
            ..EngineStats::default()
        };
        let view = ItemView::new(item.clone(), Some(stats));
        assert_eq!(view.total_bytes(), 500, "0 from the engine means unknown");

        let stats = EngineStats {
            total_bytes: 900,
            ..EngineStats::default()
        };
        assert_eq!(ItemView::new(item, Some(stats)).total_bytes(), 900);
    }

    #[test]
    fn item_view_clamps_a_nonsense_progress() {
        let stats = EngineStats {
            progress: 1.7,
            ..EngineStats::default()
        };
        assert_eq!(ItemView::new(sample_item(), Some(stats)).progress(), 1.0);
    }

    #[test]
    fn completed_item_is_derived_from_the_ledger_not_stored_twice() {
        let mut item = sample_item();
        item.total_bytes = 42;
        let done = CompletedItem::from(&item);
        assert_eq!(done.id, item.id);
        assert_eq!(done.size_bytes, 42);
        assert_eq!(done.source, Some(SourceId::CineVault));
    }

    #[test]
    fn source_status_defaults_to_unknown_not_offline() {
        // An unprobed source must not render as dead.
        assert_eq!(SourceStatus::default(), SourceStatus::Unknown);
    }

    #[test]
    fn search_ctx_defaults_to_the_documented_budget() {
        let ctx = SearchCtx::default();
        assert_eq!(ctx.list_deadline, Duration::from_secs(3));
        assert_eq!(ctx.total_deadline, Duration::from_secs(10));
        assert!(!ctx.cancel.is_cancelled());
    }

    #[test]
    fn only_downloading_consumes_a_concurrency_slot() {
        assert!(QueueStatus::Downloading.is_active_download());
        for other in [
            QueueStatus::Queued,
            QueueStatus::Paused,
            QueueStatus::Failed,
            QueueStatus::Seeding,
            QueueStatus::Missing,
        ] {
            assert!(
                !other.is_active_download(),
                "{other:?} must not hold a slot"
            );
        }
    }
}
