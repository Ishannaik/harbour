//! The real torrent engine: a [`Engine`] adapter over librqbit's `Session`.
//!
//! Everything librqbit-shaped is confined to this file. The queue, the UI and
//! the tests only ever see `core::types`, which is what let the whole queue be
//! written and tested before this existed — and what keeps the blast radius of
//! an 8→9 upgrade to one module.
//!
//! Three mappings are worth reading before changing anything:
//!
//! * **Peers** live at `stats.live.snapshot.peer_stats.live`, three levels down
//!   and only while the torrent is live. That is why `EngineStats::peers` is an
//!   `Option`: a paused torrent genuinely has no peer count, and reporting `0`
//!   would be a lie the UI cannot distinguish from a real zero.
//! * **ETA is computed here, not read.** librqbit exposes `time_remaining` only
//!   as an opaque display type whose inner `Duration` is private, so we derive
//!   it from remaining bytes and the current speed. One converter, in one place.
//! * **Speeds are MiB/s**, because that is the unit librqbit reports. The
//!   conversion happens here so no second converter exists in the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::dht::Id20;
use librqbit::http_api::HttpApi;
use librqbit::{
    AddTorrent, AddTorrentOptions, Api, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig,
};

/// librqbit's own alias is not re-exported at the crate root, and it is just an
/// `Arc` over the managed torrent — spelling it out avoids depending on a
/// private module path that could move between minor versions.
type ManagedTorrentHandle = Arc<ManagedTorrent>;

use crate::core::error::EngineError;
use crate::core::types::{
    AddRequest, Engine, EngineFuture, EngineItemState, EngineSnapshot, EngineStats, InfoHash,
};

/// Bytes in a MiB, for the speed→ETA conversion.
const MIB: f64 = 1024.0 * 1024.0;

/// Below this speed an ETA is meaningless — dividing by a near-zero rate
/// produces "four years remaining", which is worse than showing nothing.
const MIN_SPEED_FOR_ETA_MIB: f64 = 0.01;

pub struct RqbitEngine {
    session: Arc<Session>,
    /// Handles we have added, keyed by our own lowercase-hex infohash so the
    /// queue never has to know about librqbit's `TorrentId`.
    handles: Mutex<HashMap<InfoHash, ManagedTorrentHandle>>,
    /// The loopback stream server (FR-57), started lazily on first watch.
    stream: Mutex<Option<Arc<StreamServer>>>,
}

/// One running librqbit HTTP API — the Stremio-style stream server. Binds
/// loopback only (FR-61), on a random port; the player talks to it directly,
/// pulling pieces as they arrive while the torrent downloads.
struct StreamServer {
    base_url: String,
}

impl RqbitEngine {
    /// Ensures the loopback HTTP API is running and returns it. Idempotent:
    /// the first watch starts it, every later watch reuses it.
    async fn stream_server(&self) -> Option<Arc<StreamServer>> {
        if let Some(server) = self.stream.lock().unwrap().clone() {
            return Some(server);
        }
        let api = Api::new(self.session.clone(), None, None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
        let port = listener.local_addr().ok()?.port();
        let base_url = format!("http://127.0.0.1:{port}");
        // The HTTP API owns the listener and its state; run it for the
        // session's lifetime.
        tokio::spawn(async move {
            let http = HttpApi::new(api, None);
            let _ = http.make_http_api_and_run(listener, None).await;
        });
        let server = Arc::new(StreamServer { base_url });
        *self.stream.lock().unwrap() = Some(server.clone());
        Some(server)
    }

    /// The stream URL for `id`'s largest video file, if the swarm can serve
    /// it. The URL is stable; the player opens it and librqbit blocks on
    /// missing pieces while prioritizing the requested ones.
    async fn stream_url_for(&self, id: &str) -> Option<String> {
        let server = self.stream_server().await?;
        let handle = self.handles.lock().unwrap().get(id).cloned()?;
        // `with_metadata` is Result-returning (metadata may not have arrived);
        // file 0 is the honest fallback for a still-resolving torrent.
        let file_id = handle
            .with_metadata(|meta| {
                meta.file_infos
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| is_video(&f.relative_filename))
                    .max_by_key(|(_, f)| f.len)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        Some(format!(
            "{}/torrents/{id}/stream/{file_id}",
            server.base_url
        ))
    }
}

/// Video extensions a torrent file must carry to be stream-watchable.
fn is_video(path: &std::path::Path) -> bool {
    const VIDEO: &[&str] = &[
        "mkv", "mp4", "avi", "mov", "webm", "m4v", "ts", "flv", "wmv", "mpg", "mpeg",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()))
}

impl RqbitEngine {
    /// Starts a session.
    ///
    /// `state_dir` holds librqbit's own persistence, which is what makes
    /// restart resume work without a rehash (`FR-35`/`FR-50`). We deliberately
    /// keep it in *our* state directory rather than librqbit's default, so
    /// `HARBOUR_STATE_DIR` relocates everything a user has, not just our half.
    pub async fn new(download_dir: &Path, state_dir: &Path) -> Result<Self, EngineError> {
        let persistence = state_dir.join("engine");
        // A missing directory is not fatal on its own — librqbit will report it
        // — but creating it here gives a clearer error than a failed session.
        if let Err(err) = std::fs::create_dir_all(&persistence) {
            return Err(EngineError::Unavailable(format!(
                "cannot create {}: {err}",
                persistence.display()
            )));
        }

        let opts = SessionOptions {
            // Restores piece state from disk instead of rehashing every file on
            // every launch — the difference between a two-second start and a
            // ten-minute one for a large library.
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(persistence),
            }),
            ..Default::default()
        };

        let session = Session::new_with_opts(download_dir.to_path_buf(), opts)
            .await
            .map_err(|e| EngineError::Unavailable(e.to_string()))?;

        Ok(Self {
            session,
            handles: Mutex::new(HashMap::new()),
            stream: Mutex::new(None),
        })
    }

    /// Poisoned locks are recovered rather than propagated: a panic elsewhere
    /// must not take the download engine down with it (`plan-engine.md` §4.1).
    fn handles(&self) -> std::sync::MutexGuard<'_, HashMap<InfoHash, ManagedTorrentHandle>> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn id_of(hash: &str) -> Result<TorrentIdOrHash, EngineError> {
        Id20::from_str(hash)
            .map(TorrentIdOrHash::Hash)
            .map_err(|_| EngineError::InvalidInput(format!("{hash} is not a 40-hex infohash")))
    }

    fn handle(&self, hash: &str) -> Option<ManagedTorrentHandle> {
        if let Some(h) = self.handles().get(hash).cloned() {
            return Some(h);
        }
        // Torrents restored by librqbit's own persistence are in the session
        // before we ever add them, so fall back to asking the session directly.
        let id = Self::id_of(hash).ok()?;
        let handle = self.session.get(id)?;
        self.handles().insert(hash.to_owned(), handle.clone());
        Some(handle)
    }

    /// Adopts every torrent librqbit restored from its own persistence.
    ///
    /// Called once after construction: without it a resumed session's torrents
    /// would be running but invisible to the queue until something touched them.
    pub fn adopt_restored(&self) -> Vec<InfoHash> {
        // `with_torrents` takes an `Fn`, not a `FnMut`, so the collection has
        // to go through a cell rather than a captured `Vec`.
        let adopted = std::cell::RefCell::new(Vec::new());
        self.session.with_torrents(|iter| {
            for (_id, handle) in iter {
                let hash = handle.info_hash().as_string();
                self.handles().insert(hash.clone(), handle.clone());
                adopted.borrow_mut().push(hash);
            }
        });
        adopted.into_inner()
    }

    /// The `.torrent` bytes for a torrent whose metadata has arrived, for the
    /// local re-seed cache (`FR-37`).
    pub fn torrent_bytes(&self, hash: &str) -> Option<Vec<u8>> {
        let handle = self.handle(hash)?;
        handle
            .with_metadata(|meta| meta.torrent_bytes.to_vec())
            .ok()
    }

    pub async fn shutdown(&self) {
        self.session.stop().await;
    }
}

/// Maps one librqbit snapshot onto our engine-neutral shape.
fn to_snapshot(hash: InfoHash, handle: &ManagedTorrentHandle) -> EngineSnapshot {
    let stats = handle.stats();

    let state = match stats.state {
        librqbit::TorrentStatsState::Initializing => EngineItemState::Initializing,
        librqbit::TorrentStatsState::Live => EngineItemState::Live,
        librqbit::TorrentStatsState::Paused => EngineItemState::Paused,
        librqbit::TorrentStatsState::Error => EngineItemState::Errored,
    };

    let live = stats.live.as_ref();
    let speed_mib = live.map_or(0.0, |l| l.download_speed.mbps);
    let upload_speed_mib = live.map_or(0.0, |l| l.upload_speed.mbps);
    // Only *live* peers count: queued and connecting ones are aspirational and
    // showing them would overstate how healthy a swarm is.
    let peers = live.map(|l| l.snapshot.peer_stats.live as u32);

    let progress = if stats.total_bytes == 0 {
        0.0
    } else {
        (stats.progress_bytes as f64 / stats.total_bytes as f64).clamp(0.0, 1.0)
    };

    EngineSnapshot {
        id: hash,
        state,
        finished: stats.finished,
        stats: EngineStats {
            progress,
            downloaded_bytes: stats.progress_bytes,
            total_bytes: stats.total_bytes,
            speed_mib,
            upload_speed_mib,
            uploaded_bytes: stats.uploaded_bytes,
            peers,
            eta: eta(stats.progress_bytes, stats.total_bytes, speed_mib),
        },
        error: stats.error.clone(),
        name: handle.name(),
    }
}

/// Remaining time from remaining bytes and current speed.
///
/// Computed rather than read: librqbit's `time_remaining` is an opaque display
/// wrapper whose inner `Duration` is private. `None` when the torrent is done,
/// when the size is unknown, or when the speed is too low to divide by — an
/// honest "unknown" beats a fabricated week.
fn eta(downloaded: u64, total: u64, speed_mib: f64) -> Option<Duration> {
    if total == 0 || downloaded >= total || speed_mib < MIN_SPEED_FOR_ETA_MIB {
        return None;
    }
    let remaining = (total - downloaded) as f64;
    let seconds = remaining / (speed_mib * MIB);
    seconds
        .is_finite()
        .then(|| Duration::from_secs_f64(seconds))
}

impl Engine for RqbitEngine {
    fn add<'a>(&'a self, req: AddRequest) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            let opts = AddTorrentOptions {
                output_folder: Some(req.dir.to_string_lossy().into_owned()),
                // Resuming onto files that already exist is the normal case for
                // us — a re-seed, or a restart mid-download. Without this
                // librqbit refuses rather than verifying what is there.
                overwrite: true,
                ..Default::default()
            };
            let response = self
                .session
                .add_torrent(AddTorrent::from_url(req.magnet.clone()), Some(opts))
                .await
                .map_err(|e| EngineError::Backend(e.to_string()))?;

            let handle = response
                .into_handle()
                .ok_or_else(|| EngineError::Backend("engine returned no torrent handle".into()))?;
            self.handles().insert(req.id, handle);
            Ok(())
        })
    }

    fn pause<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            let handle = self.handle(id).ok_or(EngineError::NotFound)?;
            self.session
                .pause(&handle)
                .await
                .map_err(|e| EngineError::Backend(e.to_string()))
        })
    }

    fn resume<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            let handle = self.handle(id).ok_or(EngineError::NotFound)?;
            self.session
                .unpause(&handle)
                .await
                .map_err(|e| EngineError::Backend(e.to_string()))
        })
    }

    fn remove<'a>(
        &'a self,
        id: &'a str,
        delete_files: bool,
    ) -> EngineFuture<'a, Result<(), EngineError>> {
        Box::pin(async move {
            self.handles().remove(id);
            let torrent = Self::id_of(id)?;
            match self.session.delete(torrent, delete_files).await {
                Ok(()) => Ok(()),
                // Removing something already gone is success: the queue cleans
                // up on crash paths and must not have to check first.
                Err(_) if self.session.get(torrent).is_none() => Ok(()),
                Err(e) => Err(EngineError::Backend(e.to_string())),
            }
        })
    }

    fn snapshot(&self) -> Vec<EngineSnapshot> {
        let handles: Vec<(InfoHash, ManagedTorrentHandle)> = self
            .handles()
            .iter()
            .map(|(hash, handle)| (hash.clone(), handle.clone()))
            .collect();
        let mut out: Vec<EngineSnapshot> = handles
            .into_iter()
            .map(|(hash, handle)| to_snapshot(hash, &handle))
            .collect();
        // Stable order so the UI list does not shuffle between frames.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn stream_url<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Option<String>> {
        Box::pin(async move { self.stream_url_for(id).await })
    }
}

/// Where librqbit keeps its session state, for callers that need to inspect or
/// clear it.
pub fn engine_state_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("engine")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_is_none_rather_than_a_fabricated_number() {
        // Unknown size, already complete, and a stalled transfer all mean "we
        // do not know" — the UI renders an em dash for each.
        assert_eq!(eta(0, 0, 5.0), None, "unknown total");
        assert_eq!(eta(100, 100, 5.0), None, "already complete");
        assert_eq!(eta(200, 100, 5.0), None, "over-complete is still done");
        assert_eq!(eta(0, 1000, 0.0), None, "stalled");
        assert_eq!(
            eta(0, u64::MAX, 0.001),
            None,
            "a near-zero speed must not produce a decade-long estimate"
        );
    }

    #[test]
    fn eta_divides_remaining_bytes_by_the_current_speed() {
        // 1 MiB remaining at 1 MiB/s is one second.
        let one_mib = 1024 * 1024;
        let d = eta(0, one_mib, 1.0).expect("computable");
        assert!(
            (d.as_secs_f64() - 1.0).abs() < 0.01,
            "expected ~1s, got {d:?}"
        );

        // Halfway through 2 MiB at 1 MiB/s is also one second.
        let d = eta(one_mib, 2 * one_mib, 1.0).expect("computable");
        assert!((d.as_secs_f64() - 1.0).abs() < 0.01, "got {d:?}");
    }

    #[test]
    fn faster_transfers_report_shorter_etas() {
        let slow = eta(0, 100 * 1024 * 1024, 1.0).expect("computable");
        let fast = eta(0, 100 * 1024 * 1024, 10.0).expect("computable");
        assert!(fast < slow);
    }

    #[test]
    fn an_infohash_that_is_not_hex_is_rejected_before_reaching_the_engine() {
        assert!(matches!(
            RqbitEngine::id_of("not-a-hash"),
            Err(EngineError::InvalidInput(_))
        ));
        assert!(RqbitEngine::id_of("0123456789abcdef0123456789abcdef01234567").is_ok());
    }

    #[test]
    fn the_engine_state_lives_under_our_state_dir_so_one_env_var_moves_everything() {
        let root = Path::new("/state");
        assert_eq!(engine_state_dir(root), Path::new("/state/engine"));
    }
}
