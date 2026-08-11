//! YTS (Movies, JSON API) — the reference implementation for a source adapter.
//!
//! YTS publishes a real JSON API, so this is the simplest shape: one request,
//! typed deserialization, no follow-up fetch. Every field the UI needs is in the
//! response, so `magnet` is always `Some` and `resolve_magnet` is never called.
//!
//! One YTS movie carries several torrents (720p, 1080p, 2160p…), each with its
//! own infohash. Each becomes a row, tagged with its quality, because a user
//! choosing between a 2 GB and a 20 GB copy is making a real choice.

use serde::Deserialize;

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, normalize_info_hash};
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// Mirrors, in preference order. The engine's sticky hint reorders these.
pub const HOSTS: &[&str] = &["yts.mx", "yts.am", "yts.rs"];

const DEF: SourceDef = SourceDef {
    id: SourceId::Yts,
    label: "YTS",
    groups: &[SourceGroup::Movies],
    homepage: "https://yts.mx",
    reports_health: true,
};

#[derive(Debug, Deserialize)]
struct Response {
    data: Option<Data>,
}

#[derive(Debug, Deserialize)]
struct Data {
    movies: Option<Vec<Movie>>,
}

#[derive(Debug, Deserialize)]
struct Movie {
    title_long: Option<String>,
    title: Option<String>,
    date_uploaded_unix: Option<i64>,
    torrents: Option<Vec<Torrent>>,
}

#[derive(Debug, Deserialize)]
struct Torrent {
    hash: Option<String>,
    quality: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    size_bytes: Option<u64>,
    seeds: Option<u32>,
    peers: Option<u32>,
}

pub struct Yts {
    client: SourceClient,
}

impl Yts {
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for Yts {
    fn default() -> Self {
        Self::new()
    }
}

/// Turns one API response into rows.
///
/// Free of I/O so it can be tested against a committed fixture — the pattern
/// every source follows, because a parser tested only against the live site is
/// a parser that breaks silently (`FR-22`).
pub fn parse(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let response: Response =
        serde_json::from_str(body).map_err(|e| SourceError::Parse(e.to_string()))?;
    let movies = response.data.and_then(|d| d.movies).unwrap_or_default();

    let mut out = Vec::new();
    for movie in movies {
        let base = movie
            .title_long
            .or(movie.title)
            .unwrap_or_else(|| "Unknown".into());
        for torrent in movie.torrents.unwrap_or_default() {
            // A row without a usable infohash is dropped rather than rendered:
            // it could never be downloaded (`FR-14`).
            let Some(info_hash) = torrent.hash.as_deref().and_then(normalize_info_hash) else {
                continue;
            };
            let tag = [torrent.quality.as_deref(), torrent.kind.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            let name = if tag.is_empty() {
                base.clone()
            } else {
                format!("{base} [{tag}]")
            };
            out.push(TorrentResult {
                magnet: Some(build_magnet(&info_hash, &name)),
                info_hash,
                name,
                size_bytes: torrent.size_bytes.unwrap_or(0),
                seeders: torrent.seeds.unwrap_or(0),
                leechers: torrent.peers.unwrap_or(0),
                num_files: None,
                source: SourceId::Yts,
                added: movie.date_uploaded_unix,
            });
        }
    }
    Ok(out)
}

impl Source for Yts {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            let q = query.trim();
            // An empty query is the curated browse: newest first rather than
            // whatever the API's default ordering happens to be.
            let path = if q.is_empty() {
                "/api/v2/list_movies.json?limit=50&sort_by=date_added".to_string()
            } else {
                format!(
                    "/api/v2/list_movies.json?limit=50&query_term={}",
                    urlencode(q)
                )
            };
            let (body, _host) = self.client.get_text_failover(HOSTS, &path, ctx).await?;
            parse(&body)
        })
    }

    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        _ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        let existing = result.magnet.clone();
        Box::pin(async move {
            existing.ok_or_else(|| SourceError::Parse("YTS always supplies a magnet".into()))
        })
    }
}

/// Percent-encodes a query for a URL parameter.
pub fn urlencode(s: &str) -> String {
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

    const FIXTURE: &str = include_str!("fixtures/yts.json");

    #[test]
    fn parses_every_quality_as_its_own_row() {
        let rows = parse(FIXTURE).expect("fixture parses");
        assert_eq!(rows.len(), 3, "two movies, three torrents between them");

        let first = &rows[0];
        assert_eq!(first.info_hash, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1");
        assert!(first.name.contains("Example Movie (2026)"));
        assert!(first.name.contains("1080p"), "quality is in the row name");
        assert_eq!(first.size_bytes, 2_147_483_648);
        assert_eq!(first.seeders, 512);
        assert_eq!(first.leechers, 24);
        assert_eq!(first.source, SourceId::Yts);
        assert_eq!(first.added, Some(1_786_000_000));
        assert!(first.magnet.as_ref().unwrap().contains(&first.info_hash));
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded() {
        // The fixture's second movie has one torrent with a junk hash.
        let rows = parse(FIXTURE).expect("parses");
        assert!(
            rows.iter().all(|r| r.info_hash.len() == 40),
            "a row without a usable infohash must be dropped, not rendered"
        );
        assert!(rows.iter().all(|r| r.magnet.is_some()));
    }

    #[test]
    fn an_uppercase_hash_is_normalized() {
        // Sources emit both cases; the join key must be canonical.
        let rows = parse(FIXTURE).expect("parses");
        assert!(
            rows.iter()
                .all(|r| r.info_hash == r.info_hash.to_lowercase()),
            "infohashes must be lowercased at the boundary"
        );
    }

    #[test]
    fn an_empty_or_shaped_differently_response_is_not_an_error() {
        // The API answers `{"data":{"movie_count":0}}` for no matches; that is a
        // successful empty search, not a failure, and must not mark the source
        // offline.
        assert_eq!(parse(r#"{"data":{"movie_count":0}}"#).unwrap().len(), 0);
        assert_eq!(parse(r#"{"status":"ok"}"#).unwrap().len(), 0);
        assert_eq!(parse(r#"{"data":{"movies":null}}"#).unwrap().len(), 0);
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        assert!(matches!(parse("<html>nope"), Err(SourceError::Parse(_))));
    }

    #[test]
    fn queries_are_url_encoded() {
        assert_eq!(urlencode("the matrix"), "the+matrix");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert!(urlencode("進撃").is_ascii());
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let s = Yts::new();
        assert_eq!(s.def().id, SourceId::Yts);
        assert_eq!(s.def().groups, &[SourceGroup::Movies]);
        assert!(s.def().reports_health, "YTS publishes real swarm counts");
    }
}
