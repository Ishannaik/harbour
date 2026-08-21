//! Search orchestration: fan-out, the deadline budget, and merging.
//!
//! The engine owns this rather than the UI (`docs/plan-engine.md` §10 D1). The
//! per-source deadlines and the disabled-site set are session state, and the
//! merge reads all of it — splitting the merge into another layer would mean
//! it reads state it does not own.
//!
//! What the UI gets is one already-merged, already-deduplicated, already-sorted
//! list plus per-source status events. It replaces its list wholesale; it never
//! reconciles per-source batches.
//!
//! The shape of a search:
//!
//! 1. Every source starts at once, each with its own deadline and cancellation.
//! 2. A source the user disabled is skipped rather than spawned.
//! 3. Results stream: each answer re-merges and emits the whole list, so the UI
//!    fills in as sources land.
//! 4. At the list deadline the UI stops waiting; sources still running stay
//!    `Checking`, **not** `Offline`, and their results still land if they arrive.
//!
//! There is no client-side cache: it moved to the indexer with the scrapers
//! (`docs/architecture.md`), so repeated queries hit the indexer again.

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc::UnboundedSender;

use crate::core::cancel::CancelToken;
use crate::core::types::{
    ArcSource, EngineEvent, SearchCtx, SourceId, SourceStatus, TorrentResult,
};

/// Merges results from every source into the one list the UI renders.
///
/// Deduplicated by infohash, **keeping the copy that reports more seeders** —
/// the same film indexed by three sources is one row, and we keep the healthiest
/// report of it. Ordered by seeders descending, then newest first, so the row
/// most likely to actually download sits at the top.
///
/// A source with `reports_health == false` reports `seeders: 0` meaning
/// *unknown*, not *dead*; because dedup keeps the higher count, a real count
/// from another source naturally wins, which is exactly what we want.
pub fn merge(results: Vec<TorrentResult>) -> Vec<TorrentResult> {
    let mut by_hash: HashMap<String, TorrentResult> = HashMap::new();
    for result in results {
        match by_hash.get(&result.info_hash) {
            Some(existing) if existing.seeders >= result.seeders => {}
            _ => {
                by_hash.insert(result.info_hash.clone(), result);
            }
        }
    }
    let mut out: Vec<TorrentResult> = by_hash.into_values().collect();
    out.sort_by(|a, b| {
        b.seeders
            .cmp(&a.seeders)
            .then_with(|| b.added.unwrap_or(0).cmp(&a.added.unwrap_or(0)))
            // Names last so the order is total and the list never shuffles
            // between two otherwise-equal rows.
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Runs searches across the registry.
pub struct SearchEngine {
    sources: Vec<ArcSource>,
    /// Sources the user disabled in the sidebar: never queried, never merged.
    /// Empty = everything enabled.
    disabled: HashSet<SourceId>,
}

impl SearchEngine {
    pub fn new(sources: Vec<ArcSource>) -> Self {
        Self {
            sources,
            disabled: HashSet::new(),
        }
    }

    /// Replaces the disabled-source set before a search. The app owns the
    /// truth (`Config.disabled_sources`); this is the engine's read-only view
    /// so `start` can skip a disabled source before it is spawned and hand the
    /// set to the sources that do run (the `HttpSource` sends it as `exclude`).
    pub fn set_disabled(&mut self, disabled: HashSet<SourceId>) {
        self.disabled = disabled;
    }

    /// The registry, so a caller can ask the owning source to resolve a magnet
    /// it did not supply at search time.
    #[allow(
        dead_code,
        reason = "used by app.rs; the network test includes this module standalone"
    )]
    pub fn sources(&self) -> &[ArcSource] {
        &self.sources
    }

    /// Per-site health the sources reported on their last search, merged into
    /// one map. With the lone `HttpSource` this is the indexer's `sources`
    /// array; a source that never reports contributes nothing.
    pub fn reported_source_health(&self) -> HashMap<SourceId, (SourceStatus, u32)> {
        let mut all: HashMap<SourceId, (SourceStatus, u32)> = HashMap::new();
        for source in &self.sources {
            for (id, report) in source.reported_source_health() {
                all.insert(id, report);
            }
        }
        all
    }

    /// Starts a search. Returns the token that cancels it.
    ///
    /// Cancellation is the caller's handle on `FR-20`: a new query cancels the
    /// previous one, and stale answers are dropped rather than appended.
    pub fn start(
        &self,
        query: String,
        ctx: SearchCtx,
        events: UnboundedSender<EngineEvent>,
    ) -> CancelToken {
        let cancel = ctx.cancel.clone();

        for source in &self.sources {
            let source = source.clone();
            let id = source.def().id;
            // A source the user disabled is not queried at all — no fetch, no
            // events, so its sidebar dot stays unknown. This also handles the
            // `SourceId::Indexer` toggle (the whole-source switch).
            if self.disabled.contains(&id) {
                continue;
            }
            let query = query.clone();
            let events = events.clone();
            let mut ctx = ctx.clone();
            // Hand the disabled-site set to the source (the HttpSource turns it
            // into the `exclude` param), so the user's toggles reach the indexer.
            ctx.disabled = self.disabled.clone();

            tokio::spawn(run_source_search(source, id, query, ctx, events));
        }

        cancel
    }
}

/// Drives one source's whole search inside its spawn task: the status event,
/// the fetch, then the outcome events.
///
/// Module-level rather than a closure so the per-source nesting stays within
/// the budget — the future passed to `tokio::spawn` is a single call.
async fn run_source_search(
    source: ArcSource,
    id: SourceId,
    query: String,
    ctx: SearchCtx,
    events: UnboundedSender<EngineEvent>,
) {
    let _ = id;
    source.search_into_events(&query, &ctx, events).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::SourceError;
    use crate::core::types::{MagnetFuture, SearchFuture, Source, SourceDef, SourceGroup};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn result(hash: &str, seeders: u32, source: SourceId, added: i64) -> TorrentResult {
        TorrentResult {
            info_hash: hash.to_owned(),
            name: format!("row-{hash}"),
            size_bytes: 100,
            seeders,
            leechers: 0,
            num_files: None,
            source,
            magnet: Some(format!("magnet:?xt=urn:btih:{hash}")),
            added: Some(added),
        }
    }

    #[test]
    fn the_same_torrent_from_three_sources_becomes_one_row() {
        let merged = merge(vec![
            result("aaa", 10, SourceId::VaultMovies, 1),
            result("aaa", 250, SourceId::ReelSource, 1),
            result("aaa", 90, SourceId::TorrentHub, 1),
        ]);
        assert_eq!(merged.len(), 1, "one film, one row");
        assert_eq!(merged[0].seeders, 250, "the healthiest report wins");
        assert_eq!(merged[0].source, SourceId::ReelSource);
    }

    #[test]
    fn a_source_that_reports_no_health_loses_to_one_that_does() {
        // GamesHub's feed carries no swarm data, so its 0 means "unknown". A real
        // count from another source must win rather than being averaged in.
        let merged = merge(vec![
            result("aaa", 0, SourceId::GamesHub, 1),
            result("aaa", 42, SourceId::VaultMovies, 1),
        ]);
        assert_eq!(merged[0].seeders, 42);
    }

    #[test]
    fn ordering_is_seeders_then_newest_then_stable() {
        let merged = merge(vec![
            result("aaa", 5, SourceId::CineVault, 100),
            result("bbb", 50, SourceId::CineVault, 1),
            result("ccc", 5, SourceId::CineVault, 900),
        ]);
        let hashes: Vec<&str> = merged.iter().map(|r| r.info_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["bbb", "ccc", "aaa"],
            "seeders first, then newest"
        );
    }

    #[test]
    fn merging_is_deterministic_for_equal_rows() {
        // Two rows identical but for their name must not swap between renders.
        let mut a = result("aaa", 5, SourceId::CineVault, 1);
        let mut b = result("bbb", 5, SourceId::CineVault, 1);
        a.name = "zebra".into();
        b.name = "apple".into();
        let first = merge(vec![a.clone(), b.clone()]);
        let second = merge(vec![b, a]);
        assert_eq!(first, second, "merge must be order-independent");
        assert_eq!(first[0].name, "apple");
    }

    #[test]
    fn merging_nothing_is_not_an_error() {
        assert!(merge(Vec::new()).is_empty());
    }

    // --- a scripted source, so orchestration is testable without a network ---

    struct ScriptedSource {
        def: &'static SourceDef,
        rows: Vec<TorrentResult>,
        error: Option<SourceError>,
        delay: Duration,
        calls: Arc<AtomicU32>,
    }

    impl Source for ScriptedSource {
        fn def(&self) -> &'static SourceDef {
            self.def
        }
        fn search<'a>(&'a self, _q: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err(SourceError::Cancelled),
                    _ = tokio::time::sleep(self.delay) => {}
                }
                match &self.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(self.rows.clone()),
                }
            })
        }
        fn resolve_magnet<'a>(
            &'a self,
            r: &'a TorrentResult,
            _c: &'a SearchCtx,
        ) -> MagnetFuture<'a> {
            let m = r.magnet.clone();
            Box::pin(async move { m.ok_or(SourceError::Timeout) })
        }
    }

    static CINEVAULT_DEF: SourceDef = SourceDef {
        id: SourceId::CineVault,
        label: "CineVault",
        groups: &[SourceGroup::Movies],
        homepage: "https://cinevault.mx",
        reports_health: true,
    };
    static VAULTINDEX_DEF: SourceDef = SourceDef {
        id: SourceId::VaultMovies,
        label: "VaultIndex",
        groups: &[SourceGroup::Movies],
        homepage: "https://mirror-api.org",
        reports_health: true,
    };

    fn collect(rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// Drains events until `want` of them have arrived, or the deadline passes.
    async fn drain_until(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
        want: usize,
        deadline: Duration,
    ) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        let end = tokio::time::Instant::now() + deadline;
        while out.len() < want && tokio::time::Instant::now() < end {
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(e)) => out.push(e),
                Ok(None) => break,
                Err(_) => {}
            }
        }
        out.extend(collect(rx));
        out
    }

    #[tokio::test]
    async fn one_failing_source_never_stops_the_others() {
        let good = Arc::new(ScriptedSource {
            def: &CINEVAULT_DEF,
            rows: vec![result("aaa", 10, SourceId::CineVault, 1)],
            error: None,
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });
        let bad = Arc::new(ScriptedSource {
            def: &VAULTINDEX_DEF,
            rows: Vec::new(),
            error: Some(SourceError::Blocked("cf".into())),
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });

        let engine = SearchEngine::new(vec![good, bad]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.start(String::new(), SearchCtx::default(), tx);
        // Two Checking, one SourceAnswered + SourceResults, one SourceFailed.
        let events = drain_until(&mut rx, 5, Duration::from_secs(5)).await;
        assert!(
            events.iter().any(
                |e| matches!(e, EngineEvent::SourceResults { source, results }
                if *source == SourceId::CineVault && results.len() == 1)
            ),
            "the healthy source still answered"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, EngineEvent::SourceFailed { source, class, .. }
                if *source == SourceId::VaultMovies && *class == "blocked")
            ),
            "and the blocked one reported why"
        );
    }

    #[tokio::test]
    async fn a_disabled_source_is_never_queried() {
        let cinevault = Arc::new(ScriptedSource {
            def: &CINEVAULT_DEF,
            rows: vec![result("aaa", 10, SourceId::CineVault, 1)],
            error: None,
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });
        let vault_index = Arc::new(ScriptedSource {
            def: &VAULTINDEX_DEF,
            rows: vec![result("bbb", 5, SourceId::VaultMovies, 1)],
            error: None,
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });

        let mut engine = SearchEngine::new(vec![cinevault, vault_index.clone()]);
        engine.set_disabled(HashSet::from([SourceId::VaultMovies]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.start(String::new(), SearchCtx::default(), tx);
        let events = drain_until(&mut rx, 3, Duration::from_secs(5)).await;
        assert_eq!(
            vault_index.calls.load(Ordering::SeqCst),
            0,
            "a disabled source is never asked, not even once"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                EngineEvent::SourceResults { source, .. } if *source == SourceId::VaultMovies
            )),
            "a disabled source must not merge results"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                EngineEvent::SourceResults { source, .. } if *source == SourceId::CineVault
            )),
            "the enabled source still answers"
        );
    }

    #[tokio::test]
    async fn a_source_reports_checking_before_it_answers() {
        // Without this the sidebar cannot distinguish "still working" from
        // "dead", which is the whole reason SourceStatus::Checking exists.
        let slow = Arc::new(ScriptedSource {
            def: &CINEVAULT_DEF,
            rows: Vec::new(),
            error: None,
            delay: Duration::from_millis(200),
            calls: Arc::new(AtomicU32::new(0)),
        });
        let engine = SearchEngine::new(vec![slow]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.start(String::new(), SearchCtx::default(), tx);

        tokio::time::sleep(Duration::from_millis(50)).await;
        let early = collect(&mut rx);
        assert!(
            early
                .iter()
                .any(|e| matches!(e, EngineEvent::SourceStatus { status, .. }
                if *status == SourceStatus::Checking)),
            "a source in flight must announce itself as checking"
        );
    }

    #[tokio::test]
    async fn a_cancelled_search_emits_nothing() {
        let slow = Arc::new(ScriptedSource {
            def: &CINEVAULT_DEF,
            rows: vec![result("aaa", 1, SourceId::CineVault, 1)],
            error: None,
            delay: Duration::from_millis(300),
            calls: Arc::new(AtomicU32::new(0)),
        });
        let engine = SearchEngine::new(vec![slow]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = engine.start(String::new(), SearchCtx::default(), tx);

        let _ = drain_until(&mut rx, 1, Duration::from_secs(5)).await; // the Checking event
        cancel.cancel();
        // Well past the scripted source's 300ms delay, so a leaked result would
        // have had every chance to arrive.
        tokio::time::sleep(Duration::from_millis(600)).await;

        let after = collect(&mut rx);
        assert!(
            !after
                .iter()
                .any(|e| matches!(e, EngineEvent::SourceResults { .. })),
            "stale results from a replaced query must never reach the UI"
        );
    }
}
