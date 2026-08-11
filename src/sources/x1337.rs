//! 1337x (Movies + TV, HTML) — one parser, two sources, magnets fetched lazily.
//!
//! Three decisions shape this module and none of them are obvious from the
//! markup, so they are argued here rather than left to be rediscovered:
//!
//! * **The magnet is not fetched at search time.** 1337x keeps it on a per-result
//!   detail page while the list page already carries everything a row needs to be
//!   *displayed* — name, size, seeders, leechers. Fetching one detail page per row
//!   would turn a one-request search into an N+1 burst against the flakiest host
//!   in the set, doubled because `FR-11` registers 1337x twice. So `search`
//!   returns `magnet: None` and [`Source::resolve_magnet`] pays for exactly the
//!   one row the user pressed `d` on (`docs/sources.md` §3.4,
//!   `notes-for-dhruv.md` §1).
//! * **The detail page is located from the row itself.** `TorrentResult` is frozen
//!   and has no field for a detail URL, so the row's numeric torrent id is packed
//!   into `info_hash` as a *locator* (see [`locator_id`]) and the URL slug is
//!   reproduced from `name`. Nothing has to be remembered between the two calls,
//!   which keeps the source stateless as the contract requires.
//! * **A challenge page is a failure, not an empty search.** Cloudflare answers a
//!   blocked scrape with a page that parses cleanly into zero rows. Reporting that
//!   as "nothing found" would hide a blocked source behind a plausible-looking
//!   empty result list, so zero rows *plus* no results table *plus* a challenge
//!   marker is [`SourceError::Blocked`].

use scraper::{ElementRef, Html, Selector};

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet, normalize_info_hash};
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// Mirrors, in preference order. The engine's sticky hint reorders these.
pub const HOSTS: &[&str] = &["1337x.to", "1337x.st", "x1337x.ws", "1337xx.to"];

/// Category segment of the search path for the Movies source.
const MOVIES_CATEGORY: &str = "Movies";

/// Curated top list for the Movies source — used when the query is empty.
const MOVIES_BROWSE: &str = "/top-100-movies";

/// Category segment of the search path for the TV source.
const TV_CATEGORY: &str = "TV";

/// Curated top list for the TV source.
const TV_BROWSE: &str = "/top-100-television";

const MOVIES_DEF: SourceDef = SourceDef {
    id: SourceId::X1337Movies,
    label: "1337x",
    groups: &[SourceGroup::Movies],
    homepage: "https://1337x.to",
    reports_health: true,
};

const TV_DEF: SourceDef = SourceDef {
    id: SourceId::X1337Tv,
    label: "1337x",
    groups: &[SourceGroup::Tv],
    homepage: "https://1337x.to",
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
/// It namespaces the locator so a 1337x torrent id and a FitGirl post id that
/// happen to share a number can never collide on `info_hash`, which the engine
/// uses as a cross-source dedupe key (`FR-56`). Both 1337x sources share one tag
/// on purpose: it is one site with one torrent id space, so the same release
/// found under Movies and under TV is correctly recognised as one thing.
const LOCATOR_TAG: &str = "13371337";

// ---------------------------------------------------------------------------
// Detail-page locator
// ---------------------------------------------------------------------------

/// Packs a 1337x torrent id into a syntactically valid 40-hex `info_hash`.
///
/// The real infohash lives on the detail page, and this row is not going to pay
/// for that page (see the module docs) — but every downstream consumer still
/// needs *an* id: the UI keys rows by it, the queue dedupes by it, and
/// `core::paths` refuses to build a cache path from anything that is not 40 hex.
/// So the id we can see — the torrent id in the list page's href — is padded
/// into that shape behind [`LOCATOR_TAG`], and [`locator_torrent_id`] reads it
/// back out when the user finally asks for the magnet.
///
/// TODO(engine): needs a detail-URL field on `TorrentResult`. This encoding is a
/// workaround for a frozen type, and it leaves one real rough edge: until
/// `resolve_magnet` runs, `info_hash` is a *locator*, not the torrent's actual
/// infohash. The engine must therefore re-key a queue item on the hash carried by
/// the resolved magnet instead of trusting the id it enqueued with, or a lazily
/// resolved download ends up filed under the wrong key.
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

/// The slug 1337x puts in a detail URL, derived from the torrent name.
///
/// The site builds it by collapsing every run of non-alphanumerics into one `-`,
/// so it can be reproduced from the name the list page already gave us — which is
/// what makes the detail page reachable without a field to carry its URL.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut gap = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Path of the detail page for one row.
fn detail_path(torrent_id: u64, name: &str) -> String {
    let slug = slug(name);
    if slug.is_empty() {
        // A title with no ASCII alphanumerics at all. The id is the part that
        // selects the torrent, so ask for it without a slug rather than sending
        // an empty path segment.
        return format!("/torrent/{torrent_id}/");
    }
    format!("/torrent/{torrent_id}/{slug}/")
}

/// The numeric torrent id inside a list-page href (`/torrent/5231947/slug/`).
fn torrent_id_from_href(href: &str) -> Option<u64> {
    let rest = href.split("/torrent/").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // Zero is rejected as well as unparseable text: `locator_id(0)` would encode
    // to all zeros, which `locator_torrent_id` cannot tell from padding.
    digits.parse::<u64>().ok().filter(|id| *id > 0)
}

/// The search or browse path for one query.
fn list_path(query: &str, category: &str, browse: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return browse.to_string();
    }
    // Sorted by seeders descending: the list page is all the user sees for this
    // source, so the healthiest swarms belong on it (`docs/sources.md` §3.4).
    format!(
        "/sort-category-search/{}/{category}/seeders/desc/1/",
        encode_segment(q)
    )
}

/// Percent-encodes a query for use as a URL *path segment*.
///
/// Deliberately not the query-string encoder: `+` means a literal plus inside a
/// path, so encoding a space as `+` would search 1337x for the wrong title.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
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

/// The first non-blank text node of an element.
///
/// The size cell is `2.4 GB<span class="seeds mob-seeds">1842</span>` — markup for
/// the mobile layout that lives inside the desktop cell. Taking all of the cell's
/// text would read "2.4 GB1842" and parse to a nonsense size, so only the cell's
/// own leading text counts.
fn first_text(el: ElementRef<'_>) -> String {
    el.text()
        .map(str::trim)
        .find(|t| !t.is_empty())
        .unwrap_or_default()
        .to_string()
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

/// Parses a 1337x results table, tagging rows for the Movies source.
///
/// Free of I/O so it can be tested against a committed fixture — a scraper tested
/// only against the live site is a scraper that breaks silently (`FR-22`).
pub fn parse_list(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    parse_list_for(body, SourceId::X1337Movies)
}

/// The same parse, tagged for whichever of the two 1337x sources asked.
///
/// Category narrowing on 1337x is server-side and the results table carries no
/// per-row category signal, so the two sources fetch two URLs and share only this
/// parser — the one thing that was ever worth sharing (`notes-for-dhruv.md` §1).
pub fn parse_list_for(body: &str, source: SourceId) -> Result<Vec<TorrentResult>, SourceError> {
    let doc = Html::parse_document(body);
    let table_sel = sel("table.table-list")?;
    let row_sel = sel("table.table-list tbody tr")?;
    let name_sel = sel(r#"td.coll-1 a[href*="/torrent/"]"#)?;
    let seed_sel = sel("td.coll-2")?;
    let leech_sel = sel("td.coll-3")?;
    let size_sel = sel("td.coll-4")?;

    let mut out = Vec::new();
    for row in doc.select(&row_sel) {
        // The name cell holds a category-icon anchor before the title anchor, so
        // the selector asks for the one that points at a torrent, not the first.
        let Some(link) = row.select(&name_sel).next() else {
            continue;
        };
        let name = text_of(link);
        // A row with no name is unrenderable, and one with no torrent id can
        // never reach its magnet — both are dropped rather than shown (`FR-14`).
        if name.is_empty() {
            continue;
        }
        let Some(torrent_id) = link.attr("href").and_then(torrent_id_from_href) else {
            continue;
        };

        out.push(TorrentResult {
            info_hash: locator_id(torrent_id),
            name,
            size_bytes: row
                .select(&size_sel)
                .next()
                .map_or(0, |c| parse_size(&first_text(c))),
            seeders: row
                .select(&seed_sel)
                .next()
                .map_or(0, |c| parse_count(&text_of(c))),
            leechers: row
                .select(&leech_sel)
                .next()
                .map_or(0, |c| parse_count(&text_of(c))),
            num_files: None,
            source,
            // The date column reads "Jun. 14th '24" — a month abbreviation and a
            // two-digit year with no timezone. Guessing a timestamp out of that
            // would be worse than admitting we do not have one, so this stays
            // `None` rather than inventing a sort key.
            added: None,
            magnet: None,
        });
    }

    // Zero rows is a legitimate answer *and* what a blocked scrape looks like.
    // Only the absence of the table itself, together with a challenge marker,
    // separates the two.
    if out.is_empty()
        && doc.select(&table_sel).next().is_none()
        && let Some(marker) = challenge_marker(body)
    {
        return Err(SourceError::Blocked(format!(
            "1337x served an anti-bot challenge ({marker})"
        )));
    }
    Ok(out)
}

/// Extracts the magnet from a 1337x detail page and rebuilds it canonically.
///
/// The site's own magnet carries its tracker list; this returns a
/// [`build_magnet`] one instead so every magnet in the app is byte-identical for
/// the same torrent (the cache and the queue compare them) and the engine stays
/// the only thing that decides trackers.
pub fn parse_magnet(body: &str, name: &str) -> Result<String, SourceError> {
    let doc = Html::parse_document(body);

    let magnet_sel = sel(r#"a[href^="magnet:"]"#)?;
    let mut hash = doc
        .select(&magnet_sel)
        .find_map(|a| a.attr("href").and_then(info_hash_from_magnet));

    if hash.is_none() {
        // Some mirrors build the magnet in JavaScript and only print the hash in
        // the detail box. That is still a complete answer — we build the magnet.
        let box_sel = sel("div.infohash-box p")?;
        hash = doc.select(&box_sel).find_map(|el| {
            el.text()
                .collect::<String>()
                .split_whitespace()
                .find_map(normalize_info_hash)
        });
    }
    if hash.is_none() {
        // Last resort: the raw document, which reaches magnets inlined in a
        // `<script>` where no selector can follow.
        hash = info_hash_from_magnet(body);
    }

    let Some(hash) = hash else {
        // A detail page that answers with a challenge must not read as "this
        // torrent has no magnet" — that would look like a broken result to the
        // user instead of a blocked source.
        if let Some(marker) = challenge_marker(body) {
            return Err(SourceError::Blocked(format!(
                "1337x served an anti-bot challenge ({marker})"
            )));
        }
        return Err(SourceError::Parse(
            "no magnet or infohash on the 1337x detail page".into(),
        ));
    };
    Ok(build_magnet(&hash, name))
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Fetches and parses one results page.
async fn fetch_list(
    client: &SourceClient,
    query: &str,
    category: &str,
    browse: &str,
    source: SourceId,
    ctx: &SearchCtx,
) -> Result<Vec<TorrentResult>, SourceError> {
    let path = list_path(query, category, browse);
    let (body, _host) = client.get_text_failover(HOSTS, &path, ctx).await?;
    parse_list_for(&body, source)
}

/// Fetches the one detail page the user actually asked for.
async fn fetch_magnet(
    client: &SourceClient,
    result: &TorrentResult,
    ctx: &SearchCtx,
) -> Result<String, SourceError> {
    // A row that already carries a magnet costs nothing — pressing `d` twice on
    // the same result must not fetch the page twice.
    if let Some(magnet) = &result.magnet {
        return Ok(magnet.clone());
    }
    let Some(torrent_id) = locator_torrent_id(&result.info_hash) else {
        return Err(SourceError::Parse(
            "this 1337x row carries no detail-page locator".into(),
        ));
    };
    let path = detail_path(torrent_id, &result.name);
    let (body, _host) = client.get_text_failover(HOSTS, &path, ctx).await?;
    parse_magnet(&body, &result.name)
}

/// 1337x, narrowed to the Movies category.
pub struct X1337Movies {
    client: SourceClient,
}

impl X1337Movies {
    /// Builds the source with its own connection pool.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for X1337Movies {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for X1337Movies {
    fn def(&self) -> &'static SourceDef {
        &MOVIES_DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(fetch_list(
            &self.client,
            query,
            MOVIES_CATEGORY,
            MOVIES_BROWSE,
            SourceId::X1337Movies,
            ctx,
        ))
    }

    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        Box::pin(fetch_magnet(&self.client, result, ctx))
    }
}

/// 1337x, narrowed to the TV category.
pub struct X1337Tv {
    client: SourceClient,
}

impl X1337Tv {
    /// Builds the source with its own connection pool.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for X1337Tv {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for X1337Tv {
    fn def(&self) -> &'static SourceDef {
        &TV_DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(fetch_list(
            &self.client,
            query,
            TV_CATEGORY,
            TV_BROWSE,
            SourceId::X1337Tv,
            ctx,
        ))
    }

    fn resolve_magnet<'a>(
        &'a self,
        result: &'a TorrentResult,
        ctx: &'a SearchCtx,
    ) -> MagnetFuture<'a> {
        Box::pin(fetch_magnet(&self.client, result, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = include_str!("fixtures/x1337.html");
    const DETAIL: &str = include_str!("fixtures/x1337_detail.html");

    /// The real bytes a blocked scrape gets back: a full page with no results
    /// table on it at all.
    const CHALLENGE: &str = r#"<!DOCTYPE html><html><head><title>Just a moment...</title>
        <meta http-equiv="refresh" content="10"></head>
        <body class="no-js"><div class="cf-browser-verification cf-im-under-attack">
        <h1>Checking your browser before accessing 1337x.to</h1></div>
        <script>window._cf_chl_opt={cvId:"3"};</script></body></html>"#;

    /// The site's own "nothing matched" page: the table is there, the tbody is
    /// empty.
    const NO_RESULTS: &str = r#"<!DOCTYPE html><html><body>
        <div class="box-info"><h1>No results were returned</h1>
        <table class="table-list table"><thead><tr><th class="coll-1 name">Name</th></tr></thead>
        <tbody></tbody></table></div></body></html>"#;

    #[test]
    fn parses_every_column_the_list_page_offers() {
        let rows = parse_list(LIST).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(
            first.name,
            "The.Northman.2022.1080p.BluRay.x264.AAC5.1-[YTS.MX]"
        );
        assert_eq!(first.size_bytes, 2_576_980_378, "2.4 GB, 1024-based");
        assert_eq!(first.seeders, 1842);
        assert_eq!(first.leechers, 311);
        assert_eq!(first.source, SourceId::X1337Movies);
        assert_eq!(first.num_files, None);
        assert_eq!(first.added, None, "the date column is not machine-readable");
    }

    #[test]
    fn the_mobile_span_inside_the_size_cell_does_not_corrupt_the_size() {
        // `<td class="coll-4">2.4 GB<span>1842</span></td>` reads as "2.4 GB1842"
        // if the whole cell's text is taken. That would parse to a nonsense size.
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows[1].size_bytes, 20_078_972_109, "18.7 GB");
        assert_eq!(rows[2].size_bytes, 996_566_630, "950.4 MB");
    }

    #[test]
    fn thousands_separators_in_the_swarm_columns_are_tolerated() {
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows[2].seeders, 1204, "the fixture prints `1,204`");
    }

    #[test]
    fn search_rows_defer_the_magnet_instead_of_fetching_a_detail_page_each() {
        // The whole point of the module: a displayable row never requires the
        // magnet, so a search is one request no matter how many results it holds.
        let rows = parse_list(LIST).expect("parses");
        assert!(
            rows.iter().all(|r| r.magnet.is_none()),
            "no row may carry a magnet the list page could not have supplied"
        );
        assert!(
            rows.iter().all(|r| r.info_hash.len() == 40),
            "a locator must still be a usable id for the cache and the queue"
        );
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded() {
        // The fixture holds five rows: one with an empty title anchor and one
        // whose only anchor points at a user profile rather than a torrent.
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows.len(), 3, "two unusable rows must not be rendered");
        assert!(rows.iter().all(|r| !r.name.is_empty()));
        assert!(
            rows.iter()
                .all(|r| locator_torrent_id(&r.info_hash).is_some()),
            "every surviving row must be able to find its detail page"
        );
    }

    #[test]
    fn the_detail_path_is_rebuilt_from_the_row_the_list_page_gave_us() {
        // The strong form of the claim in `slug`'s docs: the path we reconstruct
        // is byte-identical to the href the site itself printed.
        let rows = parse_list(LIST).expect("parses");
        let expected = [
            "/torrent/5231947/The-Northman-2022-1080p-BluRay-x264-AAC5-1-YTS-MX/",
            "/torrent/5231902/The-Northman-2022-2160p-UHD-BluRay-x265-TERMiNAL/",
            "/torrent/5230877/The-Northman-2022-720p-WEBRip-x264-GalaxyRG/",
        ];
        for (row, want) in rows.iter().zip(expected) {
            let id = locator_torrent_id(&row.info_hash).expect("locator");
            assert_eq!(detail_path(id, &row.name), want);
            assert!(
                LIST.contains(want),
                "the fixture must print this exact href"
            );
        }
    }

    #[test]
    fn a_locator_round_trips_and_stays_a_valid_infohash() {
        // The tag has to be hex and exactly 8 characters or the locator stops
        // being a 40-hex id, and `core::paths` refuses to build a cache path
        // from it. This is what objects if someone edits LOCATOR_TAG carelessly.
        assert_eq!(LOCATOR_TAG.len(), 8);
        assert!(LOCATOR_TAG.chars().all(|c| c.is_ascii_hexdigit()));

        for id in [1u64, 42, 5_231_947, u64::MAX] {
            let packed = locator_id(id);
            assert_eq!(packed.len(), 40);
            assert_eq!(normalize_info_hash(&packed).as_deref(), Some(&*packed));
            assert_eq!(locator_torrent_id(&packed), Some(id));
        }
        // A real infohash is not mistaken for a locator.
        assert_eq!(
            locator_torrent_id("8f1b3c9d2e4a5b6c7d8e9f0a1b2c3d4e5f60718a"),
            None
        );
        // Nor is an all-padding locator, which would decode to a torrent id of 0.
        assert_eq!(
            locator_torrent_id(&format!("{LOCATOR_TAG}{}", "0".repeat(32))),
            None
        );
    }

    #[test]
    fn resolve_magnet_rebuilds_a_canonical_magnet_from_the_detail_page() {
        let magnet = parse_magnet(DETAIL, "The.Northman.2022.1080p").expect("detail parses");
        assert!(
            magnet.starts_with("magnet:?xt=urn:btih:8f1b3c9d2e4a5b6c7d8e9f0a1b2c3d4e5f60718a"),
            "the fixture's uppercase hash must be lowercased at the boundary: {magnet}"
        );
        assert!(
            !magnet.contains("&tr="),
            "the site's trackers are dropped; the engine supplies its own"
        );
        assert_eq!(
            info_hash_from_magnet(&magnet).as_deref(),
            Some("8f1b3c9d2e4a5b6c7d8e9f0a1b2c3d4e5f60718a")
        );
    }

    #[test]
    fn a_detail_page_without_an_anchor_still_yields_the_infohash_box() {
        // Mirrors that build the magnet in JavaScript still print the hash.
        let only_box = r#"<html><body><div class="infohash-box">
            <p><span>Infohash :</span> 8F1B3C9D2E4A5B6C7D8E9F0A1B2C3D4E5F60718A</p>
            </div></body></html>"#;
        let magnet = parse_magnet(only_box, "Whatever").expect("hash found");
        assert!(magnet.contains("8f1b3c9d2e4a5b6c7d8e9f0a1b2c3d4e5f60718a"));
    }

    #[test]
    fn a_detail_page_with_no_magnet_at_all_is_a_parse_error() {
        assert!(matches!(
            parse_magnet("<html><body>gone</body></html>", "x"),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn a_challenge_page_is_blocked_not_an_empty_search() {
        // An empty list here would render as "1337x found nothing", quietly
        // hiding a blocked source behind a plausible answer.
        let err = parse_list(CHALLENGE).expect_err("a challenge must not parse as success");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
        assert!(err.is_hard_host_failure(), "a block parks the host");

        let err = parse_magnet(CHALLENGE, "x").expect_err("detail challenge");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
    }

    #[test]
    fn an_empty_results_table_is_a_successful_empty_search() {
        // The table is present, so this is the site saying "no matches" — not a
        // block, and not an error.
        assert_eq!(parse_list(NO_RESULTS).expect("parses").len(), 0);
    }

    #[test]
    fn malformed_markup_never_panics_and_never_invents_rows() {
        for junk in [
            "",
            "<html",
            "<table class=\"table-list\"><tbody><tr><td>",
            "not html at all",
            "\u{0}\u{1}\u{2}",
        ] {
            assert_eq!(parse_list(junk).expect("no error on junk").len(), 0);
        }
    }

    #[test]
    fn human_sizes_parse_to_bytes() {
        assert_eq!(parse_size("2.4 GB"), 2_576_980_378);
        assert_eq!(parse_size("700 MiB"), 734_003_200);
        assert_eq!(parse_size("1,024 MB"), 1_073_741_824);
        assert_eq!(parse_size("512 B"), 512);
        assert_eq!(parse_size("4.0KB"), 4096);
        assert_eq!(parse_size("1.5 TB"), 1_649_267_441_664);
        // Unknown rather than wrong.
        assert_eq!(parse_size("—"), 0);
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size("12 parsecs"), 0);
    }

    #[test]
    fn both_sources_share_one_parser_but_keep_their_own_identity() {
        let movies = parse_list_for(LIST, SourceId::X1337Movies).expect("parses");
        let tv = parse_list_for(LIST, SourceId::X1337Tv).expect("parses");
        assert_eq!(movies.len(), tv.len());
        assert!(tv.iter().all(|r| r.source == SourceId::X1337Tv));
        // Same site, same torrent id space: a release found under both categories
        // must land on one dedupe key, not two.
        assert_eq!(
            movies[0].info_hash, tv[0].info_hash,
            "the locator must not depend on which of the two sources parsed it"
        );
    }

    #[test]
    fn a_query_is_encoded_for_a_path_segment_and_an_empty_one_browses() {
        assert_eq!(
            list_path("the northman", MOVIES_CATEGORY, MOVIES_BROWSE),
            "/sort-category-search/the%20northman/Movies/seeders/desc/1/",
            "a space must not become `+` inside a path"
        );
        assert_eq!(
            list_path("  ", TV_CATEGORY, TV_BROWSE),
            "/top-100-television",
            "an empty query is the curated top list, not a search for nothing"
        );
        assert!(list_path("a/b?c", MOVIES_CATEGORY, MOVIES_BROWSE).contains("a%2Fb%3Fc"));
        assert!(list_path("進撃", TV_CATEGORY, TV_BROWSE).is_ascii());
    }

    #[test]
    fn the_definitions_match_the_source_matrix() {
        let movies = X1337Movies::new();
        assert_eq!(movies.def().id, SourceId::X1337Movies);
        assert_eq!(movies.def().groups, &[SourceGroup::Movies]);
        assert!(movies.def().reports_health, "the list page prints seeders");

        let tv = X1337Tv::new();
        assert_eq!(tv.def().id, SourceId::X1337Tv);
        assert_eq!(tv.def().groups, &[SourceGroup::Tv]);
        assert!(tv.def().reports_health);
        assert_eq!(HOSTS.len(), 4, "the mirror chain is config, not code");
    }
}
