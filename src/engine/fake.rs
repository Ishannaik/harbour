//! An in-memory [`Engine`] for tests and for the UI's pre-integration wiring.
//!
//! This exists so the queue's real logic — the concurrency cap, oldest-first
//! promotion, the completion transition, the file-gone detector — is tested
//! without a network, a swarm, or librqbit's several-hundred-crate tree. Every
//! state change a real engine would make asynchronously is driven explicitly
//! here, which is what makes those tests deterministic instead of timing-based.
//!
//! It is a *fake*, not a mock: it holds real state and answers consistently, so
//! a test reads as a sequence of events rather than a list of expectations.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::error::EngineError;
use crate::core::types::{
    AddRequest, Engine, EngineFuture, EngineItemState, EngineSnapshot, EngineStats, InfoHash,
};

#[derive(Debug, Clone)]
struct FakeTorrent {
    state: EngineItemState,
    finished: bool,
    stats: EngineStats,
    error: Option<String>,
    name: Option<String>,
}

impl FakeTorrent {
    fn new(total_bytes: u64) -> Self {
        Self {
            state: EngineItemState::Initializing,
            finished: false,
            stats: EngineStats {
                total_bytes,
                ..EngineStats::default()
            },
            error: None,
            name: None,
        }
    }
}

/// In-memory engine. Cheap to clone-share behind an `Arc`.
#[derive(Debug, Default)]
pub struct FakeEngine {
    torrents: Mutex<HashMap<InfoHash, FakeTorrent>>,
    /// When set, the next [`Engine::add`] fails with this error. Used to test
    /// the "one item fails, the batch survives" path.
    next_add_error: Mutex<Option<EngineError>>,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Poisoned locks are recovered rather than propagated: a test helper that
    /// panicked while holding the lock should fail that test, not cascade into
    /// every later assertion in the same process.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<InfoHash, FakeTorrent>> {
        self.torrents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Makes the next `add` fail, once.
    pub fn fail_next_add(&self, err: EngineError) {
        let mut slot = self
            .next_add_error
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *slot = Some(err);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.map().contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // --- test drivers -------------------------------------------------------

    /// Metadata arrives: name and total size become known and the torrent goes
    /// live.
    pub fn deliver_metadata(&self, id: &str, name: &str, total_bytes: u64) {
        if let Some(t) = self.map().get_mut(id) {
            t.name = Some(name.to_owned());
            t.stats.total_bytes = total_bytes;
            t.state = EngineItemState::Live;
        }
    }

    /// Sets download progress as a fraction, with a plausible speed.
    pub fn set_progress(&self, id: &str, fraction: f64, speed_mib: f64) {
        if let Some(t) = self.map().get_mut(id) {
            t.state = EngineItemState::Live;
            t.stats.progress = fraction.clamp(0.0, 1.0);
            t.stats.downloaded_bytes = (t.stats.total_bytes as f64 * t.stats.progress) as u64;
            t.stats.speed_mib = speed_mib;
            t.stats.peers = Some(12);
        }
    }

    /// Drives a torrent to completion — the transition the queue turns into a
    /// seed.
    pub fn complete(&self, id: &str) {
        if let Some(t) = self.map().get_mut(id) {
            t.state = EngineItemState::Live;
            t.finished = true;
            t.stats.progress = 1.0;
            t.stats.downloaded_bytes = t.stats.total_bytes;
            t.stats.speed_mib = 0.0;
        }
    }

    /// Puts a torrent into the engine's error state.
    pub fn fail(&self, id: &str, message: &str) {
        if let Some(t) = self.map().get_mut(id) {
            t.state = EngineItemState::Errored;
            t.error = Some(message.to_owned());
            t.stats.speed_mib = 0.0;
            t.stats.peers = None;
        }
    }

    /// Simulates the files disappearing under a completed seed: it is live and
    /// pulling data again, with progress below 1. This is the *only* signal the
    /// file-gone detector is allowed to act on.
    pub fn lose_files(&self, id: &str) {
        if let Some(t) = self.map().get_mut(id) {
            t.state = EngineItemState::Live;
            t.finished = false;
            t.stats.progress = 0.10;
            t.stats.downloaded_bytes = t.stats.total_bytes / 10;
            t.stats.speed_mib = 2.5;
        }
    }
}

impl Engine for FakeEngine {
    fn add<'a>(&'a self, req: AddRequest) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            if let Some(err) = self
                .next_add_error
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                return Err(err);
            }
            if req.magnet.is_empty() {
                return Err(EngineError::InvalidInput("empty magnet".into()));
            }
            // Re-adding an id the engine already holds is a no-op rather than an
            // error: the queue treats add as idempotent (FR-56).
            self.map()
                .entry(req.id)
                .or_insert_with(|| FakeTorrent::new(0));
            Ok(())
        })
    }

    fn pause<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            match self.map().get_mut(id) {
                Some(t) => {
                    t.state = EngineItemState::Paused;
                    t.stats.speed_mib = 0.0;
                    t.stats.upload_speed_mib = 0.0;
                    // Paused means the engine genuinely cannot report these —
                    // the UI must show "—", not "0".
                    t.stats.peers = None;
                    t.stats.eta = None;
                    Ok(())
                }
                None => Err(EngineError::NotFound),
            }
        })
    }

    fn resume<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            match self.map().get_mut(id) {
                Some(t) => {
                    t.state = EngineItemState::Live;
                    Ok(())
                }
                None => Err(EngineError::NotFound),
            }
        })
    }

    fn remove<'a>(
        &'a self,
        id: &'a str,
        _delete_files: bool,
    ) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            // Removing something already gone is success, not an error: the
            // queue must be able to clean up idempotently on a crash path.
            self.map().remove(id);
            Ok(())
        })
    }

    fn snapshot(&self) -> Vec<EngineSnapshot> {
        let mut out: Vec<EngineSnapshot> = self
            .map()
            .iter()
            .map(|(id, t)| EngineSnapshot {
                id: id.clone(),
                state: t.state,
                finished: t.finished,
                stats: t.stats,
                error: t.error.clone(),
                name: t.name.clone(),
            })
            .collect();
        // Deterministic order so snapshot-driven tests never flake on HashMap
        // iteration order.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req(id: &str) -> AddRequest {
        AddRequest {
            id: id.into(),
            magnet: format!("magnet:?xt=urn:btih:{id}"),
            dir: PathBuf::from("/tmp"),
            trackers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn add_is_idempotent_and_rejects_an_empty_magnet() {
        let e = FakeEngine::new();
        e.add(req("a")).await.expect("first add");
        e.add(req("a")).await.expect("re-add is a no-op");
        assert_eq!(e.len(), 1);

        let mut bad = req("b");
        bad.magnet.clear();
        assert_eq!(
            e.add(bad).await,
            Err(EngineError::InvalidInput("empty magnet".into()))
        );
    }

    #[tokio::test]
    async fn a_one_shot_add_failure_does_not_stick() {
        let e = FakeEngine::new();
        e.fail_next_add(EngineError::Unavailable("no port".into()));
        assert!(e.add(req("a")).await.is_err());
        e.add(req("b")).await.expect("the next add succeeds");
        assert_eq!(e.len(), 1);
    }

    #[tokio::test]
    async fn pause_clears_peers_and_eta_rather_than_zeroing_them() {
        let e = FakeEngine::new();
        e.add(req("a")).await.unwrap();
        e.set_progress("a", 0.5, 4.0);
        assert_eq!(e.snapshot()[0].stats.peers, Some(12));

        e.pause("a").await.unwrap();
        let snap = &e.snapshot()[0];
        assert_eq!(snap.state, EngineItemState::Paused);
        assert_eq!(snap.stats.peers, None, "unknown while paused, not zero");
        assert_eq!(snap.stats.eta, None);
    }

    #[tokio::test]
    async fn pause_and_resume_report_not_found_for_unknown_ids() {
        let e = FakeEngine::new();
        assert_eq!(e.pause("nope").await, Err(EngineError::NotFound));
        assert_eq!(e.resume("nope").await, Err(EngineError::NotFound));
    }

    #[tokio::test]
    async fn removing_an_unknown_id_succeeds() {
        // The queue cleans up on crash paths and must not have to check first.
        let e = FakeEngine::new();
        e.remove("nope", false).await.expect("idempotent remove");
    }

    #[tokio::test]
    async fn snapshot_is_ordered_so_tests_cannot_flake() {
        let e = FakeEngine::new();
        for id in ["c", "a", "b"] {
            e.add(req(id)).await.unwrap();
        }
        let ids: Vec<String> = e.snapshot().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn drivers_move_the_state_machine_as_a_real_engine_would() {
        let e = FakeEngine::new();
        e.add(req("a")).await.unwrap();
        assert_eq!(e.snapshot()[0].state, EngineItemState::Initializing);

        e.deliver_metadata("a", "Example", 1000);
        let snap = &e.snapshot()[0];
        assert_eq!(snap.state, EngineItemState::Live);
        assert_eq!(snap.name.as_deref(), Some("Example"));
        assert_eq!(snap.stats.total_bytes, 1000);

        e.complete("a");
        let snap = &e.snapshot()[0];
        assert!(snap.finished);
        assert_eq!(snap.stats.progress, 1.0);
        assert_eq!(snap.stats.downloaded_bytes, 1000);

        e.lose_files("a");
        let snap = &e.snapshot()[0];
        assert!(!snap.finished);
        assert!(snap.stats.progress < 1.0 && snap.stats.speed_mib > 0.0);

        e.fail("a", "tracker exploded");
        let snap = &e.snapshot()[0];
        assert_eq!(snap.state, EngineItemState::Errored);
        assert_eq!(snap.error.as_deref(), Some("tracker exploded"));
    }
}
