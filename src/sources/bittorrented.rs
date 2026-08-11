//! BitTorrented (Movies, HTML) — a small curated index with a mixed list page.
//!
//! Two decisions shape this module:
//!
//! * **A row takes the magnet only if the list page already gave it.** Some rows
//!   carry a magnet anchor inline and some link out to a detail page instead
//!   (`docs/sources.md` §3.8). The inline ones cost nothing extra, so they are
//!   taken and carry a real infohash; the rest return `magnet: None` and are
//!   resolved by [`Source::resolve_magnet`] when the user presses `d`. Fetching a
//!   detail page per row at search time would be the single largest avoidable
//!   latency cost in the product, and it would be spent on rows nobody opens.
//! * **A challenge page is a failure, not an empty search.** This is the sparsest
//!   source in the set — a genuine "no matches" is routine here, so it would be
//!   the easiest place in the app for a blocked scrape to hide behind a
//!   plausible-looking empty list. Zero rows *plus* no results table *plus* a
//!   challenge marker is [`SourceError::Blocked`].
//!
//! `docs/sources.md` §3.8 records the site's page structure as unverified, so the
//! two shapes this module assumes — the results table and the detail path — are
//! pinned by the committed fixture and by [`DETAIL_PREFIX`], and nowhere else.

use scraper::{ElementRef, Html, Selector};

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet};
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// Hosts, in preference order. A single canonical domain; mirrors, if the site
/// ever announces any, belong in config rather than in a release.
pub const HOSTS: &[&str] = &["bittorrented.com"];

/// Path prefix of a per-result detail page.
///
/// The one place the detail-URL shape is assumed. `docs/sources.md` §3.8 flags
/// the site's structure as unverified, so if the live site disagrees with the
/// fixture this constant and the fixture are the whole of the change.
const DETAIL_PREFIX: &str = "/torrent/";

const DEF: SourceDef = SourceDef {
    id: SourceId::Bittorrented,
    label: "BitTorrented",
    groups: &[SourceGroup::Movies],
    homepage: "https://bittorrented.com",
    reports_health: true,
};

/// Text that only ever appears on an anti-bot interstitial. Lowercase, because
/// [`challenge_marker`] compares against a lowercased copy of the body.
const CHALLENGE_MARKERS: &[&str] = &[
    "cf-browser-verification",
    "just a moment",
    "ddos-guard",
    "checking your browser",
    "cf_chl_opt",
    "attention required! | cloudflare",
];

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const TIB: u64 = 1024 * GIB;

/// First 8 hex characters of every locator this source mints.
///
/// It namespaces the locator so this site's torrent id and another site's id that
/// happen to share a number can never collide on `info_hash`, which the engine
/// uses as a cross-source dedupe key (`FR-56`).
const LOCATOR_TAG: &str = "b1770e77";

// ---------------------------------------------------------------------------
// Detail-page locator
// ---------------------------------------------------------------------------

/// Packs a site torrent id into a syntactically valid 40-hex `info_hash`.
///
/// Used only for rows whose magnet the list page withheld. Those rows have no
/// real infohash yet, and every downstream consumer still needs *an* id: the UI
/// keys rows by it, the queue dedupes by it, and `core::paths` refuses to build a
/// cache path from anything that is not 40 hex. So the id we can see — the one in
/// the detail link — is padded into that shape behind [`LOCATOR_TAG`], and
/// [`locator_torrent_id`] reads it back out when the user asks for the magnet.
///
/// TODO(engine): needs a detail-URL field on `TorrentResult`. This encoding is a
/// workaround for a frozen type, and it leaves one real rough edge: until
/// `resolve_magnet` runs, `info_hash` is a *locator*, not the torrent's actual
/// infohash. The engine must therefore re-key a queue item on the hash carried by
/// the resolved magnet instead of trusting the id it enqueued with, or a lazily
/// resolved download ends up filed under the wrong key. Rows that came with an
/// inline magnet are unaffected — theirs is the real hash from the first byte.
fn locator_id(torrent_id: u64) -> String {
    format!("{LOCATOR_TAG}{torrent_id:032x}")
}

/// Reads a torrent id back out of a locator minted by [`locator_id`].
///
/// `None` for a real infohash. A genuine hash could in principle start with the
/// tag (one in 2^32), but that only matters for a row whose magnet is already
/// known — and such a row never reaches this function, because
/// [`Source::resolve_magnet`] returns the magnet it already has first.
fn locator_torrent_id(info_hash: &str) -> Option<u64> {
    let packed = info_hash.strip_prefix(LOCATOR_TAG)?;
    let digits = packed.trim_start_matches('0');
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// The numeric torrent id inside a list-page href (`/torrent/482913`).
fn torrent_id_from_href(href: &str) -> Option<u64> {
    let rest = href.split(DETAIL_PREFIX).nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // Zero is rejected as well as unparseable text: `locator_id(0)` would encode
    // to all zeros, which `locator_torrent_id` cannot tell from padding.
    digits.parse::<u64>().ok().filter(|id| *id > 0)
}

/// Path of the detail page for one row.
fn detail_path(torrent_id: u64) -> String {
    format!("{DETAIL_PREFIX}{torrent_id}")
}

/// The search or browse path for one query.
fn list_path(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        // The homepage is the curated picks, in the same table the search page
        // uses — so browsing costs no second parser.
        return "/".to_string();
    }
    format!("/search?q={}", encode_query(q))
}

/// Percent-encodes a query for a URL query parameter.
fn encode_query(s: &str) -> String {
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

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Compiles a CSS selector.
///
/// Returns an error rather than panicking: `unwrap` is banned outside tests, and
/// a typo in a selector would otherwise take the process down mid-search.
fn sel(css: &'static str) -> Result<Selector, SourceError> {
    Selector::parse(css).map_err(|e| SourceError::Parse(format!("bad selector `{css}`: {e}")))
}

/// All of an element's text, with runs of whitespace collapsed.
fn text_of(el: ElementRef<'_>) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses a human-readable size ("2.4 GB", "700 MiB", "1,024 MB") into bytes.
///
/// 1024-based even for the SI spellings, because every tracker in this set
/// divides its byte count by 1024 and then labels the result "GB"; taking "GB" at
/// its SI word would understate every row by about 7%. Text with no number in it
/// is 0, meaning *unknown* — the row is still worth showing.
fn parse_size(raw: &str) -> u64 {
    let Some(start) = raw.find(|c: char| c.is_ascii_digit()) else {
        return 0;
    };
    let text = &raw[start..];
    let number_end = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (number, rest) = text.split_at(number_end);
    let unit_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let value: f64 = number
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .parse()
        .unwrap_or(0.0);
    let multiplier = match rest[..unit_end].to_ascii_lowercase().as_str() {
        "" | "b" | "byte" | "bytes" => 1,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => GIB,
        "t" | "tb" | "tib" => TIB,
        // An unrecognised unit is worse than no size: guessing would put a row
        // in the wrong place in a size-sorted list.
        _ => return 0,
    };
    (value * multiplier as f64).round().max(0.0) as u64
}

/// Parses a swarm count, tolerating the thousands separators the site prints.
fn parse_count(raw: &str) -> u32 {
    raw.chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Names the anti-bot layer when `body` is one of its challenge pages.
///
/// The lowercased copy is only ever made on the zero-row path, so a successful
/// search never pays to allocate a second copy of a results page.
fn challenge_marker(body: &str) -> Option<&'static str> {
    let lowered = body.to_ascii_lowercase();
    CHALLENGE_MARKERS
        .iter()
        .copied()
        .find(|marker| lowered.contains(marker))
}

/// Turns one results table into rows.
///
/// Free of I/O so it can be tested against a committed fixture — a scraper tested
/// only against the live site is a scraper that breaks silently (`FR-22`).
pub fn parse_list(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let doc = Html::parse_document(body);
    let table_sel = sel("table.torrent-list")?;
    let row_sel = sel("table.torrent-list tbody tr")?;
    let name_sel = sel("td.name a")?;
    let size_sel = sel("td.size")?;
    let seed_sel = sel("td.seeders")?;
    let leech_sel = sel("td.leechers")?;
    let magnet_sel = sel(r#"a[href^="magnet:"]"#)?;

    let mut out = Vec::new();
    for row in doc.select(&row_sel) {
        let Some(link) = row.select(&name_sel).next() else {
            continue;
        };
        let name = text_of(link);
        // A row with no name is unrenderable and is dropped rather than shown
        // (`FR-14`).
        if name.is_empty() {
            continue;
        }

        // The curated picks publish their magnet in the row itself. Taking it
        // costs nothing and gives the row a real infohash immediately, which is
        // strictly better than deferring — the deferral exists to avoid a
        // *request*, not to avoid a magnet.
        let inline = row
            .select(&magnet_sel)
            .find_map(|a| a.attr("href").and_then(info_hash_from_magnet));

        let (info_hash, magnet) = match inline {
            // Rebuilt rather than passed through: the site's magnet carries its
            // tracker list, and every magnet in the app must be byte-identical
            // for the same torrent because the cache and the queue compare them.
            Some(hash) => {
                let magnet = build_magnet(&hash, &name);
                (hash, Some(magnet))
            }
            // No magnet here — the row must at least be able to find its detail
            // page later, or it could never be downloaded and is dropped.
            None => match link.attr("href").and_then(torrent_id_from_href) {
                Some(id) => (locator_id(id), None),
                None => continue,
            },
        };

        out.push(TorrentResult {
            info_hash,
            name,
            size_bytes: row
                .select(&size_sel)
                .next()
                .map_or(0, |c| parse_size(&text_of(c))),
            seeders: row
                .select(&seed_sel)
                .next()
                .map_or(0, |c| parse_count(&text_of(c))),
            leechers: row
                .select(&leech_sel)
                .next()
                .map_or(0, |c| parse_count(&text_of(c))),
            num_files: None,
            source: SourceId::Bittorrented,
            // The table carries no publication date. `None` is the honest answer;
            // a fabricated one would sort wrongly and silently.
            added: None,
            magnet,
        });
    }

    // Zero rows is a legitimate answer for a small curated catalog. It is also
    // what a blocked scrape looks like, and only the absence of the table itself,
    // together with a challenge marker, separates the two.
    if out.is_empty()
        && doc.select(&table_sel).next().is_none()
        && let Some(marker) = challenge_marker(body)
    {
        return Err(SourceError::Blocked(format!(
            "BitTorrented served an anti-bot challenge ({marker})"
        )));
    }
    Ok(out)
}

/// Extracts the magnet from a detail page and rebuilds it canonically.
///
/// Returns a [`build_magnet`] string rather than the site's own for the same
/// reason the list-page path does: one byte-identical magnet per torrent, with
/// trackers left to the engine.
pub fn parse_magnet(body: &str, name: &str) -> Result<String, SourceError> {
    let doc = Html::parse_document(body);
    let magnet_sel = sel(r#"a[href^="magnet:"]"#)?;
    let hash = doc
        .select(&magnet_sel)
        .find_map(|a| a.attr("href").and_then(info_hash_from_magnet))
        // Last resort: the raw document, which reaches magnets inlined in a
        // `<script>` or printed as plain text where no selector can follow.
        .or_else(|| info_hash_from_magnet(body));

    let Some(hash) = hash else {
        // A detail page that answers with a challenge must not read as "this
        // torrent has no magnet" — that would look like a broken result to the
        // user instead of a blocked source.
        if let Some(marker) = challenge_marker(body) {
            return Err(SourceError::Blocked(format!(
                "BitTorrented served an anti-bot challenge ({marker})"
            )));
        }
        return Err(SourceError::Parse(
            "no magnet on the BitTorrented detail page".into(),
        ));
    };
    Ok(build_magnet(&hash, name))
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// BitTorrented — a small curated movie index.
pub struct Bittorrented {
    client: SourceClient,
}

impl Bittorrented {
    /// Builds the source with its own connection pool.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for Bittorrented {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Bittorrented {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            let (body, _host) = self
                .client
                .get_text_failover(HOSTS, &list_path(query), ctx)
                .await?;
            parse_list(&body)
        })
    }

    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        Box::pin(async move {
            // The list page supplies a magnet for some rows; those must never
            // cost a request, however many times `d` is pressed.
            if let Some(magnet) = &result.magnet {
                return Ok(magnet.clone());
            }
            let Some(torrent_id) = locator_torrent_id(&result.info_hash) else {
                return Err(SourceError::Parse(
                    "this BitTorrented row carries no detail-page locator".into(),
                ));
            };
            let (body, _host) = self
                .client
                .get_text_failover(HOSTS, &detail_path(torrent_id), ctx)
                .await?;
            parse_magnet(&body, &result.name)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::magnet::normalize_info_hash;

    const LIST: &str = include_str!("fixtures/bittorrented.html");

    /// A detail page, trimmed to the part this module reads.
    const DETAIL: &str = r#"<!DOCTYPE html><html><body><main>
        <h1>Dune: Part Two (2024) 2160p WEB-DL DDP5.1 Atmos</h1>
        <dl class="meta"><dt>Size</dt><dd>14.8 GB</dd><dt>Seeders</dt><dd>903</dd></dl>
        <p class="get"><a class="magnet" href="magnet:?xt=urn:btih:2B7E4A0C93D15F86AE20C4B7D9081E3F6A5C4D2B&amp;dn=Dune+Part+Two+2024&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337">Magnet link</a></p>
        </main></body></html>"#;

    /// The real bytes a blocked scrape gets back: a full page with no results
    /// table on it at all.
    const CHALLENGE: &str = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
        <body class="no-js"><div class="cf-browser-verification cf-im-under-attack">
        <h1>Checking your browser before accessing bittorrented.com</h1></div>
        <script>window._cf_chl_opt={cvId:"3"};</script></body></html>"#;

    /// The site's own "nothing matched" page: the table is there, the tbody is
    /// empty. Routine for a catalog this small.
    const NO_RESULTS: &str = r#"<!DOCTYPE html><html><body><main>
        <h1 class="page-title">0 results</h1>
        <table class="torrent-list"><thead><tr><th class="name">Name</th></tr></thead>
        <tbody></tbody></table></main></body></html>"#;

    #[test]
    fn parses_every_column_the_list_page_offers() {
        let rows = parse_list(LIST).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(first.name, "The Northman (2022) 1080p BluRay x264 AAC5.1");
        assert_eq!(first.size_bytes, 2_576_980_378, "2.4 GB, 1024-based");
        assert_eq!(first.seeders, 1842);
        assert_eq!(first.leechers, 311);
        assert_eq!(first.source, SourceId::Bittorrented);
        assert_eq!(first.added, None, "the table carries no date");
        assert_eq!(first.num_files, None);
    }

    #[test]
    fn a_row_that_already_carries_a_magnet_is_not_deferred() {
        // Deferring exists to avoid a *request*. A magnet the list page handed us
        // costs nothing, so that row keeps it — and gets a real infohash with it.
        let rows = parse_list(LIST).expect("parses");
        let magnet = rows[0].magnet.as_deref().expect("the first row is a pick");
        assert_eq!(
            rows[0].info_hash, "8f1b3c9d2e4a5b6c7d8e9f0a1b2c3d4e5f60718a",
            "an inline magnet gives the row its real hash, lowercased"
        );
        assert!(magnet.contains(&rows[0].info_hash));
        assert!(
            !magnet.contains("&tr="),
            "the site's trackers are dropped; the engine supplies its own"
        );
        assert_eq!(
            locator_torrent_id(&rows[0].info_hash),
            None,
            "a real hash must not read as a locator"
        );
    }

    #[test]
    fn a_row_without_an_inline_magnet_defers_it_to_a_single_detail_fetch() {
        let rows = parse_list(LIST).expect("parses");
        let deferred = &rows[1];
        assert_eq!(deferred.magnet, None);
        assert_eq!(
            locator_torrent_id(&deferred.info_hash),
            Some(482_755),
            "the row must be able to find its own detail page"
        );
        assert_eq!(detail_path(482_755), "/torrent/482755");
        assert!(
            normalize_info_hash(&deferred.info_hash).is_some(),
            "a locator must still be a usable id for the cache and the queue"
        );
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded() {
        // The fixture's fourth row has an empty title anchor.
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows.len(), 3, "one unusable row must not be rendered");
        assert!(rows.iter().all(|r| !r.name.is_empty()));
    }

    #[test]
    fn thousands_separators_in_the_size_column_are_tolerated() {
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(
            rows[2].size_bytes, 1_073_741_824,
            "the fixture prints 1,024 MB"
        );
        assert_eq!(rows[1].size_bytes, 15_891_378_995, "14.8 GB");
    }

    #[test]
    fn resolve_magnet_rebuilds_a_canonical_magnet_from_the_detail_page() {
        let magnet = parse_magnet(DETAIL, "Dune: Part Two").expect("detail parses");
        assert!(
            magnet.starts_with("magnet:?xt=urn:btih:2b7e4a0c93d15f86ae20c4b7d9081e3f6a5c4d2b"),
            "the page's uppercase hash must be lowercased at the boundary: {magnet}"
        );
        assert!(!magnet.contains("&tr="));
    }

    #[test]
    fn a_detail_page_with_no_magnet_at_all_is_a_parse_error() {
        assert!(matches!(
            parse_magnet("<html><body>removed</body></html>", "x"),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn a_challenge_page_is_blocked_not_an_empty_search() {
        // This is the sparsest source in the set, so an empty list is the easiest
        // place in the app for a block to hide.
        let err = parse_list(CHALLENGE).expect_err("a challenge must not parse as success");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
        assert!(err.is_hard_host_failure(), "a block parks the host");

        let err = parse_magnet(CHALLENGE, "x").expect_err("detail challenge");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
    }

    #[test]
    fn an_empty_results_table_is_a_successful_empty_search() {
        assert_eq!(parse_list(NO_RESULTS).expect("parses").len(), 0);
    }

    #[test]
    fn malformed_markup_never_panics_and_never_invents_rows() {
        for junk in [
            "",
            "<html",
            "<table class=\"torrent-list\"><tbody><tr><td class=\"name\">",
            "not html at all",
            "\u{0}\u{1}\u{2}",
        ] {
            assert_eq!(parse_list(junk).expect("no error on junk").len(), 0);
        }
    }

    #[test]
    fn a_locator_round_trips_and_stays_a_valid_infohash() {
        // The tag has to be hex and exactly 8 characters or the locator stops
        // being a 40-hex id, and `core::paths` refuses to build a cache path
        // from it. This is what objects if someone edits LOCATOR_TAG carelessly.
        assert_eq!(LOCATOR_TAG.len(), 8);
        assert!(LOCATOR_TAG.chars().all(|c| c.is_ascii_hexdigit()));

        for id in [1u64, 482_913, u64::MAX] {
            let packed = locator_id(id);
            assert_eq!(normalize_info_hash(&packed).as_deref(), Some(&*packed));
            assert_eq!(locator_torrent_id(&packed), Some(id));
        }
        assert_eq!(
            locator_torrent_id(&format!("{LOCATOR_TAG}{}", "0".repeat(32))),
            None
        );
    }

    #[test]
    fn human_sizes_parse_to_bytes() {
        assert_eq!(parse_size("2.4 GB"), 2_576_980_378);
        assert_eq!(parse_size("700 MiB"), 734_003_200);
        assert_eq!(parse_size("1,024 MB"), 1_073_741_824);
        assert_eq!(parse_size("512 B"), 512);
        assert_eq!(parse_size("1.5 TB"), 1_649_267_441_664);
        assert_eq!(parse_size("—"), 0);
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size("12 parsecs"), 0);
    }

    #[test]
    fn a_query_is_url_encoded_and_an_empty_one_browses() {
        assert_eq!(list_path("the northman"), "/search?q=the+northman");
        assert_eq!(list_path("a&b=c"), "/search?q=a%26b%3Dc");
        assert_eq!(
            list_path("  "),
            "/",
            "an empty query is the curated picks, not a search for nothing"
        );
        assert!(list_path("進撃").is_ascii());
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let s = Bittorrented::new();
        assert_eq!(s.def().id, SourceId::Bittorrented);
        assert_eq!(s.def().groups, &[SourceGroup::Movies]);
        assert!(s.def().reports_health, "the table prints seeders");
    }
}
