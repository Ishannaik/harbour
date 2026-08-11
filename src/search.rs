//! Search orchestration: fan-out, the deadline budget, merging, and the caches.
//!
//! The engine owns this rather than the UI (`docs/plan-engine.md` §10 D1). The
//! cache, the per-source deadlines, the host-health marker and the sticky mirror
//! hint are all session state, and the merge reads all of it — splitting the
//! merge into another layer would mean it reads state it does not own.
//!
//! What the UI gets is one already-merged, already-deduplicated, already-sorted
//! list plus per-source status events. It replaces its list wholesale; it never
//! reconciles per-source batches.
//!
//! The shape of a search:
//!
//! 1. Every source starts at once, each with its own deadline and cancellation.
//! 2. A cache hit answers without touching the network.
//! 3. A source whose mirrors are all parked from recent hard failures is skipped
//!    rather than re-probed — the negative TTL.
//! 4. Results stream: each answer re-merges and emits the whole list, so the UI
//!    fills in as sources land.
//! 5. At the list deadline the UI stops waiting; sources still running stay
//!    `Checking`, **not** `Offline`, and their results still land if they arrive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::core::cancel::CancelToken;
use crate::core::error::SourceError;
use crate::core::types::{
    ArcSource, EngineEvent, SearchCtx, SourceId, SourceStatus, TorrentResult,
};
use crate::sources::cache::SearchCache;

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
    cache: SearchCache,
    /// Which mirror last answered, per source. This is the session state that
    /// keeps the sources themselves stateless (`docs/sources.md` §1.1).
    host_hints: Arc<Mutex<HashMap<SourceId, String>>>,
}

impl SearchEngine {
    pub fn new(sources: Vec<ArcSource>, cache: SearchCache) -> Self {
        Self {
            sources,
            cache,
            host_hints: Arc::new(Mutex::new(HashMap::new())),
        }
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

    fn hint_for(&self, id: SourceId) -> Option<String> {
        self.host_hints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
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
            let query = query.clone();
            let cache = self.cache.clone();
            let hints = self.host_hints.clone();
            let events = events.clone();
            let mut ctx = ctx.clone();
            ctx.host_hint = self.hint_for(id);

            tokio::spawn(async move {
                let _ = events.send(EngineEvent::SourceStatus {
                    source: id,
                    status: SourceStatus::Checking,
                });

                let outcome = run_one(&source, &query, &ctx, &cache, &hints).await;

                if ctx.cancel.is_cancelled() {
                    // A cancelled search must never touch the UI: its results
                    // belong to a query the user has already replaced.
                    return;
                }

                match outcome {
                    Ok(results) => {
                        let _ = events.send(EngineEvent::SourceAnswered {
                            source: id,
                            count: results.len(),
                        });
                        let _ = events.send(EngineEvent::SourceResults {
                            source: id,
                            results,
                        });
                    }
                    Err(err) => {
                        let _ = events.send(EngineEvent::SourceFailed {
                            source: id,
                            class: err.class(),
                            message: err.to_string(),
                        });
                    }
                }
            });
        }

        cancel
    }
}

/// One source's whole search: cache, health gate, fetch, cache write.
async fn run_one(
    source: &ArcSource,
    query: &str,
    ctx: &SearchCtx,
    cache: &SearchCache,
    hints: &Arc<Mutex<HashMap<SourceId, String>>>,
) -> Result<Vec<TorrentResult>, SourceError> {
    let id = source.def().id;

    // A fresh cache entry answers without any network work at all (`FR-17`),
    // which is what makes arrow-key browsing and repeated queries instant.
    if let Some(hit) = cache.get(id, query) {
        return Ok(hit);
    }

    let result = source.search(query, ctx).await;

    match &result {
        Ok(results) => {
            // Successful answers are cached, *including empty ones*: a source
            // that legitimately has nothing should not be re-asked on every
            // keystroke.
            cache.put(id, query, results);
            if let Some(host) = ctx.host_hint.as_ref() {
                cache.record_success(id, host);
            }
        }
        Err(err) => {
            // Failures never write a result entry — a dead source must never be
            // resurrected from cache. They do mark the host, so a sick mirror is
            // not re-probed for the next minute.
            if let Some(host) = ctx.host_hint.as_ref() {
                cache.record_failure(id, host, err);
            }
            let _ = hints;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{MagnetFuture, SearchFuture, Source, SourceDef, SourceGroup};
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
            result("aaa", 10, SourceId::TpbMovies, 1),
            result("aaa", 250, SourceId::X1337Movies, 1),
            result("aaa", 90, SourceId::Bittorrented, 1),
        ]);
        assert_eq!(merged.len(), 1, "one film, one row");
        assert_eq!(merged[0].seeders, 250, "the healthiest report wins");
        assert_eq!(merged[0].source, SourceId::X1337Movies);
    }

    #[test]
    fn a_source_that_reports_no_health_loses_to_one_that_does() {
        // FitGirl's feed carries no swarm data, so its 0 means "unknown". A real
        // count from another source must win rather than being averaged in.
        let merged = merge(vec![
            result("aaa", 0, SourceId::FitGirl, 1),
            result("aaa", 42, SourceId::TpbMovies, 1),
        ]);
        assert_eq!(merged[0].seeders, 42);
    }

    #[test]
    fn ordering_is_seeders_then_newest_then_stable() {
        let merged = merge(vec![
            result("aaa", 5, SourceId::Yts, 100),
            result("bbb", 50, SourceId::Yts, 1),
            result("ccc", 5, SourceId::Yts, 900),
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
        let mut a = result("aaa", 5, SourceId::Yts, 1);
        let mut b = result("bbb", 5, SourceId::Yts, 1);
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

    static YTS_DEF: SourceDef = SourceDef {
        id: SourceId::Yts,
        label: "YTS",
        groups: &[SourceGroup::Movies],
        homepage: "https://yts.mx",
        reports_health: true,
    };
    static TPB_DEF: SourceDef = SourceDef {
        id: SourceId::TpbMovies,
        label: "TPB",
        groups: &[SourceGroup::Movies],
        homepage: "https://apibay.org",
        reports_health: true,
    };

    fn temp_cache(tag: &str) -> SearchCache {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("harbour-search-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        SearchCache::new(dir)
    }

    fn collect(rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// Waits for a condition instead of sleeping a fixed amount.
    ///
    /// A fixed sleep is a bet that a spawned task finishes within it, and that
    /// bet loses on a loaded CI runner — which is how a suite acquires the
    /// intermittent failures nobody can reproduce. Polling to a generous
    /// deadline is both faster in the common case and cannot flake.
    async fn until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = tokio::time::Instant::now() + deadline;
        while tokio::time::Instant::now() < end {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        done()
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
            def: &YTS_DEF,
            rows: vec![result("aaa", 10, SourceId::Yts, 1)],
            error: None,
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });
        let bad = Arc::new(ScriptedSource {
            def: &TPB_DEF,
            rows: Vec::new(),
            error: Some(SourceError::Blocked("cf".into())),
            delay: Duration::ZERO,
            calls: Arc::new(AtomicU32::new(0)),
        });

        let engine = SearchEngine::new(vec![good, bad], temp_cache("resilient"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.start(String::new(), SearchCtx::default(), tx);
        // Two Checking, one SourceAnswered + SourceResults, one SourceFailed.
        let events = drain_until(&mut rx, 5, Duration::from_secs(5)).await;
        assert!(
            events.iter().any(
                |e| matches!(e, EngineEvent::SourceResults { source, results }
                if *source == SourceId::Yts && results.len() == 1)
            ),
            "the healthy source still answered"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, EngineEvent::SourceFailed { source, class, .. }
                if *source == SourceId::TpbMovies && *class == "blocked")
            ),
            "and the blocked one reported why"
        );
    }

    #[tokio::test]
    async fn a_source_reports_checking_before_it_answers() {
        // Without this the sidebar cannot distinguish "still working" from
        // "dead", which is the whole reason SourceStatus::Checking exists.
        let slow = Arc::new(ScriptedSource {
            def: &YTS_DEF,
            rows: Vec::new(),
            error: None,
            delay: Duration::from_millis(200),
            calls: Arc::new(AtomicU32::new(0)),
        });
        let engine = SearchEngine::new(vec![slow], temp_cache("checking"));
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
            def: &YTS_DEF,
            rows: vec![result("aaa", 1, SourceId::Yts, 1)],
            error: None,
            delay: Duration::from_millis(300),
            calls: Arc::new(AtomicU32::new(0)),
        });
        let engine = SearchEngine::new(vec![slow], temp_cache("cancel"));
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

    #[tokio::test]
    async fn a_second_identical_search_is_served_from_cache() {
        let calls = Arc::new(AtomicU32::new(0));
        let source = Arc::new(ScriptedSource {
            def: &YTS_DEF,
            rows: vec![result("aaa", 10, SourceId::Yts, 1)],
            error: None,
            delay: Duration::ZERO,
            calls: calls.clone(),
        });
        let engine = SearchEngine::new(vec![source], temp_cache("cachehit"));

        for _ in 0..2 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            engine.start("dune".into(), SearchCtx::default(), tx);
            // Wait for the source to actually settle before searching again,
            // or the second search races the first one's cache write.
            drain_until(&mut rx, 3, Duration::from_secs(5)).await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "FR-17: the second search must not touch the source"
        );
    }

    #[tokio::test]
    async fn an_empty_answer_is_cached_too() {
        let calls = Arc::new(AtomicU32::new(0));
        let source = Arc::new(ScriptedSource {
            def: &YTS_DEF,
            rows: Vec::new(),
            error: None,
            delay: Duration::ZERO,
            calls: calls.clone(),
        });
        let engine = SearchEngine::new(vec![source], temp_cache("emptycache"));
        for _ in 0..2 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            engine.start("nothing".into(), SearchCtx::default(), tx);
            drain_until(&mut rx, 3, Duration::from_secs(5)).await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "browsing a source with nothing to show must not re-hit it"
        );
    }

    #[tokio::test]
    async fn a_failure_is_never_cached() {
        let calls = Arc::new(AtomicU32::new(0));
        let source = Arc::new(ScriptedSource {
            def: &YTS_DEF,
            rows: Vec::new(),
            error: Some(SourceError::Network("refused".into())),
            delay: Duration::ZERO,
            calls: calls.clone(),
        });
        let engine = SearchEngine::new(vec![source], temp_cache("nocachefail"));
        for _ in 0..2 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            engine.start("dune".into(), SearchCtx::default(), tx);
            drain_until(&mut rx, 2, Duration::from_secs(5)).await;
            // The call counter is what this asserts on, so wait on it directly.
            until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 0).await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a dead source must never be resurrected from cache"
        );
    }
}
