//! FitGirl Repacks (Games, HTML) — a WordPress feed, so no swarm data at all.
//!
//! Three things make this source unlike the others and each is argued at its
//! decision site below:
//!
//! * **`reports_health` is false.** FitGirl is a blog. Its posts carry a title, a
//!   date and a repack size, and nothing anywhere on the site knows how many
//!   peers a torrent has. `seeders: 0` from here means *unknown*, never *dead* —
//!   an alive-only filter that drops these rows would empty the Games group,
//!   which is why the flag exists (`SourceDef::reports_health`).
//! * **The magnet is not fetched at search time.** It lives in the post body, and
//!   a FitGirl post is megabytes of HTML. Fetching one per search result would
//!   make the cheapest source in the set the most expensive by an order of
//!   magnitude, for data the user has not asked for yet. `search` returns
//!   `magnet: None`; [`Source::resolve_magnet`] fetches the one post behind the
//!   row the user pressed `d` on (`docs/sources.md` §3.1).
//! * **The post is located by its WordPress id, not its permalink.** See
//!   [`locator_id`] — the id survives the slug edits and `-2` suffixes that
//!   WordPress inflicts on permalinks, and it is the only handle that fits in the
//!   frozen `TorrentResult`.

use scraper::{ElementRef, Html, Selector};

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet};
use crate::core::types::{
    MagnetFuture, SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// Hosts, in preference order.
///
/// One canonical domain: FitGirl's mirrors are announced in blog posts and churn,
/// so extra hosts belong in config where they can be corrected without a release
/// (`docs/sources.md` §3.1). The failover call site takes a list either way.
pub const HOSTS: &[&str] = &["fitgirl-repacks.site"];

const DEF: SourceDef = SourceDef {
    id: SourceId::FitGirl,
    label: "FitGirl",
    groups: &[SourceGroup::Games],
    homepage: "https://fitgirl-repacks.site",
    // A WordPress feed carries no swarm data; see the module docs.
    reports_health: false,
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
/// It namespaces the locator so a FitGirl post id and some other site's torrent
/// id that happen to share a number can never collide on `info_hash`, which the
/// engine uses as a cross-source dedupe key (`FR-56`).
const LOCATOR_TAG: &str = "f1761a11";

// ---------------------------------------------------------------------------
// Post locator
// ---------------------------------------------------------------------------

/// Packs a WordPress post id into a syntactically valid 40-hex `info_hash`.
///
/// The real infohash is inside the post body, and this row is not going to fetch
/// that page (see the module docs) — but every downstream consumer still needs
/// *an* id: the UI keys rows by it, the queue dedupes by it, and `core::paths`
/// refuses to build a cache path from anything that is not 40 hex. So the id we
/// can see — the `id="post-46381"` WordPress prints on every article — is padded
/// into that shape behind [`LOCATOR_TAG`], and [`locator_post_id`] reads it back
/// out when the user finally asks for the magnet.
///
/// The numeric id rather than the permalink is deliberate: WordPress re-slugs a
/// permalink when the title is edited and appends `-2` when two posts collide, so
/// the slug is the one part of the URL that is not stable. `?p=<id>` always
/// resolves.
///
/// TODO(engine): needs a detail-URL field on `TorrentResult`. This encoding is a
/// workaround for a frozen type, and it leaves one real rough edge: until
/// `resolve_magnet` runs, `info_hash` is a *locator*, not the torrent's actual
/// infohash. The engine must therefore re-key a queue item on the hash carried by
/// the resolved magnet instead of trusting the id it enqueued with, or a lazily
/// resolved download ends up filed under the wrong key.
fn locator_id(post_id: u64) -> String {
    format!("{LOCATOR_TAG}{post_id:032x}")
}

/// Reads a post id back out of a locator minted by [`locator_id`].
///
/// `None` for a real infohash. A genuine hash could in principle start with the
/// tag (one in 2^32), but that only matters for a row whose magnet is already
/// known — and such a row never reaches this function, because
/// [`Source::resolve_magnet`] returns the magnet it already has first.
fn locator_post_id(info_hash: &str) -> Option<u64> {
    let packed = info_hash.strip_prefix(LOCATOR_TAG)?;
    let digits = packed.trim_start_matches('0');
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// The post id WordPress prints on every article (`id="post-46381"`).
fn post_id_from_attr(raw: &str) -> Option<u64> {
    // Zero is rejected as well as unparseable text: `locator_id(0)` would encode
    // to all zeros, which `locator_post_id` cannot tell from padding.
    raw.trim()
        .strip_prefix("post-")?
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
}

/// Path of the post behind one row — WordPress' canonical shortlink.
fn detail_path(post_id: u64) -> String {
    format!("/?p={post_id}")
}

/// The search or browse path for one query.
fn list_path(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        // The front page is the newest repacks in the same markup the search
        // results use, so browsing costs no second parser.
        return "/".to_string();
    }
    format!("/?s={}", encode_query(q))
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

/// Parses a human-readable size ("68.9 GB", "700 MiB") into bytes.
///
/// 1024-based even for the SI spellings, because the repack sizes are quoted the
/// way an installer reports them — a 1024 divisor labelled "GB". Text with no
/// number in it is 0, meaning *unknown*; the row is still worth showing.
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

/// The download size quoted in a post body.
///
/// FitGirl prints both an "Original Size" and a "Repack Size"; the repack size is
/// the one the user actually downloads, so a post that quotes only the original
/// reads as unknown rather than promising a number that is three times too big.
/// The quote is often a range ("from 34.2 GB [Selective Download]"), and taking
/// its first number is right — that is the floor, and it is what the site means.
fn repack_size(content: &str) -> u64 {
    const LABEL: &str = "repack size";
    let lowered = content.to_ascii_lowercase();
    let Some(at) = lowered.find(LABEL) else {
        return 0;
    };
    // Byte offsets are shared with the original because `to_ascii_lowercase`
    // changes no ASCII byte's width and leaves every other byte alone.
    let tail = &content[at + LABEL.len()..];
    // Bounded window: a post that names the label but quotes no number must read
    // as unknown, not pick up whatever digits appear three paragraphs later.
    parse_size(tail.get(..64).unwrap_or(tail))
}

/// Converts a WordPress `<time datetime="…">` value to unix seconds.
///
/// The stamp is a fixed-shape ISO 8601 string, so a dozen lines of civil-date
/// arithmetic beats a `chrono` dependency for one integer — the same trade
/// `TorrentResult::added` itself was frozen on. Anything that does not match the
/// shape is `None`: a guessed timestamp is worse than an admitted gap, because it
/// sorts wrongly and silently.
fn parse_iso8601(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse().ok() };
    let year = field(0..4)?;
    let month = field(5..7)?;
    let day = field(8..10)?;
    let hour = field(11..13)?;
    let minute = field(14..16)?;
    let second = field(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let utc = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(utc - utc_offset(s.get(19..).unwrap_or_default())?)
}

/// Days from the unix epoch to a proleptic-Gregorian date.
///
/// Hinnant's `days_from_civil`, which is the standard closed form for this and
/// avoids both a leap-year table and a dependency.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // March-based year: February's leap day becomes the last day of the year,
    // which is what removes every special case from the arithmetic below.
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let year_of_era = y - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Seconds to subtract for the trailing timezone of an ISO 8601 stamp.
fn utc_offset(raw: &str) -> Option<i64> {
    // Fractional seconds, when a theme emits them, sit between the seconds and
    // the offset and carry no information we keep.
    let rest = match raw.strip_prefix('.') {
        Some(fraction) => fraction.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => raw,
    }
    .trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let mut chars = rest.chars();
    let sign = match chars.next() {
        Some('+') => 1,
        Some('-') => -1,
        _ => return None,
    };
    let digits: String = chars.filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3_600 + minutes * 60))
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

/// Turns one WordPress index or search page into rows.
///
/// Free of I/O so it can be tested against a committed fixture — a scraper tested
/// only against the live site is a scraper that breaks silently (`FR-22`).
pub fn parse_list(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let doc = Html::parse_document(body);
    let article_sel = sel("article")?;
    let title_sel = sel(".entry-title a, .entry-title")?;
    let content_sel = sel(".entry-content")?;
    let time_sel = sel("time[datetime]")?;

    let mut articles = 0usize;
    let mut out = Vec::new();
    for article in doc.select(&article_sel) {
        articles += 1;
        let Some(title) = article.select(&title_sel).next() else {
            continue;
        };
        let name = text_of(title);
        if name.is_empty() {
            continue;
        }
        // A pinned housekeeping post is rendered without a post id and has no
        // repack behind it; without an id we could never reach its magnet, so it
        // is dropped rather than shown as an undownloadable row (`FR-14`).
        let Some(post_id) = article.attr("id").and_then(post_id_from_attr) else {
            continue;
        };

        out.push(TorrentResult {
            info_hash: locator_id(post_id),
            name,
            size_bytes: article
                .select(&content_sel)
                .next()
                .map_or(0, |c| repack_size(&text_of(c))),
            // Not zero-as-in-dead: this source reports no health at all, and
            // `SourceDef::reports_health` is what tells the UI to render a
            // neutral dot instead of a red one.
            seeders: 0,
            leechers: 0,
            num_files: None,
            source: SourceId::FitGirl,
            added: article
                .select(&time_sel)
                .next()
                .and_then(|t| t.attr("datetime"))
                .and_then(parse_iso8601),
            magnet: None,
        });
    }

    // Zero rows is a legitimate answer — WordPress search only matches site text,
    // so a real repack whose title does not contain the query genuinely returns
    // nothing. It is also what a blocked scrape looks like, and only the absence
    // of any article at all, together with a challenge marker, separates them.
    if out.is_empty()
        && articles == 0
        && let Some(marker) = challenge_marker(body)
    {
        return Err(SourceError::Blocked(format!(
            "FitGirl served an anti-bot challenge ({marker})"
        )));
    }
    Ok(out)
}

/// Extracts the magnet from a repack post and rebuilds it canonically.
///
/// The first magnet in the post is the repack itself; the ones below it are
/// mirrors and update packs for the same release. Taking the first is deliberate
/// — and it is the reason this returns a [`build_magnet`] string rather than the
/// site's own: every magnet in the app is then byte-identical for the same
/// torrent, which is what the cache and the queue compare.
pub fn parse_magnet(body: &str, name: &str) -> Result<String, SourceError> {
    let doc = Html::parse_document(body);
    let magnet_sel = sel(r#"a[href^="magnet:"]"#)?;
    let hash = doc
        .select(&magnet_sel)
        .find_map(|a| a.attr("href").and_then(info_hash_from_magnet))
        // Last resort: the raw document. FitGirl posts sometimes print the magnet
        // as plain text next to the link, where no selector can reach it.
        .or_else(|| info_hash_from_magnet(body));

    let Some(hash) = hash else {
        // A post that answers with a challenge must not read as "this repack has
        // no magnet" — that would look like a broken result to the user instead
        // of a blocked source.
        if let Some(marker) = challenge_marker(body) {
            return Err(SourceError::Blocked(format!(
                "FitGirl served an anti-bot challenge ({marker})"
            )));
        }
        return Err(SourceError::Parse(
            "no magnet in the FitGirl post body".into(),
        ));
    };
    Ok(build_magnet(&hash, name))
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// FitGirl Repacks — the Games group's only source.
pub struct FitGirl {
    client: SourceClient,
}

impl FitGirl {
    /// Builds the source with its own connection pool.
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for FitGirl {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for FitGirl {
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
            // A row that already carries a magnet costs nothing — pressing `d`
            // twice on the same result must not fetch the post twice.
            if let Some(magnet) = &result.magnet {
                return Ok(magnet.clone());
            }
            let Some(post_id) = locator_post_id(&result.info_hash) else {
                return Err(SourceError::Parse(
                    "this FitGirl row carries no post locator".into(),
                ));
            };
            let (body, _host) = self
                .client
                .get_text_failover(HOSTS, &detail_path(post_id), ctx)
                .await?;
            parse_magnet(&body, &result.name)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::magnet::normalize_info_hash;

    const LIST: &str = include_str!("fixtures/fitgirl.html");

    /// A repack post, trimmed to the parts this module reads. The real thing is
    /// megabytes of screenshots and mirror lists — which is exactly why the
    /// magnet is fetched lazily and not once per search result.
    const POST: &str = r#"<!DOCTYPE html><html><body>
        <article id="post-46381" class="post-46381 post">
        <h1 class="entry-title">Cyberpunk 2077: Ultimate Edition</h1>
        <div class="entry-content">
          <p><strong>Repack Size: 68.9 GB</strong></p>
          <h3>Download Mirrors</h3>
          <ul>
            <li>1337x [<a href="magnet:?xt=urn:btih:C4A9E7B1D6F80352AB19CD7E4406F5238B0D91EA&amp;dn=Cyberpunk.2077.Ultimate.Edition&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337">magnet</a>]</li>
            <li>RuTracker [<a href="https://rutracker.org/forum/viewtopic.php?t=1">torrent</a>]</li>
          </ul>
        </div></article></body></html>"#;

    /// The real bytes a blocked scrape gets back: a full page with no article on
    /// it at all.
    const CHALLENGE: &str = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
        <body class="no-js"><div class="cf-browser-verification cf-im-under-attack">
        <h1>Checking your browser before accessing fitgirl-repacks.site</h1></div>
        <script>window._cf_chl_opt={cvId:"3"};</script></body></html>"#;

    /// WordPress' own "nothing matched" page: the layout is there, the loop
    /// produced no articles.
    const NO_RESULTS: &str = r#"<!DOCTYPE html><html><body><div id="primary">
        <main id="main" class="site-main"><header class="page-header">
        <h1 class="page-title">Nothing Found</h1></header>
        <p>Sorry, but nothing matched your search terms.</p>
        </main></div></body></html>"#;

    #[test]
    fn parses_the_fields_a_wordpress_post_actually_carries() {
        let rows = parse_list(LIST).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(first.name, "Cyberpunk 2077: Ultimate Edition");
        assert_eq!(first.size_bytes, 73_980_811_674, "68.9 GB, 1024-based");
        assert_eq!(first.source, SourceId::FitGirl);
        assert_eq!(
            first.added,
            Some(1_773_220_961),
            "2026-03-11T09:22:41+00:00"
        );
        assert_eq!(first.num_files, None);
    }

    #[test]
    fn a_size_range_is_read_as_its_floor() {
        // The second post quotes "Repack Size: from 34.2 GB [Selective Download]".
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows[1].size_bytes, 36_721_970_381, "34.2 GB");
        assert_eq!(rows[1].added, Some(1_770_041_100));
    }

    #[test]
    fn the_original_size_is_never_mistaken_for_the_repack_size() {
        // Both posts also quote a much larger "Original Size"; reading that one
        // would tell the user a 68.9 GB download is 122 GB.
        let rows = parse_list(LIST).expect("parses");
        assert!(rows[0].size_bytes < 122 * GIB);
        assert_eq!(
            repack_size("Original Size: 122 GB Repack Size: 68.9 GB"),
            73_980_811_674
        );
        // A post that quotes only an original size reads as unknown, not as a lie.
        assert_eq!(repack_size("Original Size: 122 GB"), 0);
    }

    #[test]
    fn search_rows_defer_the_magnet_instead_of_fetching_every_post() {
        // The whole point of the module: a displayable row never requires the
        // magnet, so a search is one request no matter how many posts it matched.
        let rows = parse_list(LIST).expect("parses");
        assert!(rows.iter().all(|r| r.magnet.is_none()));
        assert!(
            rows.iter()
                .all(|r| normalize_info_hash(&r.info_hash).is_some()),
            "a locator must still be a usable id for the cache and the queue"
        );
    }

    #[test]
    fn reports_no_swarm_data_rather_than_pretending_to() {
        let rows = parse_list(LIST).expect("parses");
        assert!(rows.iter().all(|r| r.seeders == 0 && r.leechers == 0));
        assert!(
            !FitGirl::new().def().reports_health,
            "those zeros mean unknown, and only this flag says so"
        );
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded() {
        // The fixture holds four articles: a pinned post rendered without an id,
        // and a post whose title anchor is empty.
        let rows = parse_list(LIST).expect("parses");
        assert_eq!(rows.len(), 2, "two unusable articles must not be rendered");
        assert!(rows.iter().all(|r| !r.name.is_empty()));
        assert!(rows.iter().all(|r| locator_post_id(&r.info_hash).is_some()));
    }

    #[test]
    fn the_post_is_located_by_id_so_a_reslugged_permalink_cannot_break_it() {
        let rows = parse_list(LIST).expect("parses");
        let id = locator_post_id(&rows[0].info_hash).expect("locator");
        assert_eq!(id, 46_381);
        assert_eq!(detail_path(id), "/?p=46381");
    }

    #[test]
    fn a_locator_round_trips_and_stays_a_valid_infohash() {
        // The tag has to be hex and exactly 8 characters or the locator stops
        // being a 40-hex id, and `core::paths` refuses to build a cache path
        // from it. This is what objects if someone edits LOCATOR_TAG carelessly.
        assert_eq!(LOCATOR_TAG.len(), 8);
        assert!(LOCATOR_TAG.chars().all(|c| c.is_ascii_hexdigit()));

        for id in [1u64, 46_381, u64::MAX] {
            let packed = locator_id(id);
            assert_eq!(normalize_info_hash(&packed).as_deref(), Some(&*packed));
            assert_eq!(locator_post_id(&packed), Some(id));
        }
        assert_eq!(
            locator_post_id("c4a9e7b1d6f80352ab19cd7e4406f5238b0d91ea"),
            None,
            "a real infohash must not be mistaken for a locator"
        );
        assert_eq!(
            locator_post_id(&format!("{LOCATOR_TAG}{}", "0".repeat(32))),
            None
        );
    }

    #[test]
    fn resolve_magnet_rebuilds_a_canonical_magnet_from_the_post_body() {
        let magnet = parse_magnet(POST, "Cyberpunk 2077").expect("post parses");
        assert!(
            magnet.starts_with("magnet:?xt=urn:btih:c4a9e7b1d6f80352ab19cd7e4406f5238b0d91ea"),
            "the post's uppercase hash must be lowercased at the boundary: {magnet}"
        );
        assert!(
            !magnet.contains("&tr="),
            "the site's trackers are dropped; the engine supplies its own"
        );
    }

    #[test]
    fn a_post_with_no_magnet_at_all_is_a_parse_error() {
        assert!(matches!(
            parse_magnet("<html><body>coming soon</body></html>", "x"),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn a_challenge_page_is_blocked_not_an_empty_search() {
        // An empty list here would render as "FitGirl found nothing", quietly
        // hiding a blocked source behind a plausible answer — and FitGirl sits
        // behind Cloudflare, so this is the common failure, not a rare one.
        let err = parse_list(CHALLENGE).expect_err("a challenge must not parse as success");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
        assert!(err.is_hard_host_failure(), "a block parks the host");

        let err = parse_magnet(CHALLENGE, "x").expect_err("post challenge");
        assert!(matches!(err, SourceError::Blocked(_)), "got {err:?}");
    }

    #[test]
    fn wordpress_finding_nothing_is_a_successful_empty_search() {
        assert_eq!(parse_list(NO_RESULTS).expect("parses").len(), 0);
    }

    #[test]
    fn malformed_markup_never_panics_and_never_invents_rows() {
        for junk in [
            "",
            "<html",
            "<article id=\"post-\"><h1 class=\"entry-title\">",
            "not html at all",
            "\u{0}\u{1}\u{2}",
        ] {
            assert_eq!(parse_list(junk).expect("no error on junk").len(), 0);
        }
    }

    #[test]
    fn human_sizes_parse_to_bytes() {
        assert_eq!(parse_size("68.9 GB"), 73_980_811_674);
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
    fn iso_timestamps_convert_without_a_calendar_dependency() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601("2000-03-01T00:00:00+00:00"),
            Some(951_868_800)
        );
        assert_eq!(
            parse_iso8601("2024-02-29T00:00:00Z"),
            Some(1_709_164_800),
            "a leap day is a real date"
        );
        assert_eq!(
            parse_iso8601("2026-08-11T18:30:00+05:30"),
            Some(1_786_453_200),
            "a non-UTC offset is subtracted, not ignored"
        );
        // A shape we do not recognise is admitted, never guessed.
        assert_eq!(parse_iso8601("March 11, 2026"), None);
        assert_eq!(parse_iso8601(""), None);
        assert_eq!(parse_iso8601("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_query_is_url_encoded_and_an_empty_one_browses() {
        assert_eq!(list_path("cyberpunk 2077"), "/?s=cyberpunk+2077");
        assert_eq!(list_path("a&b=c"), "/?s=a%26b%3Dc");
        assert_eq!(
            list_path("   "),
            "/",
            "an empty query is the newest repacks, not a search for nothing"
        );
        assert!(list_path("進撃").is_ascii());
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let s = FitGirl::new();
        assert_eq!(s.def().id, SourceId::FitGirl);
        assert_eq!(s.def().groups, &[SourceGroup::Games]);
        assert!(!s.def().reports_health);
    }
}
