//! Search-result cache and per-host health markers.
//!
//! Two files, two different jobs:
//!
//! * `cache/search/<source>/<query>.json` — results for one `(source, query)`
//!   pair, with a 5-minute TTL (`FR-17`). An *empty successful* answer is cached
//!   too, so browsing top lists does not hammer a source that legitimately has
//!   nothing.
//! * `cache/health/<source>.json` — when each **host** last failed hard
//!   (`plan-engine.md` §10 D5). This is per host rather than per query because a
//!   dead mirror is a property of the host, not of what was asked; parking it in
//!   the query cache would mean a healthy mirror stays parked behind a dead one.
//!
//! Failures never write result entries: a dead source must never be resurrected
//! from cache. And every cache operation degrades to "miss" rather than
//! propagating an error — a cache is an optimisation, and a broken one must not
//! be able to fail a search.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::error::SourceError;
use crate::core::paths;
use crate::core::types::{SourceId, TorrentResult};

/// Result freshness (`FR-17`).
pub const TTL_SECS: u64 = 300;

/// How long a host stays parked after a hard failure.
///
/// Short on purpose: long enough that a keystroke-driven search does not hammer
/// a sick host, short enough that a brief outage does not hide a source for the
/// rest of the session.
pub const HEALTH_TTL_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CacheFile {
    fetched_at: u64,
    results: Vec<TorrentResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct HealthFile {
    /// host → (unix seconds, error class)
    hosts: HashMap<String, HealthEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct HealthEntry {
    failed_at: u64,
    class: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        // A clock before 1970 means something is very wrong with the machine;
        // treating it as "epoch" makes every entry look stale, which is the
        // safe direction (re-fetch rather than serve something ancient).
        .unwrap_or(0)
}

/// Cache rooted at one state directory.
#[derive(Debug, Clone)]
pub struct SearchCache {
    root: PathBuf,
}

impl SearchCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Filesystem-safe filename for a query.
    ///
    /// Percent-encoded rather than sanitized-by-deletion: two different queries
    /// must never collapse onto one cache file, and no query may produce a path
    /// separator or a traversal (`NFR-11`). The empty query — the curated
    /// browse — gets its own stable name rather than an empty filename.
    fn query_file(&self, source: SourceId, query: &str) -> PathBuf {
        let trimmed = query.trim().to_lowercase();
        let mut name = String::with_capacity(trimmed.len() + 8);
        for byte in trimmed.as_bytes() {
            match byte {
                b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => name.push(*byte as char),
                other => name.push_str(&format!("%{other:02X}")),
            }
        }
        if name.is_empty() {
            name.push_str("__browse__");
        }
        // Filesystems cap component length; a very long query would otherwise
        // fail to write and silently disable caching for that search.
        name.truncate(120);
        paths::search_cache_dir(&self.root, source).join(format!("{name}.json"))
    }

    /// Fresh results for this `(source, query)`, or `None`.
    pub fn get(&self, source: SourceId, query: &str) -> Option<Vec<TorrentResult>> {
        let raw = fs::read_to_string(self.query_file(source, query)).ok()?;
        // A schema that has drifted is a miss, not an error: the entry is
        // overwritten on the next successful fetch.
        let file: CacheFile = serde_json::from_str(&raw).ok()?;
        let age = now_secs().saturating_sub(file.fetched_at);
        (age < TTL_SECS).then_some(file.results)
    }

    /// Stores a successful answer, including an empty one.
    ///
    /// Returns nothing: a cache write that fails is invisible to the user by
    /// design. The search already succeeded.
    pub fn put(&self, source: SourceId, query: &str, results: &[TorrentResult]) {
        let path = self.query_file(source, query);
        let file = CacheFile {
            fetched_at: now_secs(),
            results: results.to_vec(),
        };
        let Ok(json) = serde_json::to_vec(&file) else {
            return;
        };
        if let Some(dir) = path.parent()
            && fs::create_dir_all(dir).is_err()
        {
            return;
        }
        let _ = crate::persist::atomic_write(&path, &json);
    }

    // --- host health --------------------------------------------------------

    fn health_path(&self, source: SourceId) -> PathBuf {
        paths::health_marker_file(&self.root, source)
    }

    fn load_health(&self, source: SourceId) -> HealthFile {
        fs::read_to_string(self.health_path(source))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Hosts that failed hard recently and should not be probed first.
    pub fn parked_hosts(&self, source: SourceId) -> Vec<String> {
        let now = now_secs();
        self.load_health(source)
            .hosts
            .into_iter()
            .filter(|(_, e)| now.saturating_sub(e.failed_at) < HEALTH_TTL_SECS)
            .map(|(host, _)| host)
            .collect()
    }

    /// True when every configured host is currently parked, so the caller can
    /// skip the source entirely instead of walking a list of known-dead mirrors.
    pub fn all_parked(&self, source: SourceId, hosts: &[&str]) -> bool {
        if hosts.is_empty() {
            return false;
        }
        let parked = self.parked_hosts(source);
        hosts.iter().all(|h| parked.iter().any(|p| p == h))
    }

    /// Records a hard failure against one host. Soft failures are ignored:
    /// [`SourceError::is_hard_host_failure`] decides, so a parse error (the
    /// source's fault, identical on every mirror) never parks a healthy host.
    pub fn record_failure(&self, source: SourceId, host: &str, err: &SourceError) {
        if !err.is_hard_host_failure() {
            return;
        }
        let mut health = self.load_health(source);
        health.hosts.insert(
            host.to_owned(),
            HealthEntry {
                failed_at: now_secs(),
                class: err.class().to_owned(),
            },
        );
        self.write_health(source, &health);
    }

    /// Clears a host's mark after it answers.
    pub fn record_success(&self, source: SourceId, host: &str) {
        let mut health = self.load_health(source);
        if health.hosts.remove(host).is_some() {
            self.write_health(source, &health);
        }
    }

    fn write_health(&self, source: SourceId, health: &HealthFile) {
        let path = self.health_path(source);
        let Ok(json) = serde_json::to_vec(health) else {
            return;
        };
        if let Some(dir) = path.parent()
            && fs::create_dir_all(dir).is_err()
        {
            return;
        }
        let _ = crate::persist::atomic_write(&path, &json);
    }

    /// Removes every cached search result, leaving health markers alone.
    pub fn clear_results(&self) -> std::io::Result<()> {
        let dir = paths::cache_dir(&self.root).join("search");
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_cache(tag: &str) -> SearchCache {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("harbour-cache-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        SearchCache::new(dir)
    }

    fn result(hash: char) -> TorrentResult {
        TorrentResult {
            info_hash: std::iter::repeat_n(hash, 40).collect(),
            name: "Example".into(),
            size_bytes: 1000,
            seeders: 10,
            leechers: 1,
            num_files: None,
            source: SourceId::Yts,
            magnet: Some("magnet:?xt=urn:btih:aaa".into()),
            added: None,
        }
    }

    #[test]
    fn a_stored_result_comes_back() {
        let cache = temp_cache("hit");
        cache.put(SourceId::Yts, "dune", &[result('a')]);
        let got = cache.get(SourceId::Yts, "dune").expect("cache hit");
        assert_eq!(got, vec![result('a')]);
    }

    #[test]
    fn queries_are_normalized_but_never_collide() {
        let cache = temp_cache("norm");
        cache.put(SourceId::Yts, "Dune", &[result('a')]);
        assert!(
            cache.get(SourceId::Yts, "  dune ").is_some(),
            "case and surrounding space are the same query"
        );
        // Two queries that a naive sanitizer would flatten together.
        cache.put(SourceId::Yts, "a b", &[result('a')]);
        cache.put(SourceId::Yts, "a/b", &[result('b')]);
        assert_ne!(
            cache.get(SourceId::Yts, "a b"),
            cache.get(SourceId::Yts, "a/b"),
            "distinct queries must not share a cache file"
        );
    }

    #[test]
    fn a_query_cannot_escape_the_cache_directory() {
        let cache = temp_cache("traversal");
        cache.put(SourceId::Yts, "../../etc/passwd", &[result('a')]);
        let escaped = cache.root().join("etc").join("passwd");
        assert!(!escaped.exists(), "NFR-11: no traversal via a query");
        assert!(cache.get(SourceId::Yts, "../../etc/passwd").is_some());
    }

    #[test]
    fn the_empty_query_caches_under_its_own_name() {
        let cache = temp_cache("browse");
        cache.put(SourceId::Yts, "", &[result('a')]);
        assert!(cache.get(SourceId::Yts, "").is_some());
        assert!(
            cache.get(SourceId::Yts, "something").is_none(),
            "browse must not answer for a real query"
        );
    }

    #[test]
    fn an_empty_but_successful_answer_is_cached() {
        // Otherwise browsing a source that legitimately has nothing re-hits it
        // on every keystroke.
        let cache = temp_cache("empty");
        cache.put(SourceId::Nyaa, "nothing matches", &[]);
        assert_eq!(
            cache.get(SourceId::Nyaa, "nothing matches"),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_stale_entry_is_a_miss() {
        let cache = temp_cache("stale");
        let path = cache.query_file(SourceId::Yts, "old");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale = CacheFile {
            fetched_at: now_secs().saturating_sub(TTL_SECS + 1),
            results: vec![result('a')],
        };
        fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(cache.get(SourceId::Yts, "old").is_none());
    }

    #[test]
    fn a_corrupt_or_drifted_entry_is_a_miss_not_an_error() {
        let cache = temp_cache("corrupt");
        let path = cache.query_file(SourceId::Yts, "bad");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{not json").unwrap();
        assert!(cache.get(SourceId::Yts, "bad").is_none());
        // And it is replaced by the next good write rather than wedging.
        cache.put(SourceId::Yts, "bad", &[result('a')]);
        assert!(cache.get(SourceId::Yts, "bad").is_some());
    }

    #[test]
    fn only_hard_failures_park_a_host() {
        let cache = temp_cache("health");
        cache.record_failure(
            SourceId::Yts,
            "yts.mx",
            &SourceError::Network("refused".into()),
        );
        assert_eq!(
            cache.parked_hosts(SourceId::Yts),
            vec!["yts.mx".to_string()]
        );

        // A parse failure is the source's defect, identical on every mirror.
        cache.record_failure(
            SourceId::Yts,
            "yts.am",
            &SourceError::Parse("bad row".into()),
        );
        assert!(
            !cache
                .parked_hosts(SourceId::Yts)
                .contains(&"yts.am".to_string())
        );

        // We cancelled it; the host did nothing wrong.
        cache.record_failure(SourceId::Yts, "yts.rs", &SourceError::Cancelled);
        assert_eq!(cache.parked_hosts(SourceId::Yts).len(), 1);
    }

    #[test]
    fn a_host_that_answers_is_unparked() {
        let cache = temp_cache("unpark");
        cache.record_failure(SourceId::Yts, "yts.mx", &SourceError::Timeout);
        assert!(!cache.parked_hosts(SourceId::Yts).is_empty());
        cache.record_success(SourceId::Yts, "yts.mx");
        assert!(cache.parked_hosts(SourceId::Yts).is_empty());
    }

    #[test]
    fn a_parked_host_expires_on_its_own() {
        let cache = temp_cache("expire");
        let mut health = HealthFile::default();
        health.hosts.insert(
            "yts.mx".into(),
            HealthEntry {
                failed_at: now_secs().saturating_sub(HEALTH_TTL_SECS + 5),
                class: "network".into(),
            },
        );
        cache.write_health(SourceId::Yts, &health);
        assert!(
            cache.parked_hosts(SourceId::Yts).is_empty(),
            "a brief outage must not hide a source for the session"
        );
    }

    #[test]
    fn all_parked_only_fires_when_every_mirror_is_down() {
        let cache = temp_cache("allparked");
        let hosts = ["yts.mx", "yts.am"];
        assert!(!cache.all_parked(SourceId::Yts, &hosts));
        cache.record_failure(SourceId::Yts, "yts.mx", &SourceError::Timeout);
        assert!(
            !cache.all_parked(SourceId::Yts, &hosts),
            "one mirror is still live"
        );
        cache.record_failure(SourceId::Yts, "yts.am", &SourceError::Timeout);
        assert!(cache.all_parked(SourceId::Yts, &hosts));
        assert!(
            !cache.all_parked(SourceId::Yts, &[]),
            "no hosts is not all-parked"
        );
    }

    #[test]
    fn clearing_results_leaves_health_alone() {
        let cache = temp_cache("clear");
        cache.put(SourceId::Yts, "dune", &[result('a')]);
        cache.record_failure(SourceId::Yts, "yts.mx", &SourceError::Timeout);
        cache.clear_results().expect("clear");
        assert!(cache.get(SourceId::Yts, "dune").is_none());
        assert!(!cache.parked_hosts(SourceId::Yts).is_empty());
        cache.clear_results().expect("clearing twice is fine");
    }
}
