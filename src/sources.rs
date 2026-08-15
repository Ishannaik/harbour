//! The one client-side source: an HTTP proxy to the user-run harbour indexer.
//!
//! The client ships **zero scrapers** — all ten site adapters, the resilient
//! fetch layer, and the search cache live in the separate `harbour-indexer`
//! service. This module is the client's only [`Source`]: it forwards a search
//! to the indexer over HTTP and turns the wire JSON back into
//! [`TorrentResult`]s, so the client stays legal/neutral wherever it runs and
//! the user supplies the indexer (Stremio-addon model, `docs/architecture.md`).
//!
//! Wire contract (both sides must match exactly):
//!
//! * `GET {base}/search?q={query}&exclude={csv}` →
//!   `{"results":[TorrentResult…], "sources":[SourceReport…]}` — `exclude`
//!   lists site `SourceId`s to skip, empty string = none. `sources` is the
//!   indexer's per-site health (`id`/`status`/`count`), which the app folds
//!   into the sidebar dots; an old indexer that omits it is tolerated.
//! * `GET {base}/magnet?hash={info_hash}&source={SourceId}` → `{"magnet":…}`
//!   or `404 {"error":…}`.
//!
//! A non-200 answer or a timeout is a hard host failure: the UI shows the
//! error banner and marks the source `Offline` — never a silent empty list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::core::error::SourceError;
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceId, SourceStatus, TorrentResult,
};

#[cfg(test)]
use std::collections::HashSet;

/// Static metadata for the indexer source. `groups` is empty on purpose: the
/// sidebar renders the ten *site* rows (`SourceId::ALL`) that the indexer
/// searches, not this proxy.
const DEF: SourceDef = SourceDef {
    id: SourceId::Indexer,
    label: "Indexer",
    groups: &[],
    homepage: "http://127.0.0.1:8765",
    reports_health: true,
};

/// The magnet answer.
#[derive(Debug, Deserialize)]
struct MagnetResponse {
    magnet: String,
}

/// One row of the indexer's per-site report (`sources` array).
#[derive(Debug, Deserialize)]
struct SourceReport {
    id: String,
    status: String,
    count: u32,
}

/// Parses the indexer's search answer, dropping malformed rows instead of
/// failing the whole response (`FR-14`).
///
/// One scraper's schema drift (or a truncated row) must not sink the other
/// results: each `results` entry is converted to a [`TorrentResult`]
/// individually, and rows that fail to deserialize — or that are missing an
/// `info_hash`/`name` — are skipped. The only hard errors are a wrong outer
/// shape (not JSON, no `results` array) or a response where *every* row
/// fails, because an all-dead answer is more likely a wire-contract break
/// than a scrape hiccup.
fn parse_results(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let outer: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| SourceError::Parse(format!("indexer search response: {e}")))?;
    let rows = outer
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            SourceError::Parse("indexer search response: missing \"results\" array".into())
        })?;
    let valid: Vec<TorrentResult> = rows
        .iter()
        .filter_map(|row| {
            let result: TorrentResult = serde_json::from_value(row.clone()).ok()?;
            if result.info_hash.is_empty() || result.name.trim().is_empty() {
                return None;
            }
            Some(result)
        })
        .collect();
    if valid.is_empty() && !rows.is_empty() {
        return Err(SourceError::Parse(
            "indexer search response: no valid rows".into(),
        ));
    }
    Ok(valid)
}

/// One HTTP-backed [`Source`]: every search and magnet resolution is a single
/// GET to the indexer. Stateless by contract, like every source — except the
/// one shared per-site health store, which is the *indexer's* report, not
/// source state: it rides the search answer and is handed back to the app.
#[derive(Debug, Clone)]
pub struct HttpSource {
    base_url: String,
    client: reqwest::Client,
    /// The indexer's last per-site report (`sources` array), keyed by site id.
    /// Shared so the app can read it after a search completes; clones of this
    /// source (there is only ever one) all see the same store.
    health: Arc<Mutex<HashMap<SourceId, (SourceStatus, u32)>>>,
}

/// Resolves the effective indexer URL.
///
/// If `url` is the default "http://127.0.0.1:8765" or empty, it checks for:
/// 1. `HARBOUR_INDEXER_URL` environment variable override.
/// 2. Active port lockfile in `~/.harbour/indexer.port`.
/// 3. Fallback to `http://127.0.0.1:8765`.
pub fn resolve_indexer_url(configured_url: &str) -> String {
    if let Ok(env_url) = std::env::var("HARBOUR_INDEXER_URL") {
        if !env_url.trim().is_empty() {
            return env_url.trim().to_string();
        }
    }

    if configured_url == "http://127.0.0.1:8765" || configured_url.is_empty() {
        let state_root = crate::core::paths::state_dir();
        let port_path = crate::core::paths::indexer_port_file(&state_root);
        if let Ok(content) = std::fs::read_to_string(port_path) {
            if let Ok(port) = content.trim().parse::<u16>() {
                return format!("http://127.0.0.1:{port}");
            }
        }
    }

    configured_url.to_string()
}

impl HttpSource {
    /// Builds a client for `base_url` (e.g. `http://127.0.0.1:8765`).
    ///
    /// A per-request connect timeout guards against a half-open connection
    /// hanging past the search budget; the caller's `total_deadline` still
    /// bounds the whole round trip.
    pub fn new(base_url: String) -> Self {
        let resolved_url = resolve_indexer_url(&base_url);
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|err| {
                eprintln!("harbour: falling back to a default HTTP client ({err})");
                reqwest::Client::new()
            });
        Self {
            base_url: resolved_url,
            client,
            health: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// GETs `url` (already URL-encoded) inside the search budget.
    ///
    /// A non-2xx answer is a hard failure — the indexer is the only source, so
    /// there is no mirror to fail over to and nothing to retry. A timeout past
    /// `ctx.total_deadline` reports `SourceError::Timeout`, everything else
    /// `SourceError::Network`.
    async fn get(&self, url: &str, ctx: &SearchCtx) -> Result<String, SourceError> {
        if ctx.cancel.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let deadline = ctx.total_deadline + Duration::from_secs(5);
        let response = tokio::time::timeout(deadline, self.client.get(url).send())
            .await
            .map_err(|_| SourceError::Timeout)?
            .map_err(|e| SourceError::Network(format!("indexer unreachable: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Network(format!(
                "indexer returned HTTP {status}"
            )));
        }
        response
            .text()
            .await
            .map_err(|e| SourceError::Network(format!("reading indexer response: {e}")))
    }

    /// Records the indexer's per-site report (`sources` array) into the shared
    /// store, replacing the previous report wholesale — the array covers every
    /// source the indexer ran for this query.
    ///
    /// Defensive by contract: an answer without a `sources` array (an old
    /// indexer, or a test stub) leaves the store untouched, and one malformed
    /// entry never sinks the rest — it is skipped like a malformed row.
    fn record_sources(&self, body: &str) {
        let outer: serde_json::Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => return,
        };
        let Some(report) = outer.get("sources") else {
            return;
        };
        let mut entries: HashMap<SourceId, (SourceStatus, u32)> = HashMap::new();
        for entry in report.as_array().into_iter().flatten() {
            let Ok(row) = serde_json::from_value::<SourceReport>(entry.clone()) else {
                continue;
            };
            let status = match row.status.as_str() {
                "online" => SourceStatus::Online,
                "empty" => SourceStatus::Empty,
                "offline" => SourceStatus::Offline,
                // An unknown status string is a contract break for that entry;
                // skipping it beats inventing a state the indexer never sent.
                _ => continue,
            };
            let Some(id) = SourceId::parse(&row.id) else {
                continue;
            };
            entries.insert(id, (status, row.count));
        }
        // Poison recovery: a thread that panicked while holding the lock must
        // not take search health down with it — recover the guard and carry on.
        let mut guard = self.health.lock().unwrap_or_else(|e| e.into_inner());
        *guard = entries;
    }

    /// Snapshot of the per-site health the indexer last reported for a search,
    /// keyed by site id. Empty until a search has answered with a report (or
    /// when the indexer does not send one).
    pub fn reported_status(&self) -> HashMap<SourceId, (SourceStatus, u32)> {
        let guard = self.health.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    }
}

impl Source for HttpSource {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            // The whole-source toggle: a disabled indexer is never queried.
            if ctx.disabled.contains(&SourceId::Indexer) {
                return Ok(Vec::new());
            }
            let mut url = format!("{}/search?q={}", self.base_url, urlencode(query.trim()));
            // User-disabled sites ride the exclude param so the indexer never
            // queries them either. Sorted for deterministic requests.
            let mut excluded: Vec<&str> = ctx
                .disabled
                .iter()
                .filter(|id| **id != SourceId::Indexer)
                .map(|id| id.as_str())
                .collect();
            excluded.sort_unstable();
            if !excluded.is_empty() {
                url.push_str("&exclude=");
                url.push_str(&excluded.join(","));
            }
            let body = self.get(&url, ctx).await?;
            // The per-site report rides the same answer; an indexer that does
            // not send one leaves the store untouched (FR-15/18).
            self.record_sources(&body);
            // One malformed row must not sink the whole answer (FR-14): the
            // indexer concatenates every scraper's rows, and one schema drift
            // would otherwise hide the other nine sites' results.
            parse_results(&body)
        })
    }

    fn reported_source_health(&self) -> HashMap<SourceId, (SourceStatus, u32)> {
        self.reported_status()
    }

    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        // The indexer needs the *site* the row came from to pick the right
        // resolver; results carry the real site id even though the only
        // registered source is this proxy.
        let url = format!(
            "{}/magnet?hash={}&source={}",
            self.base_url, result.info_hash, result.source
        );
        Box::pin(async move {
            let body = self.get(&url, ctx).await?;
            let response: MagnetResponse = serde_json::from_str(&body)
                .map_err(|e| SourceError::Parse(format!("indexer magnet response: {e}")))?;
            Ok(response.magnet)
        })
    }
}

/// Percent-encodes a query for a URL parameter. The lone client-side copy:
/// the indexer owns the scrapers' encoders now.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    /// One canned HTTP response: status line + JSON body, connection closed.
    fn respond(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Error",
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n\
             {body}",
            body.len()
        );
        let _ = stream.flush();
    }

    /// Reads one HTTP request; the request line is all these tests assert on.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Spawns an indexer stub on an ephemeral port. Serves one canned
    /// response per entry in `answers` (in order) and returns the request
    /// lines it saw when joined.
    fn spawn_indexer(answers: Vec<(u16, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub indexer");
        let base = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("stub address").port()
        );
        let handle = thread::spawn(move || run_stub(listener, answers));
        (base, handle)
    }

    /// The stub server loop, top-level so the nested `if` stays under the
    /// FR-67 nesting threshold (a closure+for+if inside `spawn_indexer` is
    /// one level too deep for clippy).
    fn run_stub(listener: TcpListener, answers: Vec<(u16, String)>) -> Vec<String> {
        let mut requests = Vec::new();
        for (status, body) in answers {
            if !serve_one(&listener, &mut requests, status, &body) {
                break;
            }
        }
        requests
    }

    /// Serves one canned response; returns false when the client is gone.
    fn serve_one(
        listener: &TcpListener,
        requests: &mut Vec<String>,
        status: u16,
        body: &str,
    ) -> bool {
        match listener.accept() {
            Ok((mut stream, _)) => {
                requests.push(read_request(&mut stream));
                respond(&mut stream, status, body);
                true
            }
            Err(_) => false,
        }
    }

    fn ctx(disabled: HashSet<SourceId>) -> SearchCtx {
        SearchCtx {
            disabled,
            ..SearchCtx::default()
        }
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn row(source: SourceId) -> TorrentResult {
        TorrentResult {
            info_hash: HASH.to_string(),
            name: "row".into(),
            size_bytes: 0,
            seeders: 0,
            leechers: 0,
            num_files: None,
            source,
            magnet: None,
            added: None,
        }
    }

    #[test]
    fn the_registry_is_the_single_http_source() {
        // The client ships exactly one source: the indexer proxy. The ten site
        // scrapers live in harbour-indexer, so `SourceId::ALL` (the toggleable
        // sites) is deliberately *not* the registry anymore.
        let source = HttpSource::new("http://127.0.0.1:8765".to_string());
        assert_eq!(source.def().id, SourceId::Indexer);
        assert!(!source.def().label.is_empty());
        assert_eq!(source.def().groups, &[]);
    }

    #[tokio::test]
    async fn an_indexer_search_parses_into_results() {
        let body = format!(
            r#"{{"results":[
                {{"info_hash":"{HASH}","name":"Dune: Part Two [1080p]","size_bytes":2147483648,
                  "seeders":512,"leechers":24,"num_files":1,"source":"yts",
                  "magnet":"magnet:?xt=urn:btih:{HASH}&dn=dune","added":1786000000}},
                {{"info_hash":"fedcba9876543210fedcba9876543210fedcba98","name":"Frieren - 01",
                  "size_bytes":734003200,"seeders":0,"leechers":0,"source":"nyaa"}}
            ]}}"#
        );
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        let rows = source
            .search("dune", &ctx(HashSet::new()))
            .await
            .expect("search");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].info_hash, HASH);
        assert_eq!(rows[0].seeders, 512);
        assert_eq!(rows[0].source, SourceId::Yts);
        assert_eq!(rows[0].added, Some(1_786_000_000));
        assert_eq!(rows[1].source, SourceId::Nyaa);
        assert_eq!(
            rows[1].magnet, None,
            "an omitted optional field stays None, not an error"
        );

        let requests = handle.join().expect("indexer thread");
        assert!(
            requests[0].starts_with("GET /search?q=dune"),
            "the query rides the q param: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn a_malformed_row_is_dropped_not_fatal() {
        // FR-14: one scraper's schema drift (wrong-type hash, missing name)
        // must not sink the valid rows around it.
        let body = format!(
            r#"{{"results":[
                {{"info_hash":123,"name":"broken row"}},
                {{"info_hash":"","name":"empty hash"}},
                {{"info_hash":"{HASH}","name":"Dune: Part Two [1080p]","size_bytes":2147483648,
                  "seeders":512,"leechers":24,"num_files":1,"source":"yts",
                  "magnet":"magnet:?xt=urn:btih:{HASH}&dn=dune","added":1786000000}},
                {{"info_hash":"fedcba9876543210fedcba9876543210fedcba98","name":"   ",
                  "size_bytes":1,"seeders":1,"leechers":1,"source":"nyaa"}}
            ]}}"#
        );
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        let rows = source
            .search("dune", &ctx(HashSet::new()))
            .await
            .expect("a single bad row is not a search failure");
        assert_eq!(rows.len(), 1, "only the well-formed row survives: {rows:?}");
        assert_eq!(rows[0].info_hash, HASH);
        handle.join().expect("indexer thread");
    }

    #[tokio::test]
    async fn a_response_where_every_row_fails_is_an_error() {
        // All rows dead is a wire-contract break, not a scrape hiccup: only
        // then (or a wrong outer shape) is the response a hard failure.
        let body = r#"{"results":[
            {"info_hash":123,"name":"one"},
            {"info_hash":456,"name":"two"}
        ]}"#
        .to_string();
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        let result = source.search("dune", &ctx(HashSet::new())).await;
        assert!(
            matches!(result, Err(SourceError::Parse(_))),
            "an all-malformed answer is a parse error: {result:?}"
        );
        handle.join().expect("indexer thread");

        // A missing "results" array is the same class of error.
        let (base, handle) = spawn_indexer(vec![(200, r#"{"error":"no rows"}"#.into())]);
        let source = HttpSource::new(base);
        let result = source.search("dune", &ctx(HashSet::new())).await;
        assert!(
            matches!(result, Err(SourceError::Parse(_))),
            "a missing results array is a parse error: {result:?}"
        );
        handle.join().expect("indexer thread");
    }

    #[tokio::test]
    async fn queries_are_url_encoded() {
        let (base, handle) = spawn_indexer(vec![(200, r#"{"results":[]}"#.into())]);
        let source = HttpSource::new(base);
        let rows = source
            .search("the matrix & more", &ctx(HashSet::new()))
            .await
            .expect("search");
        assert!(rows.is_empty());
        let requests = handle.join().expect("indexer thread");
        assert!(
            requests[0].contains("q=the+matrix+%26+more"),
            "query is percent-encoded: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn disabled_sites_are_sent_as_exclude() {
        let (base, handle) = spawn_indexer(vec![(200, r#"{"results":[]}"#.into())]);
        let source = HttpSource::new(base);
        let rows = source
            .search(
                "dune",
                &ctx(HashSet::from([SourceId::FitGirl, SourceId::Nyaa])),
            )
            .await
            .expect("search");
        assert!(rows.is_empty());
        let requests = handle.join().expect("indexer thread");
        assert!(
            requests[0].contains("exclude=fitgirl,nyaa"),
            "disabled sites ride the exclude param: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn a_disabled_indexer_is_never_queried() {
        let (base, handle) = spawn_indexer(Vec::new());
        let source = HttpSource::new(base);
        let rows = source
            .search("dune", &ctx(HashSet::from([SourceId::Indexer])))
            .await
            .expect("no network, empty rows");
        assert!(rows.is_empty());
        let requests = handle.join().expect("indexer thread");
        assert!(
            requests.is_empty(),
            "a disabled indexer is not asked, not even once"
        );
    }

    #[tokio::test]
    async fn a_non_200_answer_is_a_source_error() {
        let (base, handle) = spawn_indexer(vec![(500, r#"{"error":"boom"}"#.into())]);
        let source = HttpSource::new(base);
        let result = source.search("dune", &ctx(HashSet::new())).await;
        assert!(
            matches!(result, Err(SourceError::Network(_))),
            "a 5xx is a hard host failure: {result:?}"
        );
        let requests = handle.join().expect("indexer thread");
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_indexer_is_a_source_error() {
        // A port nothing listens on: grab an ephemeral port and drop the
        // listener so the connection is refused.
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("address")
            .port();
        let source = HttpSource::new(format!("http://127.0.0.1:{port}"));
        let result = source.search("dune", &ctx(HashSet::new())).await;
        assert!(
            matches!(result, Err(SourceError::Network(_))),
            "connection refused is a source error: {result:?}"
        );
    }

    #[tokio::test]
    async fn the_sources_array_populates_the_health_store() {
        // FR-15/18: the indexer's per-site report rides the search answer and
        // must land in the store so the app can paint the sidebar dots.
        let body = r#"{
            "results": [],
            "sources": [
                {"id": "yts", "status": "online", "count": 3},
                {"id": "nyaa", "status": "empty", "count": 0},
                {"id": "fitgirl", "status": "offline", "count": 0}
            ]
        }"#
        .to_string();
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        source
            .search("dune", &ctx(HashSet::new()))
            .await
            .expect("search");
        let report = source.reported_status();
        assert_eq!(report.get(&SourceId::Yts), Some(&(SourceStatus::Online, 3)));
        assert_eq!(report.get(&SourceId::Nyaa), Some(&(SourceStatus::Empty, 0)));
        assert_eq!(
            report.get(&SourceId::FitGirl),
            Some(&(SourceStatus::Offline, 0))
        );
        handle.join().expect("indexer thread");
    }

    #[tokio::test]
    async fn a_malformed_sources_entry_is_skipped_not_fatal() {
        // One bad entry (unknown id, unknown status, wrong-typed count) must
        // not sink the healthy ones around it — same rule as result rows.
        let body = r#"{
            "results": [],
            "sources": [
                {"id": "not-a-source", "status": "online", "count": 1},
                {"id": "yts", "status": "flying", "count": 1},
                {"id": "nyaa", "status": "empty", "count": "lots"},
                {"id": "fitgirl", "status": "offline", "count": 0}
            ]
        }"#
        .to_string();
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        source
            .search("dune", &ctx(HashSet::new()))
            .await
            .expect("search");
        let report = source.reported_status();
        assert_eq!(
            report.len(),
            1,
            "only the healthy entry survives: {report:?}"
        );
        assert_eq!(
            report.get(&SourceId::FitGirl),
            Some(&(SourceStatus::Offline, 0))
        );
        handle.join().expect("indexer thread");
    }

    #[tokio::test]
    async fn an_answer_without_sources_keeps_the_previous_report() {
        // Defensive: an old indexer (or a stub) omits the array; the store must
        // keep whatever the last report said instead of being wiped.
        let with =
            r#"{"results":[],"sources":[{"id":"yts","status":"online","count":3}]}"#.to_string();
        let without = r#"{"results":[]}"#.to_string();
        let (base, handle) = spawn_indexer(vec![(200, with), (200, without)]);
        let source = HttpSource::new(base);
        source
            .search("dune", &ctx(HashSet::new()))
            .await
            .expect("search");
        source
            .search("other", &ctx(HashSet::new()))
            .await
            .expect("search");
        assert_eq!(
            source.reported_status().get(&SourceId::Yts),
            Some(&(SourceStatus::Online, 3)),
            "a report-less answer leaves the store as it was"
        );
        handle.join().expect("indexer thread");
    }

    #[tokio::test]
    async fn resolve_magnet_asks_the_indexer_for_the_site() {
        let magnet = format!("magnet:?xt=urn:btih:{HASH}&dn=dune");
        let body = format!(r#"{{"magnet":"{magnet}"}}"#);
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        let got = source
            .resolve_magnet(&row(SourceId::FitGirl), &ctx(HashSet::new()))
            .await
            .expect("resolves");
        assert_eq!(got, magnet);
        let requests = handle.join().expect("indexer thread");
        assert!(
            requests[0].contains(&format!("/magnet?hash={HASH}")),
            "the hash rides the hash param: {}",
            requests[0]
        );
        assert!(
            requests[0].contains("source=fitgirl"),
            "the row's real site id picks the resolver: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn a_404_magnet_answer_is_a_source_error() {
        let (base, handle) = spawn_indexer(vec![(404, r#"{"error":"unknown source"}"#.into())]);
        let source = HttpSource::new(base);
        let result = source
            .resolve_magnet(&row(SourceId::FitGirl), &ctx(HashSet::new()))
            .await;
        assert!(
            matches!(result, Err(SourceError::Network(_))),
            "404 means no magnet: {result:?}"
        );
        handle.join().expect("indexer thread");
    }
}
