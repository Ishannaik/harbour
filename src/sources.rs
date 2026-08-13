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
//! * `GET {base}/search?q={query}&exclude={csv}` → `{"results":[TorrentResult…]}`
//!   — `exclude` lists site `SourceId`s to skip, empty string = none.
//! * `GET {base}/magnet?hash={info_hash}&source={SourceId}` → `{"magnet":…}`
//!   or `404 {"error":…}`.
//!
//! A non-200 answer or a timeout is a hard host failure: the UI shows the
//! error banner and marks the source `Offline` — never a silent empty list.

use std::time::Duration;

use serde::Deserialize;

use crate::core::error::SourceError;
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceId, TorrentResult,
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

/// The search answer: raw concatenated results from every enabled scraper.
/// The client's [`crate::search::merge`] dedupes them by infohash.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    results: Vec<TorrentResult>,
}

/// The magnet answer.
#[derive(Debug, Deserialize)]
struct MagnetResponse {
    magnet: String,
}

/// One HTTP-backed [`Source`]: every search and magnet resolution is a single
/// GET to the indexer. Stateless by contract, like every source.
#[derive(Debug, Clone)]
pub struct HttpSource {
    base_url: String,
    client: reqwest::Client,
}

impl HttpSource {
    /// Builds a client for `base_url` (e.g. `http://127.0.0.1:8765`).
    ///
    /// A per-request connect timeout guards against a half-open connection
    /// hanging past the search budget; the caller's `total_deadline` still
    /// bounds the whole round trip.
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|err| {
                eprintln!("harbour: falling back to a default HTTP client ({err})");
                reqwest::Client::new()
            });
        Self { base_url, client }
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
        let response = tokio::time::timeout(ctx.total_deadline, self.client.get(url).send())
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
            let response: SearchResponse = serde_json::from_str(&body)
                .map_err(|e| SourceError::Parse(format!("indexer search response: {e}")))?;
            Ok(response.results)
        })
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
                  "seeders":512,"leechers":24,"num_files":1,"source":"cinevault",
                  "magnet":"magnet:?xt=urn:btih:{HASH}&dn=dune","added":1786000000}},
                {{"info_hash":"fedcba9876543210fedcba9876543210fedcba98","name":"Frieren - 01",
                  "size_bytes":734003200,"seeders":0,"leechers":0,"source":"tsukibase"}}
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
        assert_eq!(rows[0].source, SourceId::CineVault);
        assert_eq!(rows[0].added, Some(1_786_000_000));
        assert_eq!(rows[1].source, SourceId::TsukiBase);
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
                &ctx(HashSet::from([SourceId::GamesHub, SourceId::TsukiBase])),
            )
            .await
            .expect("search");
        assert!(rows.is_empty());
        let requests = handle.join().expect("indexer thread");
        assert!(
            requests[0].contains("exclude=gameshub,tsukibase"),
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
    async fn resolve_magnet_asks_the_indexer_for_the_site() {
        let magnet = format!("magnet:?xt=urn:btih:{HASH}&dn=dune");
        let body = format!(r#"{{"magnet":"{magnet}"}}"#);
        let (base, handle) = spawn_indexer(vec![(200, body)]);
        let source = HttpSource::new(base);
        let got = source
            .resolve_magnet(&row(SourceId::GamesHub), &ctx(HashSet::new()))
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
            requests[0].contains("source=gameshub"),
            "the row's real site id picks the resolver: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn a_404_magnet_answer_is_a_source_error() {
        let (base, handle) = spawn_indexer(vec![(404, r#"{"error":"unknown source"}"#.into())]);
        let source = HttpSource::new(base);
        let result = source
            .resolve_magnet(&row(SourceId::GamesHub), &ctx(HashSet::new()))
            .await;
        assert!(
            matches!(result, Err(SourceError::Network(_))),
            "404 means no magnet: {result:?}"
        );
        handle.join().expect("indexer thread");
    }
}
