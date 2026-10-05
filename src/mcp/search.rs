//! Search tool implementation for the MCP server.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::paths;
use crate::core::types::{EngineEvent, SearchCtx, SourceId, TorrentResult};
use crate::persist::Store;
use crate::search::SearchEngine;
use crate::sources::HttpSource;

/// Serialized torrent search result returned to MCP clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub name: String,
    pub info_hash: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    pub source: String,
    pub magnet: Option<String>,
}

impl From<TorrentResult> for SearchResult {
    fn from(r: TorrentResult) -> Self {
        // Synthesize magnet link from canonical infohash when omitted by detail-page sources.
        let magnet = r.magnet.or_else(|| {
            if crate::core::magnet::is_info_hash(&r.info_hash) {
                Some(crate::core::magnet::build_magnet(&r.info_hash, &r.name))
            } else {
                None
            }
        });
        Self {
            name: r.name,
            info_hash: r.info_hash,
            size_bytes: r.size_bytes,
            seeders: r.seeders,
            leechers: r.leechers,
            source: r.source.as_str().to_string(),
            magnet,
        }
    }
}

/// Abstract search handler allowing mock result injection in tests.
pub trait SearchHandler: Send + Sync {
    /// Executes a search query, returning results or an error message.
    fn search<'a>(
        &'a self,
        query: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>, String>> + Send + 'a>>;
}

impl<F, Fut> SearchHandler for F
where
    F: Fn(&str) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Vec<SearchResult>, String>> + Send + 'static,
{
    fn search<'a>(
        &'a self,
        query: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>, String>> + Send + 'a>> {
        Box::pin(self(query))
    }
}

/// Production search handler backed by the local indexer and `SearchEngine`.
pub struct LiveSearch {
    engine: SearchEngine,
    disabled: HashSet<SourceId>,
}

impl LiveSearch {
    /// Loads configuration, ensures indexer availability, and prepares the engine.
    pub async fn new() -> Self {
        let store = Store::from_env();
        let config = store.load_config().value();
        crate::ensure_indexer::ensure_local_indexer().await;

        let disabled: HashSet<SourceId> = config.disabled_sources.iter().copied().collect();
        let mut engine =
            SearchEngine::new(vec![Arc::new(HttpSource::new(config.indexer_url.clone()))]);
        engine.set_disabled(disabled.clone());

        Self { engine, disabled }
    }
}

fn store_source_results(
    source: SourceId,
    results: Vec<TorrentResult>,
    disabled: &HashSet<SourceId>,
    partial: &mut HashMap<SourceId, Vec<TorrentResult>>,
) {
    if source == SourceId::Indexer {
        for r in results
            .into_iter()
            .filter(|r| !disabled.contains(&r.source))
        {
            partial.entry(r.source).or_default().push(r);
        }
    } else if !disabled.contains(&source) {
        partial.insert(source, results);
    }
}

fn apply_search_event(
    event: EngineEvent,
    disabled: &HashSet<SourceId>,
    partial: &mut HashMap<SourceId, Vec<TorrentResult>>,
    finished_sites: &mut HashSet<SourceId>,
    indexer_error: &mut Option<String>,
) {
    match event {
        EngineEvent::SourceResults { source, results } => {
            store_source_results(source, results, disabled, partial);
        }
        EngineEvent::SourceAnswered { source, .. } => {
            if source != SourceId::Indexer {
                finished_sites.insert(source);
            }
        }
        EngineEvent::SourceFailed {
            source, message, ..
        } => {
            if source == SourceId::Indexer {
                *indexer_error = Some(message);
            } else {
                finished_sites.insert(source);
            }
        }
        _ => {}
    }
}

async fn collect_events(
    mut rx: UnboundedReceiver<EngineEvent>,
    enabled: &HashSet<SourceId>,
    disabled: &HashSet<SourceId>,
    timeout: Duration,
) -> (Vec<TorrentResult>, Option<String>) {
    let mut partial: HashMap<SourceId, Vec<TorrentResult>> = HashMap::new();
    let mut finished: HashSet<SourceId> = HashSet::new();
    let mut error: Option<String> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    // A failed indexer sends nothing more, so stop waiting instead of running out the clock.
    while error.is_none()
        && !finished.is_superset(enabled)
        && tokio::time::Instant::now() < deadline
    {
        let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = match tokio::time::timeout(remain, rx.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) | Err(_) => break,
        };
        apply_search_event(event, disabled, &mut partial, &mut finished, &mut error);
    }

    let all: Vec<TorrentResult> = partial.into_values().flatten().collect();
    (all, error)
}

impl SearchHandler for LiveSearch {
    fn search<'a>(
        &'a self,
        query: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>, String>> + Send + 'a>> {
        Box::pin(async move {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let timeout = paths::source_timeout();
            let ctx = SearchCtx {
                total_deadline: timeout,
                ..SearchCtx::default()
            };
            let _cancel = self.engine.start(query.to_string(), ctx, tx);

            let enabled: HashSet<SourceId> = SourceId::ALL
                .iter()
                .copied()
                .filter(|id| !self.disabled.contains(id))
                .collect();
            if enabled.is_empty() {
                return Ok(Vec::new());
            }

            let (all, error) = collect_events(rx, &enabled, &self.disabled, timeout).await;
            if let Some(err) = error.filter(|_| all.is_empty()) {
                return Err(err);
            }

            let merged = crate::search::merge(all);
            Ok(merged.into_iter().map(SearchResult::from).collect())
        })
    }
}
