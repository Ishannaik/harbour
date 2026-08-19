//! The download queue: the concurrency cap, oldest-first promotion, and the
//! projection of engine state onto [`QueueStatus`].
//!
//! The queue owns *policy*; the engine owns *transfer*. Everything here is
//! testable against [`crate::engine::fake::FakeEngine`] with no network, which
//! is why the trait exists.
//!
//! Two behaviours are worth reading before changing anything:
//!
//! * **The file-gone detector.** A real seed never pulls data off the network:
//!   verifying on-disk files reads the *disk*, so download speed stays zero.
//!   A "seed" that is downloading has therefore lost its files. It must survive
//!   a grace period and consecutive observations before we act, because a fresh
//!   re-seed legitimately looks identical while it verifies. On trip we stop the
//!   torrent and mark it `Missing` — we never silently re-download it.
//! * **The cap is a policy, not a limit on the queue.** Any number of items can
//!   be queued; `max_downloads` bounds only how many are handed to the engine at
//!   once, and [`Queue::promote`] fills freed slots oldest-first.
//!
//! `dead_code` is allowed module-wide until the app loop owns a `Queue` (E2).
//! Everything here is exercised by the tests below. Remove the allow as the
//! wiring lands.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::error::EngineError;
use crate::core::types::{
    AddBytesRequest, AddRequest, CompletedItem, Engine, EngineEvent, EngineItemState, EngineStats,
    InfoHash, ItemView, QueueItem, QueueStatus, SourceId, project_status,
};

/// How long a freshly (re-)started seed is exempt from the file-gone detector.
///
/// Verification reads the disk but an engine can briefly report progress < 1
/// with non-zero speed while it settles, which is indistinguishable from a
/// genuinely missing file. Ten seconds covers a normal verification.
pub const SEED_GRACE: Duration = Duration::from_secs(10);

/// Consecutive detector observations before acting, so a single-piece repair
/// blip cannot condemn a healthy seed.
pub const STRAY_TICKS: u32 = 2;

/// One entry's runtime bookkeeping — never persisted.
#[derive(Debug, Default, Clone)]
struct Runtime {
    stats: Option<EngineStats>,
    stray_hits: u32,
    seed_started_at: Option<Instant>,
}

/// What a caller wants downloaded.
#[derive(Debug, Clone, PartialEq)]
pub struct AddInput {
    pub id: InfoHash,
    pub name: String,
    pub source: Option<SourceId>,
    /// `None` when the magnet still has to be resolved from a detail page; the
    /// item is accepted and stays `Queued` until it is supplied.
    pub magnet: Option<String>,
    /// Raw `.torrent` bytes when this item is added from a file rather than a
    /// magnet (`FR-02`/`FR-39`). Mutually exclusive with `magnet` in practice;
    /// an item with either may start.
    pub bytes: Option<Vec<u8>>,
    pub dir: PathBuf,
    pub size_bytes: u64,
    /// Selected file indices for batch downloads (None = download all).
    pub only_files: Option<HashSet<usize>>,
}

/// Outcome of [`Queue::add`], so callers can tell the user what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Newly accepted and started.
    Started,
    /// Newly accepted, waiting for a slot.
    Queued,
    /// Already known — focus the existing row instead of creating a duplicate.
    Duplicate,
    /// Known and previously failed or flagged missing; it has been reset and
    /// retried (a `Failed` retry, or a `Missing` re-check, `FR-46`).
    Retried,
}

pub struct Queue {
    engine: Arc<dyn Engine>,
    /// Insertion-ordered: promotion is oldest-first and the ledger should read
    /// in a stable order, so this is a Vec rather than a map.
    items: Vec<QueueItem>,
    runtime: HashMap<InfoHash, Runtime>,
    /// 0 means unlimited (`FR-07`).
    max_downloads: usize,
    /// When set, a finished seed whose share ratio (uploaded/downloaded) has
    /// reached it pauses itself (qBittorrent `max_ratio` + `max_ratio_act` =
    /// stop). The seed keeps its files; it is a paused seed, per AGENTS.
    stop_ratio: Option<f64>,
    pub seed_by_default: bool,
    trackers: Vec<String>,
}

impl Queue {
    /// The underlying engine — needed by the UI for stream URLs (FR-57).
    pub fn engine(&self) -> &Arc<dyn Engine> {
        &self.engine
    }

    pub fn new(engine: Arc<dyn Engine>, max_downloads: usize) -> Self {
        Self {
            engine,
            items: Vec::new(),
            runtime: HashMap::new(),
            max_downloads,
            stop_ratio: None,
            seed_by_default: true,
            trackers: Vec::new(),
        }
    }

    /// Extra announce URLs applied to torrents added from now on.
    pub fn set_trackers(&mut self, trackers: Vec<String>) {
        self.trackers = trackers;
    }

    /// Live queueing cap (0 = unlimited), from settings.
    pub fn set_max_downloads(&mut self, max_downloads: usize) {
        self.max_downloads = max_downloads;
    }

    /// Live seed-stop ratio policy from settings; `None` disables it.
    pub fn set_stop_ratio(&mut self, ratio: Option<f64>) {
        self.stop_ratio = ratio;
    }

    /// Live auto-seeding policy from settings.
    pub fn set_seed_by_default(&mut self, val: bool) {
        self.seed_by_default = val;
    }

    /// Removes all completed / seeding items from the queue, keeping their files on disk.
    pub async fn clear_completed(&mut self) -> Vec<InfoHash> {
        let completed: Vec<InfoHash> = self
            .items
            .iter()
            .filter(|i| {
                i.finished || matches!(i.status, QueueStatus::Seeding | QueueStatus::Missing)
            })
            .map(|i| i.id.clone())
            .collect();

        for id in &completed {
            let _ = self.engine.remove(id, false).await;
            self.items.retain(|i| &i.id != id);
            self.runtime.remove(id);
        }
        self.promote().await;
        completed
    }

    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    pub fn get(&self, id: &str) -> Option<&QueueItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Items joined with their live stats, which is what the UI renders.
    pub fn views(&self) -> Vec<ItemView> {
        self.items
            .iter()
            .map(|item| {
                let stats = self.runtime.get(&item.id).and_then(|r| r.stats);
                ItemView::new(item.clone(), stats)
            })
            .collect()
    }

    /// Completed downloads, newest first — the "recently downloaded" list,
    /// derived from the ledger rather than stored separately.
    pub fn completed(&self) -> Vec<CompletedItem> {
        let mut done: Vec<&QueueItem> = self.items.iter().filter(|i| i.finished).collect();
        done.sort_by_key(|i| std::cmp::Reverse(i.added_at_epoch_ms));
        done.into_iter().map(CompletedItem::from).collect()
    }

    pub fn active_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status.is_active_download())
            .count()
    }

    fn slot_free(&self) -> bool {
        self.max_downloads == 0 || self.active_count() < self.max_downloads
    }

    /// Adds an item, or reports why it was not added.
    ///
    /// Duplicate detection is by infohash (`FR-56`): a second add of a known id
    /// focuses the existing row. A previously *failed* item is the exception —
    /// re-adding it is how a user retries. A `Missing` item is the other
    /// (`FR-46`): its files were flagged gone, so re-adding is the explicit
    /// re-check that restarts it against whatever is on disk.
    pub async fn add(&mut self, input: AddInput, now_ms: i64) -> AddOutcome {
        if let Some(existing) = self.items.iter_mut().find(|i| i.id == input.id) {
            if !matches!(existing.status, QueueStatus::Failed | QueueStatus::Missing) {
                return AddOutcome::Duplicate;
            }
            existing.status = QueueStatus::Queued;
            existing.error = None;
            if existing.magnet.is_none() {
                existing.magnet = input.magnet;
            }
            if existing.bytes.is_none() {
                existing.bytes = input.bytes;
            }
            self.promote().await;
            return AddOutcome::Retried;
        }

        let mut item = QueueItem::new(
            input.id,
            input.name,
            input.source,
            input.magnet,
            input.dir,
            now_ms,
        );
        item.total_bytes = input.size_bytes;
        item.bytes = input.bytes;
        item.only_files = input.only_files;
        let id = item.id.clone();
        self.items.push(item);
        self.runtime.insert(id.clone(), Runtime::default());
        self.promote().await;

        match self.get(&id).map(|i| i.status) {
            Some(QueueStatus::Downloading) => AddOutcome::Started,
            _ => AddOutcome::Queued,
        }
    }

    /// Supplies a magnet resolved on demand, unblocking a queued item.
    pub async fn set_magnet(&mut self, id: &str, magnet: String) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.magnet = Some(magnet);
        }
        self.promote().await;
    }

    /// Starts queued items oldest-first while slots are free.
    ///
    /// With no cap this still runs, because items restored from a previously
    /// capped run come back `Queued` and must start. An item with neither a
    /// magnet nor `.torrent` bytes is skipped rather than started — it is
    /// waiting on resolution, not on a slot — and must not block the items
    /// behind it.
    pub async fn promote(&mut self) {
        loop {
            if !self.slot_free() {
                return;
            }
            let next = self
                .items
                .iter()
                .filter(|i| {
                    i.status == QueueStatus::Queued && (i.magnet.is_some() || i.bytes.is_some())
                })
                .min_by_key(|i| i.added_at_epoch_ms)
                .map(|i| i.id.clone());
            let Some(id) = next else { return };
            if !self.start(&id).await {
                // Starting failed and the item is now `Failed`; loop again so
                // one bad item cannot stall everything behind it.
                continue;
            }
        }
    }

    /// Hands one durable item to the engine: from its `.torrent` bytes when it
    /// was added from a file, otherwise from its magnet.
    async fn add_item_to_engine(&self, item: &QueueItem) -> Result<(), EngineError> {
        let trackers = self.trackers.clone();
        match &item.bytes {
            Some(bytes) => {
                self.engine
                    .add_bytes(AddBytesRequest {
                        bytes: bytes.clone(),
                        dir: item.dir.clone(),
                        trackers,
                        only_files: item.only_files.clone(),
                    })
                    .await
            }
            None => {
                let magnet = item.magnet.clone().unwrap_or_default();
                self.engine
                    .add(AddRequest {
                        id: item.id.clone(),
                        magnet,
                        dir: item.dir.clone(),
                        trackers,
                        only_files: item.only_files.clone(),
                    })
                    .await
            }
        }
    }

    /// Hands one item to the engine. Returns false if it failed to start.
    async fn start(&mut self, id: &str) -> bool {
        let Some(idx) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        let item = &self.items[idx];
        if item.magnet.is_none() && item.bytes.is_none() {
            return false;
        }
        match self.add_item_to_engine(item).await {
            Ok(()) => {
                let item = &mut self.items[idx];
                item.status = QueueStatus::Downloading;
                item.error = None;
                true
            }
            Err(err) => {
                // One item's failure must never fail the caller, which is
                // usually a whole restore (plan §4.2).
                let item = &mut self.items[idx];
                item.status = QueueStatus::Failed;
                item.error = Some(err.to_string());
                false
            }
        }
    }

    /// Pauses a downloading, queued, or seeding item.
    pub async fn pause(&mut self, id: &str) -> Result<(), EngineError> {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return Err(EngineError::NotFound);
        };
        let was_live = matches!(item.status, QueueStatus::Downloading | QueueStatus::Seeding);
        item.status = QueueStatus::Paused;
        if was_live {
            self.engine.pause(id).await?;
            if let Some(rt) = self.runtime.get_mut(id) {
                rt.seed_started_at = None;
                rt.stray_hits = 0;
            }
            // A freed slot goes to whoever is waiting.
            self.promote().await;
        }
        Ok(())
    }

    /// Resumes a paused item. A paused *seed* goes back to the engine; a paused
    /// download re-enters the queue and respects the cap.
    pub async fn resume(&mut self, id: &str, now: Instant) -> Result<(), EngineError> {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return Err(EngineError::NotFound);
        };
        if item.status != QueueStatus::Paused {
            return Ok(());
        }
        if item.finished {
            item.status = QueueStatus::Seeding;
            self.engine.resume(id).await?;
            if let Some(rt) = self.runtime.get_mut(id) {
                // Re-verification starts now, so the detector's grace restarts.
                rt.seed_started_at = Some(now);
                rt.stray_hits = 0;
            }
        } else {
            item.status = QueueStatus::Queued;
            self.promote().await;
        }
        Ok(())
    }

    /// Removes an item entirely. `delete_files` is destructive and is never a
    /// default anywhere above this layer.
    pub async fn remove(&mut self, id: &str, delete_files: bool) -> Result<(), EngineError> {
        if !self.items.iter().any(|i| i.id == id) {
            return Err(EngineError::NotFound);
        }
        self.engine.remove(id, delete_files).await?;
        self.items.retain(|i| i.id != id);
        self.runtime.remove(id);
        self.promote().await;
        Ok(())
    }

    /// Polls the engine and reconciles the queue with it.
    ///
    /// `now` is a parameter rather than read from the clock so the grace period
    /// and the consecutive-observation counter are testable at fixed ticks.
    /// Returns the events the app should forward to the UI.
    pub async fn tick(&mut self, now: Instant) -> Vec<EngineEvent> {
        let snapshots = self.engine.snapshot();
        let mut events = Vec::new();
        let mut newly_missing: Vec<InfoHash> = Vec::new();
        let mut freed_slot = false;
        // Seeds that hit the share-ratio target this tick; paused after the
        // loop so no borrow crosses an await.
        let mut ratio_paused: Vec<InfoHash> = Vec::new();

        for snap in snapshots {
            let Some(idx) = self.items.iter().position(|i| i.id == snap.id) else {
                continue;
            };

            // Metadata arriving is the first moment we know the real name/size.
            if let Some(name) = &snap.name {
                sync_metadata(
                    &mut self.items,
                    idx,
                    name,
                    snap.stats.total_bytes,
                    &mut events,
                );
            }

            let was_finished = self.items[idx].finished;
            let previous = self.items[idx].status;

            // A paused item is ours to keep paused: the engine may still be
            // settling and must not resurrect it.
            if previous == QueueStatus::Paused {
                self.runtime.entry(snap.id.clone()).or_default().stats = Some(snap.stats);
                continue;
            }

            if self.detect_missing(&snap, previous, was_finished, now) {
                newly_missing.push(snap.id.clone());
                continue;
            }

            let projected = project_status(snap.state, snap.finished || was_finished);
            {
                let rt = self.runtime.entry(snap.id.clone()).or_default();
                rt.stats = Some(snap.stats);
            }

            let item = &mut self.items[idx];
            let newly_finished = snap.finished && !item.finished;
            if newly_finished {
                item.finished = true;
                events.push(EngineEvent::Done {
                    id: snap.id.clone(),
                });
                freed_slot = true;
            }
            if newly_finished && !self.seed_by_default {
                item.status = QueueStatus::Paused;
                ratio_paused.push(snap.id.clone());
            }

            let status_changed = projected != previous;
            if status_changed && previous.is_active_download() && !projected.is_active_download() {
                freed_slot = true;
            }
            if status_changed {
                item.status = projected;
            }
            if status_changed && projected == QueueStatus::Failed {
                let message = snap
                    .error
                    .clone()
                    .unwrap_or_else(|| "the torrent engine reported a failure".into());
                item.error = Some(message.clone());
                events.push(EngineEvent::Failed {
                    id: snap.id.clone(),
                    message,
                });
            }

            // Track when a seed started so the detector's grace has an anchor.
            if projected == QueueStatus::Seeding {
                let rt = self.runtime.entry(snap.id.clone()).or_default();
                rt.seed_started_at.get_or_insert(now);
            }

            // qBittorrent-style share-ratio stop: a finished seed that has
            // reached its target ratio pauses itself. The files stay; it
            // reads as a paused seed (AGENTS vocabulary). The engine pause
            // happens after the loop so this borrow never crosses an await.
            if item.finished
                && item.status == QueueStatus::Seeding
                && self.stop_ratio.is_some_and(|target| {
                    let d = snap.stats.downloaded_bytes;
                    d > 0 && snap.stats.uploaded_bytes as f64 / d as f64 >= target
                })
            {
                item.status = QueueStatus::Paused;
                ratio_paused.push(snap.id.clone());
            }

            events.push(EngineEvent::Progress {
                id: snap.id.clone(),
                stats: snap.stats,
            });
        }

        for id in ratio_paused {
            let _ = self.engine.pause(&id).await;
        }

        for id in newly_missing {
            // Stop the torrent before flagging it: leaving it running is what
            // would re-download the whole thing.
            let _ = self.engine.remove(&id, false).await;
            if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                item.status = QueueStatus::Missing;
            }
            if let Some(rt) = self.runtime.get_mut(&id) {
                reset_runtime(rt);
            }
            events.push(EngineEvent::Missing { id });
            freed_slot = true;
        }

        if freed_slot {
            self.promote().await;
        }
        events
    }

    /// The file-gone detector. See the module docs for the reasoning.
    ///
    /// Only a *seed* can be missing, so an item that has never finished is never
    /// a candidate — an unfinished download that is downloading is simply doing
    /// its job. An engine error never routes here either: `Failed` and `Missing`
    /// mean different things to a user and conflating them would report a
    /// transient tracker failure as lost data.
    fn detect_missing(
        &mut self,
        snap: &crate::core::types::EngineSnapshot,
        previous: QueueStatus,
        was_finished: bool,
        now: Instant,
    ) -> bool {
        let is_seed = was_finished || previous == QueueStatus::Seeding;
        if !is_seed || snap.state != EngineItemState::Live {
            return false;
        }
        let pulling = snap.stats.progress < 1.0 && snap.stats.speed_mib > 0.0;
        let rt = self.runtime.entry(snap.id.clone()).or_default();
        if !pulling {
            rt.stray_hits = 0;
            return false;
        }
        // Inside the grace window, verification looks exactly like this.
        let age = rt.seed_started_at.map(|t| now.saturating_duration_since(t));
        if age.is_none_or(|a| a < SEED_GRACE) {
            return false;
        }
        rt.stray_hits += 1;
        rt.stray_hits >= STRAY_TICKS
    }

    /// Replaces the queue contents on startup.
    ///
    /// `safe` is the bootguard path (`FR-53`): the previous run died mid-restore,
    /// so everything comes back paused with no engine started and the user
    /// resumes on their own terms.
    pub async fn restore(&mut self, items: Vec<QueueItem>, safe: bool) {
        self.items = items;
        self.runtime = self
            .items
            .iter()
            .map(|i| (i.id.clone(), Runtime::default()))
            .collect();

        for item in &mut self.items {
            match item.status {
                // Nothing is live yet after a restart: the bootguard parks
                // everything, otherwise a download waits its turn.
                QueueStatus::Downloading if !safe => item.status = QueueStatus::Queued,
                QueueStatus::Downloading => item.status = QueueStatus::Paused,
                QueueStatus::Queued | QueueStatus::Seeding if safe => {
                    item.status = QueueStatus::Paused
                }
                _ => {}
            }
        }

        if safe {
            return;
        }

        // Restart seeds first: they hold no download slot, and a user expects
        // their seeds back.
        let seeds: Vec<InfoHash> = self
            .items
            .iter()
            .filter(|i| {
                i.status == QueueStatus::Seeding && (i.magnet.is_some() || i.bytes.is_some())
            })
            .map(|i| i.id.clone())
            .collect();
        for id in seeds {
            let item = self
                .get(&id)
                .cloned()
                .unwrap_or_else(|| unreachable!("id came from self.items"));
            if let Err(err) = self.add_item_to_engine(&item).await
                && let Some(item) = self.items.iter_mut().find(|i| i.id == id)
            {
                // A seed that will not restart is visible and paused, not
                // silently dropped and not re-downloaded.
                item.status = QueueStatus::Paused;
                item.error = Some(err.to_string());
            }
        }
        self.promote().await;
    }
}

/// Applies one snapshot's name/size to the queue item, pushing a `Metadata`
/// event when something actually changed.
fn sync_metadata(
    items: &mut [QueueItem],
    idx: usize,
    name: &str,
    total_bytes: u64,
    events: &mut Vec<EngineEvent>,
) {
    let item = &mut items[idx];
    if item.name != name || item.total_bytes != total_bytes {
        item.name = name.to_owned();
        if total_bytes > 0 {
            item.total_bytes = total_bytes;
        }
        events.push(EngineEvent::Metadata {
            id: item.id.clone(),
            name: name.to_owned(),
            total_bytes: item.total_bytes,
        });
    }
}

/// Clears the file-gone detector's state for one runtime entry: the grace
/// anchor and any stale stats, so a re-add starts from a clean slate.
fn reset_runtime(rt: &mut Runtime) {
    rt.stray_hits = 0;
    rt.seed_started_at = None;
    if let Some(stats) = rt.stats.as_mut() {
        stats.speed_mib = 0.0;
        stats.peers = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fake::FakeEngine;

    fn input(id: &str, at: i64) -> AddInput {
        AddInput {
            id: id.repeat(40)[..40].to_owned(),
            name: format!("item-{id}"),
            source: Some(SourceId::CineVault),
            magnet: Some(format!("magnet:?xt=urn:btih:{}", id.repeat(40))),
            bytes: None,
            dir: PathBuf::from("/tmp/dl"),
            size_bytes: 1000,
            only_files: None,
        }
        .with_time(at)
    }

    impl AddInput {
        fn with_time(self, _at: i64) -> Self {
            self
        }
    }

    fn setup(max: usize) -> (Queue, Arc<FakeEngine>) {
        let engine = Arc::new(FakeEngine::new());
        (Queue::new(engine.clone(), max), engine)
    }

    fn id_of(c: &str) -> String {
        c.repeat(40)[..40].to_owned()
    }

    #[tokio::test]
    async fn add_starts_when_a_slot_is_free_and_queues_when_it_is_not() {
        let (mut q, engine) = setup(1);
        assert_eq!(q.add(input("a", 1), 1).await, AddOutcome::Started);
        assert_eq!(q.add(input("b", 2), 2).await, AddOutcome::Queued);

        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Downloading);
        assert_eq!(q.get(&id_of("b")).unwrap().status, QueueStatus::Queued);
        assert_eq!(engine.len(), 1, "only the started item reaches the engine");
    }

    #[tokio::test]
    async fn unlimited_is_the_default_and_starts_everything() {
        let (mut q, engine) = setup(0);
        for (i, c) in ["a", "b", "c"].iter().enumerate() {
            q.add(input(c, i as i64), i as i64).await;
        }
        assert_eq!(q.active_count(), 3);
        assert_eq!(engine.len(), 3);
    }

    #[tokio::test]
    async fn a_freed_slot_promotes_the_oldest_waiter_first() {
        let (mut q, _e) = setup(1);
        q.add(input("a", 1), 1).await;
        // Added out of chronological order on purpose: promotion must use the
        // timestamp, not insertion order.
        q.add(input("c", 30), 30).await;
        q.add(input("b", 20), 20).await;

        q.pause(&id_of("a")).await.unwrap();
        assert_eq!(
            q.get(&id_of("b")).unwrap().status,
            QueueStatus::Downloading,
            "the older of the two waiters starts"
        );
        assert_eq!(q.get(&id_of("c")).unwrap().status, QueueStatus::Queued);
    }

    #[tokio::test]
    async fn a_duplicate_add_focuses_the_existing_item() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        assert_eq!(q.add(input("a", 2), 2).await, AddOutcome::Duplicate);
        assert_eq!(q.items().len(), 1, "FR-56: no second row");
        assert_eq!(engine.len(), 1);
    }

    #[tokio::test]
    async fn re_adding_a_failed_item_retries_it() {
        let (mut q, engine) = setup(0);
        engine.fail_next_add(EngineError::Unavailable("no port".into()));
        q.add(input("a", 1), 1).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Failed);

        assert_eq!(q.add(input("a", 2), 2).await, AddOutcome::Retried);
        let item = q.get(&id_of("a")).unwrap();
        assert_eq!(item.status, QueueStatus::Downloading);
        assert!(item.error.is_none(), "the old error is cleared on retry");
    }

    #[tokio::test]
    async fn re_adding_a_missing_item_rechecks_it() {
        // FR-46: the file-gone detector flags a seed `Missing`; re-adding the
        // same id must restart it as a re-check, not be swallowed as a
        // duplicate — that is the only way a `Missing` row comes back.
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.deliver_metadata(&id_of("a"), "Example", 1000);
        engine.complete(&id_of("a"));
        let t0 = Instant::now();
        q.tick(t0).await;
        engine.lose_files(&id_of("a"));
        q.tick(t0 + SEED_GRACE + Duration::from_secs(1)).await;
        q.tick(t0 + SEED_GRACE + Duration::from_secs(2)).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Missing);
        assert!(!engine.contains(&id_of("a")), "the detector stopped it");

        assert_eq!(q.add(input("a", 2), 2).await, AddOutcome::Retried);
        let item = q.get(&id_of("a")).unwrap();
        assert_eq!(
            item.status,
            QueueStatus::Downloading,
            "the re-check restarts the torrent"
        );
        assert!(item.error.is_none());
        assert!(engine.contains(&id_of("a")), "the engine holds it again");
    }

    #[tokio::test]
    async fn a_torrent_bytes_add_starts_under_its_own_infohash() {
        // FR-02/.39: an item added from raw `.torrent` bytes is keyed by the
        // hash the engine derives from the payload, so the poll and restart
        // both match, and a second add of the same file dedupes by hash.
        let (mut q, engine) = setup(0);
        let payload = b"d4:infod6:lengthi5e4:name3:fooe".to_vec();
        let hash = engine.torrent_info_hash(&payload).expect("the fake parses");

        let outcome = q
            .add(
                AddInput {
                    id: hash.clone(),
                    name: "movie".into(),
                    source: None,
                    magnet: None,
                    bytes: Some(payload.clone()),
                    dir: PathBuf::from("/tmp/dl"),
                    size_bytes: 0,
                    only_files: None,
                },
                1,
            )
            .await;
        assert_eq!(outcome, AddOutcome::Started);

        let item = q.get(&hash).expect("keyed by the file's own hash");
        assert_eq!(item.status, QueueStatus::Downloading);
        assert!(engine.contains(&hash), "the engine keys identically");
        assert_eq!(q.items().len(), 1);

        // Same payload again: FR-56 dedupe by infohash still holds for bytes.
        assert_eq!(
            q.add(
                AddInput {
                    id: hash.clone(),
                    name: "movie".into(),
                    source: None,
                    magnet: None,
                    bytes: Some(payload),
                    dir: PathBuf::from("/tmp/dl"),
                    size_bytes: 0,
                    only_files: None,
                },
                2,
            )
            .await,
            AddOutcome::Duplicate
        );
        assert_eq!(q.items().len(), 1);
    }

    #[tokio::test]
    async fn a_queued_torrent_bytes_item_keeps_its_bytes() {
        // A file-add behind a full cap must not lose its payload: the bytes
        // stay on the durable item until a slot frees (FR-39).
        let (mut q, _e) = setup(1);
        let payload = b"d4:infod6:lengthi5e4:name3:fooe".to_vec();
        let hash = {
            let engine = q.engine().clone();
            // the fake derives the id from the payload
            engine.torrent_info_hash(&payload).expect("parses")
        };
        q.add(input("b", 2), 2).await; // takes the single slot
        q.add(
            AddInput {
                id: hash.clone(),
                name: "movie".into(),
                source: None,
                magnet: None,
                bytes: Some(payload.clone()),
                dir: PathBuf::from("/tmp/dl"),
                size_bytes: 0,
                only_files: None,
            },
            3,
        )
        .await;

        let item = q.get(&hash).unwrap();
        assert_eq!(item.status, QueueStatus::Queued, "waits for a slot");
        assert_eq!(item.bytes.as_deref(), Some(payload.as_slice()));
    }

    #[tokio::test]
    async fn one_item_failing_to_start_does_not_stall_the_rest() {
        let (mut q, engine) = setup(0);
        engine.fail_next_add(EngineError::Unavailable("nope".into()));
        q.add(input("a", 1), 1).await;
        q.add(input("b", 2), 2).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Failed);
        assert_eq!(
            q.get(&id_of("b")).unwrap().status,
            QueueStatus::Downloading,
            "the batch survives one bad item"
        );
    }

    #[tokio::test]
    async fn an_item_without_a_magnet_waits_without_blocking_the_queue() {
        let (mut q, _e) = setup(1);
        let mut lazy = input("a", 1);
        lazy.magnet = None;
        q.add(lazy, 1).await;
        q.add(input("b", 2), 2).await;

        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Queued);
        assert_eq!(
            q.get(&id_of("b")).unwrap().status,
            QueueStatus::Downloading,
            "a row awaiting magnet resolution must not hold the slot"
        );

        // Resolution arrives; with the slot busy it stays queued but is now
        // eligible.
        q.set_magnet(&id_of("a"), "magnet:?xt=urn:btih:x".into())
            .await;
        q.pause(&id_of("b")).await.unwrap();
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Downloading);
    }

    #[tokio::test]
    async fn completion_moves_to_seeding_and_frees_a_slot() {
        let (mut q, engine) = setup(1);
        q.add(input("a", 1), 1).await;
        q.add(input("b", 2), 2).await;

        engine.complete(&id_of("a"));
        let events = q.tick(Instant::now()).await;

        assert!(events.iter().any(|e| matches!(e, EngineEvent::Done { .. })));
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);
        assert!(q.get(&id_of("a")).unwrap().finished);
        assert_eq!(
            q.get(&id_of("b")).unwrap().status,
            QueueStatus::Downloading,
            "seeding does not hold a download slot"
        );
    }

    #[tokio::test]
    async fn a_seed_at_its_share_ratio_pauses_itself() {
        let (mut q, engine) = setup(1);
        q.add(input("a", 1), 1).await;
        engine.deliver_metadata(&id_of("a"), "Example", 1000);
        engine.complete(&id_of("a"));
        q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        // Ratio 0.5: uploaded >= 500 of 1000 downloaded stops the seed.
        q.set_stop_ratio(Some(0.5));
        engine.set_uploaded(&id_of("a"), 600);
        q.tick(Instant::now()).await;
        let item = q.get(&id_of("a")).unwrap();
        assert_eq!(item.status, QueueStatus::Paused, "the seed pauses itself");
        assert!(item.finished, "a paused seed keeps its finished flag");
        assert!(item.is_paused_seed());

        // A paused seed stays paused until the user resumes it (qBittorrent's
        // "stop" semantics). Resume with a policy that no longer applies —
        // the ratio target raised above the current share — keeps it seeding.
        q.set_stop_ratio(Some(2.0));
        engine.set_uploaded(&id_of("a"), 100);
        q.resume(&id_of("a"), Instant::now()).await.unwrap();
        q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        // Disabling the policy never stops a seed, whatever it has uploaded.
        q.set_stop_ratio(None);
        engine.set_uploaded(&id_of("a"), 5000);
        q.resume(&id_of("a"), Instant::now()).await.unwrap();
        q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);
    }

    #[tokio::test]
    async fn an_engine_error_fails_the_item_and_never_marks_it_missing() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.complete(&id_of("a"));
        q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        // A tracker error on a *seed* must read as failed, not as lost files.
        engine.fail(&id_of("a"), "tracker timeout");
        let events = q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Failed);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EngineEvent::Failed { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, EngineEvent::Missing { .. })),
            "an engine error must never be reported as missing files"
        );
    }

    #[tokio::test]
    async fn the_file_gone_detector_needs_grace_and_consecutive_hits() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.deliver_metadata(&id_of("a"), "Example", 1000);
        engine.complete(&id_of("a"));

        let t0 = Instant::now();
        q.tick(t0).await; // becomes a seed; grace starts here
        engine.lose_files(&id_of("a"));

        // Inside the grace window this looks exactly like verification.
        q.tick(t0 + Duration::from_secs(5)).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        // Past grace, one observation is still not enough — a one-piece repair
        // blip must not condemn a healthy seed.
        q.tick(t0 + SEED_GRACE + Duration::from_secs(1)).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        let events = q.tick(t0 + SEED_GRACE + Duration::from_secs(2)).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Missing);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EngineEvent::Missing { .. }))
        );
        assert!(
            !engine.contains(&id_of("a")),
            "the torrent is stopped, never left running to re-download"
        );
    }

    #[tokio::test]
    async fn a_healthy_seed_is_never_flagged_missing() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.complete(&id_of("a"));
        let t0 = Instant::now();
        for i in 0..10 {
            q.tick(t0 + Duration::from_secs(i * 5)).await;
        }
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);
    }

    #[tokio::test]
    async fn an_unfinished_download_is_never_flagged_missing() {
        // Downloading with progress < 1 and speed > 0 is the normal case; only
        // a *seed* doing it means anything is wrong.
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.set_progress(&id_of("a"), 0.3, 5.0);
        let t0 = Instant::now();
        for i in 0..6 {
            q.tick(t0 + Duration::from_secs(i * 10)).await;
        }
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Downloading);
    }

    #[tokio::test]
    async fn a_paused_item_is_not_resurrected_by_the_engine_poll() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        q.pause(&id_of("a")).await.unwrap();
        engine.set_progress(&id_of("a"), 0.9, 3.0); // engine still settling
        q.tick(Instant::now()).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Paused);
    }

    #[tokio::test]
    async fn pausing_and_resuming_a_seed_restarts_its_grace_window() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        engine.complete(&id_of("a"));
        let t0 = Instant::now();
        q.tick(t0).await;

        q.pause(&id_of("a")).await.unwrap();
        assert!(q.get(&id_of("a")).unwrap().is_paused_seed());

        let resume_at = t0 + Duration::from_secs(60);
        q.resume(&id_of("a"), resume_at).await.unwrap();
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);

        // Files vanish right after the resume: the fresh grace window must
        // protect the re-verification that a resume legitimately performs.
        engine.lose_files(&id_of("a"));
        q.tick(resume_at + Duration::from_secs(2)).await;
        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Seeding);
    }

    #[tokio::test]
    async fn resuming_a_paused_download_respects_the_cap() {
        let (mut q, _e) = setup(1);
        q.add(input("a", 1), 1).await;
        q.add(input("b", 2), 2).await;
        q.pause(&id_of("a")).await.unwrap(); // b starts
        q.resume(&id_of("a"), Instant::now()).await.unwrap();
        assert_eq!(
            q.get(&id_of("a")).unwrap().status,
            QueueStatus::Queued,
            "no free slot, so it waits rather than exceeding the cap"
        );
    }

    #[tokio::test]
    async fn remove_drops_the_item_and_frees_its_slot() {
        let (mut q, engine) = setup(1);
        q.add(input("a", 1), 1).await;
        q.add(input("b", 2), 2).await;
        q.remove(&id_of("a"), false).await.unwrap();

        assert!(q.get(&id_of("a")).is_none());
        assert!(!engine.contains(&id_of("a")));
        assert_eq!(q.get(&id_of("b")).unwrap().status, QueueStatus::Downloading);
        assert_eq!(q.remove("unknown", false).await, Err(EngineError::NotFound));
    }

    #[tokio::test]
    async fn restore_resumes_downloads_and_seeds() {
        let (mut q, engine) = setup(0);
        let mut downloading = QueueItem::new(
            id_of("a"),
            "A".into(),
            None,
            Some("magnet:a".into()),
            PathBuf::from("/tmp"),
            1,
        );
        downloading.status = QueueStatus::Downloading;
        let mut seeding = QueueItem::new(
            id_of("b"),
            "B".into(),
            None,
            Some("magnet:b".into()),
            PathBuf::from("/tmp"),
            2,
        );
        seeding.status = QueueStatus::Seeding;
        seeding.finished = true;

        q.restore(vec![downloading, seeding], false).await;

        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Downloading);
        assert_eq!(q.get(&id_of("b")).unwrap().status, QueueStatus::Seeding);
        assert_eq!(engine.len(), 2);
    }

    #[tokio::test]
    async fn safe_mode_restores_everything_paused_and_starts_no_engine() {
        let (mut q, engine) = setup(0);
        let mut downloading = QueueItem::new(
            id_of("a"),
            "A".into(),
            None,
            Some("magnet:a".into()),
            PathBuf::from("/tmp"),
            1,
        );
        downloading.status = QueueStatus::Downloading;
        let mut seeding = QueueItem::new(
            id_of("b"),
            "B".into(),
            None,
            Some("magnet:b".into()),
            PathBuf::from("/tmp"),
            2,
        );
        seeding.status = QueueStatus::Seeding;
        seeding.finished = true;

        q.restore(vec![downloading, seeding], true).await;

        assert_eq!(q.get(&id_of("a")).unwrap().status, QueueStatus::Paused);
        assert_eq!(q.get(&id_of("b")).unwrap().status, QueueStatus::Paused);
        assert!(
            engine.is_empty(),
            "FR-53: safe mode starts no engine at all"
        );
    }

    #[tokio::test]
    async fn a_seed_that_will_not_restart_stays_visible_and_paused() {
        let (mut q, engine) = setup(0);
        engine.fail_next_add(EngineError::Unavailable("busted".into()));
        let mut seeding = QueueItem::new(
            id_of("b"),
            "B".into(),
            None,
            Some("magnet:b".into()),
            PathBuf::from("/tmp"),
            2,
        );
        seeding.status = QueueStatus::Seeding;
        seeding.finished = true;

        q.restore(vec![seeding], false).await;
        let item = q.get(&id_of("b")).unwrap();
        assert_eq!(item.status, QueueStatus::Paused, "visible and resumable");
        assert!(item.error.is_some(), "and it says why");
    }

    #[tokio::test]
    async fn restore_respects_the_cap_and_queues_the_overflow() {
        let (mut q, _e) = setup(1);
        let items: Vec<QueueItem> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut item = QueueItem::new(
                    id_of(c),
                    c.to_string(),
                    None,
                    Some(format!("magnet:{c}")),
                    PathBuf::from("/tmp"),
                    i as i64,
                );
                item.status = QueueStatus::Downloading;
                item
            })
            .collect();
        q.restore(items, false).await;
        assert_eq!(q.active_count(), 1);
        assert_eq!(
            q.items()
                .iter()
                .filter(|i| i.status == QueueStatus::Queued)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn views_join_durable_items_with_live_stats() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 1), 1).await;
        q.add(input("b", 2), 2).await;
        engine.deliver_metadata(&id_of("a"), "Example", 2000);
        engine.set_progress(&id_of("a"), 0.5, 6.0);
        q.tick(Instant::now()).await;

        let views = q.views();
        let a = views.iter().find(|v| v.item.id == id_of("a")).unwrap();
        assert_eq!(a.progress(), 0.5);
        assert_eq!(a.peers(), Some(12));
        assert_eq!(a.speed_mib(), 6.0);
        assert_eq!(a.total_bytes(), 2000);
    }

    #[tokio::test]
    async fn completed_lists_only_finished_items_newest_first() {
        let (mut q, engine) = setup(0);
        q.add(input("a", 10), 10).await;
        q.add(input("b", 20), 20).await;
        q.add(input("c", 30), 30).await;
        engine.complete(&id_of("a"));
        engine.complete(&id_of("c"));
        q.tick(Instant::now()).await;

        let done: Vec<String> = q.completed().into_iter().map(|c| c.id).collect();
        assert_eq!(done, vec![id_of("c"), id_of("a")], "newest first");
    }
}
