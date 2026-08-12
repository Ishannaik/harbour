//! The Pirate Bay via apibay.org (Movies and TV) — two `SourceId`s, one parser.
//!
//! apibay is TPB's own JSON backend: a flat array carrying an infohash on every
//! row, so like YTS this is one request with no detail-page follow-up and
//! `magnet` is always `Some`. The default [`Source::resolve_magnet`] is
//! therefore already correct and is deliberately not overridden.
//!
//! Three properties of this API shape the whole module:
//!
//! * **Every number arrives as a string** — `"seeders":"120"`,
//!   `"size":"2147483648"`. They are parsed here, and a value that will not
//!   parse degrades to `0` rather than failing the response: apibay is an
//!   unofficial mirror that does emit garbage rows (`docs/sources.md` §3.3), and
//!   one unreadable `seeders` field must not cost the other ninety-nine results.
//! * **"Nothing matched" is a row, not an empty array.** A search with no hits
//!   answers with a single element whose id is `0` and whose infohash is forty
//!   zeros. Forty zeros *is* valid hex, so it survives normalization and would
//!   otherwise render as an undownloadable row named "No results returned" —
//!   and would report the source as having answered with results. Recognising
//!   it turns that response back into the honest empty search it is.
//! * **Category filtering is ours to do.** `cat` takes a single value and
//!   neither source maps to a single subcategory, so we ask for the whole video
//!   tree and narrow locally. That local filter is the only thing keeping a TV
//!   row out of tpb-movies, which is why these are two ids and not one.

use serde::Deserialize;
use serde_json::Value;

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, normalize_info_hash};
use crate::core::types::{
    SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;
// One percent-encoder for the crate rather than a second, subtly different copy
// here. If a third source needs it, it should move to a shared module.
use crate::sources::yts::urlencode;

/// The only host. apibay is TPB's own API endpoint rather than one of the many
/// HTML proxies, so there is no mirror chain to rotate through — but the
/// failover call is used anyway, which keeps the retry ladder and the sticky
/// host hint working the day a second endpoint appears.
pub const HOSTS: &[&str] = &["apibay.org"];

/// TPB's video parent category.
///
/// `q.php` accepts one `cat`, and neither source is one subcategory (Movies is
/// four of them, TV is two), so the request asks for the whole video tree and
/// [`parse`] narrows it. Asking per-subcategory would mean several requests, and
/// asking for `205` alone would silently lose every HD TV show.
const VIDEO_CATEGORY: u32 = 200;

/// Movie subcategories: Movies, Movies DVDR, HD Movies, 3D.
const MOVIE_CATEGORIES: &[u32] = &[201, 202, 207, 209];

/// TV subcategories: TV shows, HD TV shows.
const TV_CATEGORIES: &[u32] = &[205, 208];

/// apibay's precompiled top-100 lists, one static file per category. `207` and
/// `208` are the HD categories the site's own front page browses, they are far
/// cheaper for apibay than a wildcard query, and both pass the category filter.
const TOP_100_MOVIES: &str = "/precompiled/data_top100_207.json";
const TOP_100_TV: &str = "/precompiled/data_top100_208.json";

/// The infohash apibay sends when nothing matched (see the module docs).
const NO_RESULTS_INFO_HASH: &str = "0000000000000000000000000000000000000000";

const MOVIES_DEF: SourceDef = SourceDef {
    id: SourceId::TpbMovies,
    label: "TPB",
    groups: &[SourceGroup::Movies],
    // The human-facing site; the JSON API this adapter talks to lives on apibay.
    homepage: "https://thepiratebay.org",
    reports_health: true,
};

const TV_DEF: SourceDef = SourceDef {
    id: SourceId::TpbTv,
    label: "TPB",
    groups: &[SourceGroup::Tv],
    homepage: "https://thepiratebay.org",
    reports_health: true,
};

/// One element of apibay's flat array. Unknown fields (`id`, `username`,
/// `status`, `imdb`) are ignored so additions upstream cannot break parsing.
///
/// The numeric fields are [`Value`] rather than `String` because apibay types
/// them as strings *today*: a mirror that started emitting real JSON numbers
/// would otherwise fail deserialization for the entire array rather than for one
/// field, which is the opposite of how this parser is supposed to degrade.
#[derive(Debug, Deserialize)]
struct Row {
    name: Option<String>,
    info_hash: Option<String>,
    #[serde(default)]
    seeders: Value,
    #[serde(default)]
    leechers: Value,
    #[serde(default)]
    num_files: Value,
    #[serde(default)]
    size: Value,
    #[serde(default)]
    added: Value,
    #[serde(default)]
    category: Value,
}

/// Reads one of apibay's string-typed numbers.
///
/// Anything unreadable degrades to `0` instead of failing the row: `FR-14` drops
/// rows that could never be *downloaded*, and a bad seeder count is not that.
/// `0` is also what apibay itself sends for "unknown", so the degraded value is
/// indistinguishable from an honest one and needs no separate representation.
fn as_u64(value: &Value) -> u64 {
    match value {
        Value::String(s) => s.trim().parse().unwrap_or(0),
        // Not the shape apibay uses, but one arm buys us a mirror that fixes its
        // types without us noticing.
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// [`as_u64`] saturated into the width the UI columns use.
fn as_u32(value: &Value) -> u32 {
    u32::try_from(as_u64(value)).unwrap_or(u32::MAX)
}

/// The subcategories a source claims.
///
/// Anything that is not one of the two TPB ids claims nothing, so a mis-wired
/// registry yields an empty search rather than a panic inside a search task.
fn categories_for(source: SourceId) -> &'static [u32] {
    match source {
        SourceId::TpbMovies => MOVIE_CATEGORIES,
        SourceId::TpbTv => TV_CATEGORIES,
        _ => &[],
    }
}

/// The path one search fetches.
///
/// Split out of the fetch and kept pure so the endpoint choice — top-100 list
/// versus query — is covered by a test rather than only by running a search.
fn request_path(source: SourceId, query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        // An empty `q` returns nothing at all from `q.php`, so the curated
        // browse has to come from the precompiled list instead.
        return match source {
            SourceId::TpbTv => TOP_100_TV,
            _ => TOP_100_MOVIES,
        }
        .to_string();
    }
    format!("/q.php?q={}&cat={VIDEO_CATEGORY}", urlencode(q))
}

/// Turns one apibay response into the rows belonging to `source`.
///
/// Free of I/O so it can be tested against a committed fixture — the pattern
/// every source follows, because a parser tested only against the live site is
/// a parser that breaks silently (`FR-22`).
pub fn parse(body: &str, source: SourceId) -> Result<Vec<TorrentResult>, SourceError> {
    let rows: Vec<Row> =
        serde_json::from_str(body).map_err(|e| SourceError::Parse(e.to_string()))?;
    let categories = categories_for(source);

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // A row without a usable infohash is dropped rather than rendered: it
        // could never be downloaded (`FR-14`).
        let Some(info_hash) = row.info_hash.as_deref().and_then(normalize_info_hash) else {
            continue;
        };
        // Checked before the category filter, not left to it: apibay's sentinel
        // happens to carry category `0` today, but "an all-zero infohash is not
        // a torrent" is the durable reason to drop it.
        if info_hash == NO_RESULTS_INFO_HASH {
            continue;
        }
        // A row we cannot classify degrades to category 0 and is dropped by
        // both sources — showing it under a category it may not belong to is
        // exactly the mixing these two ids exist to prevent.
        if !categories.contains(&as_u32(&row.category)) {
            continue;
        }
        // A nameless row is undisplayable and equally undownloadable (`FR-14`).
        let raw_name = row.name.unwrap_or_default();
        let name = raw_name.trim();
        if name.is_empty() {
            continue;
        }

        out.push(TorrentResult {
            // Built by the one builder so the infohash is lowercased and the
            // display name escaped in exactly one place.
            magnet: Some(build_magnet(&info_hash, name)),
            info_hash,
            name: name.to_string(),
            size_bytes: as_u64(&row.size),
            seeders: as_u32(&row.seeders),
            leechers: as_u32(&row.leechers),
            // `0` means *unknown* here — the precompiled lists send it for every
            // row — and a torrent with no files does not exist, so it becomes
            // `None` and the UI renders a dash instead of a confident lie.
            num_files: match as_u32(&row.num_files) {
                0 => None,
                n => Some(n),
            },
            source,
            // Same reasoning: `added: "0"` is a missing timestamp, not midnight
            // on 1 January 1970.
            added: match as_u64(&row.added) {
                0 => None,
                secs => i64::try_from(secs).ok(),
            },
        });
    }
    Ok(out)
}

/// One search against apibay, shared by both sources — the fetch half of the
/// "two ids, one adapter" split.
async fn search_apibay<'a>(
    client: &'a SourceClient,
    source: SourceId,
    query: &'a str,
    ctx: &'a SearchCtx,
) -> Result<Vec<TorrentResult>, SourceError> {
    let path = request_path(source, query);
    let (body, _host) = client.get_text_failover(HOSTS, &path, ctx).await?;
    parse(&body, source)
}

/// The Pirate Bay, Movies half.
pub struct TpbMovies {
    client: SourceClient,
}

impl TpbMovies {
    /// Builds the adapter and its HTTP client.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for TpbMovies {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for TpbMovies {
    fn def(&self) -> &'static SourceDef {
        &MOVIES_DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(search_apibay(&self.client, SourceId::TpbMovies, query, ctx))
    }
}

/// The Pirate Bay, TV half. Same API and same parser as [`TpbMovies`]; only the
/// categories and the curated top list differ.
pub struct TpbTv {
    client: SourceClient,
}

impl TpbTv {
    /// Builds the adapter and its HTTP client.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for TpbTv {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for TpbTv {
    fn def(&self) -> &'static SourceDef {
        &TV_DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(search_apibay(&self.client, SourceId::TpbTv, query, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One response covering both categories plus every drop case. The zero
    /// sentinel rides along in it so the drop is exercised from the same file;
    /// in the wild it arrives alone, which `NO_RESULTS` covers.
    const FIXTURE: &str = include_str!("fixtures/tpb.json");

    /// apibay's actual "nothing matched" body — one row, not an empty array.
    /// Reflowed across lines: JSON whitespace is insignificant, so the bytes
    /// the parser sees are identical.
    const NO_RESULTS: &str = r#"[{"id":"0","name":"No results returned",
"info_hash":"0000000000000000000000000000000000000000","leechers":"0","seeders":"0",
"num_files":"0","size":"0","username":"","added":"0","status":"","category":"0","imdb":""}]"#;

    fn movies() -> Vec<TorrentResult> {
        parse(FIXTURE, SourceId::TpbMovies).expect("fixture parses")
    }

    fn tv() -> Vec<TorrentResult> {
        parse(FIXTURE, SourceId::TpbTv).expect("fixture parses")
    }

    #[test]
    fn string_typed_numbers_become_real_ones() {
        let rows = movies();
        let first = &rows[0];
        assert_eq!(first.name, "Example Movie 2026 1080p WEBRip x264-HARBOUR");
        assert_eq!(first.info_hash, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1");
        assert_eq!(first.size_bytes, 2_147_483_648, "\"size\" is a byte string");
        assert_eq!(first.seeders, 1240);
        assert_eq!(first.leechers, 37);
        assert_eq!(first.num_files, Some(3));
        assert_eq!(first.added, Some(1_786_000_000));
        assert_eq!(first.source, SourceId::TpbMovies);
    }

    #[test]
    fn an_uppercase_hash_is_normalized() {
        // apibay emits uppercase; the join key across sources must be canonical.
        let rows = movies();
        assert!(
            rows.iter()
                .all(|r| r.info_hash == r.info_hash.to_lowercase()),
            "infohashes must be lowercased at the boundary"
        );
        assert!(
            rows.iter()
                .any(|r| r.info_hash == "ccccccccccccccccccccccccccccccccccccccc3"),
            "the fixture's uppercase C row must survive as lowercase"
        );
    }

    #[test]
    fn every_row_carries_a_magnet_from_the_shared_builder() {
        for row in movies().iter().chain(tv().iter()) {
            assert_eq!(
                row.magnet,
                Some(build_magnet(&row.info_hash, &row.name)),
                "magnets are never hand-formatted"
            );
        }
    }

    #[test]
    fn the_zero_result_sentinel_is_an_empty_search_not_a_bogus_row() {
        // The whole point: forty zeros is valid hex, so without the check this
        // would be a downloadable-looking row and the source would read as
        // Online-with-results instead of Empty.
        for source in [SourceId::TpbMovies, SourceId::TpbTv] {
            let rows = parse(NO_RESULTS, source).expect("the sentinel is not a failure");
            assert!(rows.is_empty(), "{source} turned the sentinel into rows");
        }

        // Not merely a side effect of the category filter: the sentinel hash is
        // dropped even when it wears a category this source accepts.
        let disguised = NO_RESULTS.replace(r#""category":"0""#, r#""category":"207""#);
        assert!(
            parse(&disguised, SourceId::TpbMovies)
                .expect("parses")
                .is_empty()
        );

        assert!(
            movies()
                .iter()
                .chain(tv().iter())
                .all(|r| r.info_hash != NO_RESULTS_INFO_HASH)
        );
    }

    #[test]
    fn movies_and_tv_never_see_each_others_rows() {
        let movies = movies();
        let tv = tv();
        assert_eq!(movies.len(), 5, "201, 202, 207, 209 and the salvaged row");
        assert_eq!(tv.len(), 2, "205 and 208");

        assert!(
            movies.iter().all(|r| !r.name.contains("S02E05")),
            "a TV row reached tpb-movies"
        );
        assert!(
            tv.iter().all(|r| r.name.starts_with("Example Show")),
            "a movie row reached tpb-tv"
        );
        assert_eq!(
            tv.iter().filter(|r| r.source == SourceId::TpbTv).count(),
            tv.len(),
            "rows are tagged with the source that produced them"
        );

        // A category in neither list — the video tree also carries music videos
        // and clips — belongs to no source at all.
        assert!(
            movies
                .iter()
                .chain(tv.iter())
                .all(|r| !r.name.contains("Music Video")),
            "category 203 belongs to neither source"
        );
    }

    #[test]
    fn a_junk_numeric_field_degrades_instead_of_losing_the_row() {
        let movies = movies();
        let broken = movies
            .iter()
            .find(|r| r.name.starts_with("Broken Metadata"))
            .expect("a row with unparseable numbers is still downloadable");
        assert_eq!(broken.seeders, 0, "\"lots\" degrades to 0");
        assert_eq!(broken.leechers, 0, "an empty string degrades to 0");
        assert_eq!(broken.size_bytes, 0);
        assert_eq!(broken.num_files, None, "0 files means unknown, not none");
        assert_eq!(broken.added, None, "an unreadable date is absent, not 1970");
        assert_eq!(movies.len(), 5, "the other rows are untouched by it");
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded_or_displayed() {
        let rows: Vec<TorrentResult> = movies().into_iter().chain(tv()).collect();
        assert!(rows.iter().all(|r| r.info_hash.len() == 40));
        assert!(rows.iter().all(|r| r.magnet.is_some()));
        assert!(
            rows.iter().all(|r| !r.name.trim().is_empty()),
            "a nameless row is undisplayable (FR-14)"
        );
        assert!(
            rows.iter().all(|r| !r.name.contains("Truncated Hash")),
            "an 8-character infohash is not a usable id"
        );
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        assert!(matches!(
            parse("<html>nope", SourceId::TpbMovies),
            Err(SourceError::Parse(_))
        ));
        // apibay answers an overloaded backend with markup or an object, never a
        // partial array — neither must be mistaken for zero results.
        assert!(matches!(
            parse(r#"{"error":"busy"}"#, SourceId::TpbTv),
            Err(SourceError::Parse(_))
        ));
        assert!(matches!(
            parse(r#"[{"name":"truncated"#, SourceId::TpbMovies),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn an_empty_array_is_a_successful_empty_search() {
        assert!(parse("[]", SourceId::TpbMovies).expect("parses").is_empty());
        // Missing fields are absent, not fatal.
        assert!(
            parse(r#"[{}]"#, SourceId::TpbTv)
                .expect("parses")
                .is_empty()
        );
    }

    #[test]
    fn an_empty_query_browses_the_curated_top_list() {
        assert_eq!(
            request_path(SourceId::TpbMovies, ""),
            "/precompiled/data_top100_207.json"
        );
        assert_eq!(
            request_path(SourceId::TpbTv, "   "),
            "/precompiled/data_top100_208.json",
            "whitespace is not a query"
        );
        assert_eq!(
            request_path(SourceId::TpbMovies, "the matrix"),
            "/q.php?q=the+matrix&cat=200"
        );
        assert!(
            request_path(SourceId::TpbTv, "a&b=c").contains("q=a%26b%3Dc"),
            "a query must not be able to inject its own parameters"
        );
    }

    #[test]
    fn the_definitions_match_the_source_matrix() {
        let movies = TpbMovies::new();
        assert_eq!(movies.def().id, SourceId::TpbMovies);
        assert_eq!(movies.def().groups, &[SourceGroup::Movies]);
        assert!(movies.def().reports_health, "apibay publishes swarm counts");

        let tv = TpbTv::new();
        assert_eq!(tv.def().id, SourceId::TpbTv);
        assert_eq!(tv.def().groups, &[SourceGroup::Tv]);
        assert!(tv.def().reports_health);

        // One site, two sidebar rows — the label is shared on purpose.
        assert_eq!(movies.def().label, tv.def().label);
        assert_eq!(movies.def().homepage, tv.def().homepage);
    }

    #[test]
    fn a_source_outside_this_module_claims_no_categories() {
        // Guards the `_ => &[]` arm: a mis-wired registry must yield an empty
        // search, not TPB rows attributed to another source.
        assert!(parse(FIXTURE, SourceId::Yts).expect("parses").is_empty());
    }
}
