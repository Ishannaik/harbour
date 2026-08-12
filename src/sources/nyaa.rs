//! Nyaa (Anime, RSS) — a real server-side search, delivered as RSS.
//!
//! Unlike the other two feed sources, nyaa takes the query as a URL parameter,
//! so there is no local filtering here: whatever the feed returns is the result
//! set (`docs/sources.md` §3.6). The category is pinned to `1_0` and must stay
//! pinned — without it live-action and manga torrents leak into the anime group.
//!
//! nyaa.si is the only host. Mirror instances are unsanctioned and would have to
//! be verified by hand before being trusted with a magnet, so a dead nyaa.si
//! reports offline rather than chasing lookalikes.
//!
//! Its RSS extension namespace carries `nyaa:seeders`, `nyaa:leechers` and
//! `nyaa:infoHash`, which is why `reports_health` is true — these are the site's
//! own swarm numbers, not a guess.

use oxixml_xml::Reader;
use oxixml_xml::escape::resolve_predefined_entity;
use oxixml_xml::events::Event;
use oxixml_xml::name::QName;

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet, normalize_info_hash};
use crate::core::types::{
    SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// The canonical host, and the only one. See the module docs for why there is
/// no mirror list.
pub const HOSTS: &[&str] = &["nyaa.si"];

/// The anime category. Dropping it mixes live action and manga into the anime
/// group, so it is a constant rather than a parameter.
const CATEGORY: &str = "1_0";

const DEF: SourceDef = SourceDef {
    id: SourceId::Nyaa,
    label: "Nyaa",
    groups: &[SourceGroup::Anime],
    homepage: "https://nyaa.si",
    reports_health: true,
};

/// The Nyaa adapter.
pub struct Nyaa {
    client: SourceClient,
}

impl Nyaa {
    /// Builds the adapter with its own connection pool (see [`SourceClient`]).
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for Nyaa {
    fn default() -> Self {
        Self::new()
    }
}

/// One `<item>` while it is being read.
///
/// Every field is optional: nyaa omits `nyaa:size` on some uploads and the
/// extension elements disappear entirely on a cached/proxied copy of the feed.
/// The row is judged once, in [`Item::finish`].
#[derive(Default)]
struct Item {
    title: String,
    /// From `<nyaa:infoHash>`, which is the site's own canonical id.
    info_hash: Option<String>,
    /// Texts that might hold a magnet, newline-joined so a single scan finds the
    /// hash and cannot splice one across two fields.
    magnet_text: String,
    size_bytes: u64,
    seeders: u32,
    leechers: u32,
    added: Option<i64>,
}

impl Item {
    /// Stores one direct child of `<item>`, keyed by its lowercased local name.
    fn set(&mut self, field: &str, value: &str) {
        match field {
            "title" => self.title = value.to_string(),
            // A blank or malformed hash must not erase a good one, so this
            // assigns only on success.
            "infohash" => {
                if let Some(hash) = normalize_info_hash(value) {
                    self.info_hash = Some(hash);
                }
            }
            // `link` is the `.torrent` URL, not a magnet — but uploaders often
            // paste one into the description, and that is the only fallback
            // when the extension namespace is missing.
            "description" | "link" => {
                self.magnet_text.push('\n');
                self.magnet_text.push_str(value);
            }
            "size" => self.size_bytes = parse_size(value),
            "seeders" => self.seeders = value.parse().unwrap_or(0),
            "leechers" => self.leechers = value.parse().unwrap_or(0),
            "pubdate" => self.added = pub_date_to_unix(value),
            _ => {}
        }
    }

    /// Turns the accumulated item into a row, or drops it.
    fn finish(self) -> Option<TorrentResult> {
        let name = self.title.trim().to_string();
        // `FR-14`: a row with no name or no usable infohash could never be shown
        // or handed to the engine, so it is dropped rather than rendered.
        if name.is_empty() {
            return None;
        }
        let info_hash = self
            .info_hash
            .or_else(|| info_hash_from_magnet(&self.magnet_text))?;
        Some(TorrentResult {
            // Rebuilt rather than reused: a magnet scraped out of a description
            // carries trackers and a `dn` we did not choose, and the cache
            // compares these strings.
            magnet: Some(build_magnet(&info_hash, &name)),
            info_hash,
            name,
            size_bytes: self.size_bytes,
            seeders: self.seeders,
            leechers: self.leechers,
            num_files: None,
            source: SourceId::Nyaa,
            added: self.added,
        })
    }
}

/// Ends the open `<item>`, keeping the row when it gathered the fields a row needs.
fn push_item_row(out: &mut Vec<TorrentResult>, item: &mut Item) {
    if let Some(row) = std::mem::take(item).finish() {
        out.push(row);
    }
}

/// Turns one feed body into rows.
///
/// Free of I/O so it can be tested against a committed fixture — the pattern
/// every source follows, because a parser tested only against the live site is
/// a parser that breaks silently (`FR-22`).
///
/// A query that matched nothing yields a feed with no `<item>`s, and that is
/// `Ok(vec![])`, never an error: nyaa answered, it simply had nothing.
pub fn parse(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let mut reader = Reader::from_str(body);
    // Release titles are full of punctuation and bare `&` does turn up.
    // Rejecting the whole document over one would lose every good row.
    reader.config_mut().allow_dangling_amp = true;

    let mut out = Vec::new();
    let mut depth = 0usize;
    // `Some(d)` while inside an `<item>` that opened at depth `d`. Tracking the
    // depth rather than a boolean is what keeps a nested tag inside a
    // description from being mistaken for a field of its own.
    let mut item_depth: Option<usize> = None;
    let mut item = Item::default();
    let mut field: Option<String> = None;
    let mut text = String::new();

    loop {
        match reader
            .read_event()
            .map_err(|e| SourceError::Parse(e.to_string()))?
        {
            Event::Start(tag) => {
                depth += 1;
                let name = local_name(tag.name());
                if item_depth.is_none() && name == "item" {
                    item_depth = Some(depth);
                    item = Item::default();
                } else if item_depth == Some(depth - 1) {
                    field = Some(name);
                    text.clear();
                }
            }
            Event::End(tag) => {
                let name = local_name(tag.name());
                if item_depth == Some(depth) && name == "item" {
                    item_depth = None;
                    push_item_row(&mut out, &mut item);
                } else if item_depth == Some(depth - 1)
                    && let Some(open) = field.take()
                {
                    item.set(&open, text.trim());
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(chunk) => {
                if field.is_some() {
                    text.push_str(
                        &chunk
                            .xml10_content()
                            .map_err(|e| SourceError::Parse(e.to_string()))?,
                    );
                }
            }
            Event::CData(chunk) => {
                if field.is_some() {
                    text.push_str(
                        &chunk
                            .xml10_content()
                            .map_err(|e| SourceError::Parse(e.to_string()))?,
                    );
                }
            }
            Event::GeneralRef(reference) => {
                // quick-xml reports `&amp;` as its own event instead of folding
                // it into the surrounding text, so a magnet written
                // `…&amp;dn=…` has to be reassembled here — without this the
                // hash runs into `dn` and the row is silently dropped.
                if field.is_none() {
                    continue;
                }
                let entity = reference
                    .decode()
                    .map_err(|e| SourceError::Parse(e.to_string()))?;
                if let Some(resolved) = resolve_predefined_entity(&entity) {
                    text.push_str(resolved);
                } else if let Some(ch) = numeric_char_ref(&entity) {
                    text.push(ch);
                }
                // An entity the feed never declared is dropped: a stray
                // `&nbsp;` is not worth losing the release over.
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// The feed path for a query, category pinned.
///
/// Public so the URL shape is testable without a network: getting `c=1_0` wrong
/// is silent — the search still works, it just quietly stops being an anime
/// source.
pub fn feed_path(query: &str) -> String {
    let query = query.trim();
    if query.is_empty() {
        // The curated browse: the category's newest uploads, which is what the
        // feed returns with no `q`.
        format!("/?page=rss&c={CATEGORY}")
    } else {
        format!("/?page=rss&q={}&c={CATEGORY}", urlencode(query))
    }
}

/// Percent-encodes a query for a URL parameter.
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

/// Lowercased local name of a tag, with any namespace prefix stripped.
///
/// Matching the local name rather than the literal `nyaa:seeders` keeps parsing
/// working through a proxy or cache that rewrites the namespace prefix.
fn local_name(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.local_name().as_ref()).to_ascii_lowercase()
}

/// Resolves a numeric character reference (`&#38;`, `&#x26;`).
fn numeric_char_ref(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

/// Parses `<nyaa:size>`, which the site writes in human units ("1.4 GiB").
///
/// A bare number is accepted as bytes so a schema change to a raw count does not
/// silently zero the column, and anything unreadable becomes 0 — which the UI
/// renders as unknown rather than as a confidently wrong number.
///
/// `MB` is treated as `MiB` deliberately: trackers write decimal prefixes and
/// mean binary ones, and honouring the SI meaning would under-report every size.
fn parse_size(raw: &str) -> u64 {
    let text = raw.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().unwrap_or(0.0);
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        // "N/A", "unknown", anything else: say nothing rather than guess.
        _ => return 0,
    };
    (value * multiplier) as u64
}

/// Months as RFC 2822 spells them, indexed from January.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Converts an RSS `pubDate` ("Sat, 08 Aug 2026 14:30:00 -0000") to unix seconds.
///
/// Hand-rolled rather than taking a date-time dependency: `TorrentResult::added`
/// is an integer precisely so no calendar type has to exist downstream
/// (`core::types`). Anything unreadable becomes `None` — an unknown publication
/// date is not a reason to drop an otherwise usable release.
fn pub_date_to_unix(raw: &str) -> Option<i64> {
    let rest = raw.trim();
    // The day-of-week prefix is optional in RFC 2822 and carries no information.
    let rest = rest.split_once(',').map_or(rest, |(_, tail)| tail);
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(month_name))? as i64
        + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    // RFC 2822 allows the seconds to be omitted; feeds rarely do, but a missing
    // one should cost precision, not the whole date.
    let second: i64 = match hms.next() {
        Some(value) => value.parse().ok()?,
        None => 0,
    };
    let offset = parts.next().and_then(zone_offset_seconds).unwrap_or(0);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// `+0530` / `-0000` → seconds east of UTC.
///
/// Nyaa writes `-0000`, which RFC 2822 defines as UTC with an unknown local
/// zone; the sign handling falls out of the same arithmetic.
fn zone_offset_seconds(zone: &str) -> Option<i64> {
    let (sign, digits) = match zone.as_bytes().first()? {
        b'+' => (1, &zone[1..]),
        b'-' => (-1, &zone[1..]),
        _ => return Some(0),
    };
    if digits.len() != 4 {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Days since 1970-01-01 for a proleptic-Gregorian date.
///
/// Howard Hinnant's `days_from_civil`: ten lines of arithmetic instead of a
/// calendar crate, which is the whole reason `added` is a unix integer.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

impl Source for Nyaa {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            let path = feed_path(query);
            // Failover over a one-host list rather than a bare GET: it keeps the
            // sticky-hint plumbing identical to every other source, and it is
            // where a verified mirror would be added.
            let (body, _host) = self.client.get_text_failover(HOSTS, &path, ctx).await?;
            parse(&body)
        })
    }

    // `resolve_magnet` is not implemented: `parse` always builds a magnet, so
    // the trait's default (hand back the one we already have) is correct.
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/nyaa.xml");

    #[test]
    fn parses_every_field_the_nyaa_namespace_provides() {
        let rows = parse(FIXTURE).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(
            first.name,
            "[HarbourSubs] Example Anime - 01 (1080p) [1A2B3C4D].mkv"
        );
        assert_eq!(first.info_hash, "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678");
        assert_eq!(first.size_bytes, 1_503_238_553, "1.4 GiB");
        assert_eq!(first.seeders, 1204);
        assert_eq!(first.leechers, 85);
        assert_eq!(first.num_files, None, "the feed never states a file count");
        assert_eq!(first.source, SourceId::Nyaa);
        assert_eq!(first.added, Some(1_786_199_400));
        assert_eq!(
            first.magnet.as_deref(),
            Some(build_magnet(&first.info_hash, &first.name).as_str()),
            "the magnet is rebuilt, not copied out of the feed"
        );
    }

    #[test]
    fn a_non_ascii_title_and_an_uppercase_hash_are_both_handled() {
        let rows = parse(FIXTURE).expect("parses");
        let second = &rows[1];
        assert!(second.name.contains('進'), "the title must not be mangled");
        assert!(second.name.contains(" & "), "an entity must be restored");
        assert_eq!(
            second.info_hash, "b1b2b3b4b5b6b7b8b9babbbcbdbebfb0b1b2b3b4",
            "an uppercase hash must be canonicalized at the boundary"
        );
        let magnet = second.magnet.as_deref().unwrap_or_default();
        assert!(magnet.is_ascii(), "the display name must be encoded");
        assert_eq!(second.size_bytes, 734_003_200, "700.0 MiB");
    }

    #[test]
    fn the_hash_falls_back_to_a_magnet_in_the_description() {
        // Third item has no nyaa:infoHash; the only hash is inside the CDATA.
        let rows = parse(FIXTURE).expect("parses");
        let batch = &rows[2];
        assert_eq!(batch.info_hash, "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00");
        assert_eq!(batch.size_bytes, 13_207_024_435, "12.3 GiB");
        assert_eq!(batch.seeders, 340);
        assert_eq!(batch.leechers, 52);
    }

    #[test]
    fn a_missing_size_or_leecher_count_degrades_instead_of_dropping_the_row() {
        let rows = parse(FIXTURE).expect("parses");
        let sizeless = rows.last().expect("a last row");
        assert_eq!(sizeless.name, "[HarbourSubs] Sizeless Release - 03 (480p)");
        assert_eq!(sizeless.size_bytes, 0, "unknown, and the UI renders it so");
        assert_eq!(sizeless.seeders, 6);
        assert_eq!(sizeless.leechers, 0);
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded_or_named() {
        let rows = parse(FIXTURE).expect("parses");
        assert_eq!(rows.len(), 4, "six items, two of them unusable");
        assert!(
            rows.iter().all(|r| r.info_hash.len() == 40),
            "a junk infohash must be dropped, not rendered"
        );
        assert!(
            rows.iter().all(|r| !r.name.trim().is_empty()),
            "an unnameable row must be dropped (FR-14)"
        );
        assert!(
            !rows.iter().any(|r| r.name.contains("Corrupt Entry")),
            "the `not-a-real-info-hash` item must not appear"
        );
        assert!(rows.iter().all(|r| r.magnet.is_some()));
        assert!(
            rows.iter()
                .all(|r| r.info_hash == r.info_hash.to_lowercase()),
            "infohashes are the cross-source join key and must be canonical"
        );
    }

    #[test]
    fn an_empty_feed_is_not_an_error() {
        // A query that matched nothing is a successful empty search and must
        // never mark the source offline.
        let empty = concat!(
            "<?xml version=\"1.0\"?><rss version=\"2.0\">",
            "<channel><title>Nyaa</title></channel></rss>"
        );
        assert_eq!(parse(empty).expect("empty feed parses").len(), 0);
        assert_eq!(parse("").expect("an empty body parses").len(), 0);
    }

    #[test]
    fn malformed_xml_is_a_parse_error() {
        let broken = "<rss><channel><item><title>x</wrong></item></channel></rss>";
        assert!(matches!(parse(broken), Err(SourceError::Parse(_))));
    }

    #[test]
    fn the_query_goes_to_the_server_with_the_category_pinned() {
        assert_eq!(feed_path(""), "/?page=rss&c=1_0");
        assert_eq!(feed_path("   "), "/?page=rss&c=1_0", "blank is a browse");
        assert_eq!(
            feed_path("one piece"),
            "/?page=rss&q=one+piece&c=1_0",
            "the query is encoded and the category survives it"
        );
        assert_eq!(feed_path("a&b=c"), "/?page=rss&q=a%26b%3Dc&c=1_0");
        assert!(
            feed_path("進撃").is_ascii(),
            "a non-ASCII query must be percent-encoded"
        );
        for path in [feed_path(""), feed_path("x")] {
            assert!(path.contains("c=1_0"), "dropping the category leaks manga");
        }
    }

    #[test]
    fn sizes_parse_from_the_human_units_the_site_writes() {
        assert_eq!(parse_size("1.4 GiB"), 1_503_238_553);
        assert_eq!(parse_size("700.0 MiB"), 734_003_200);
        assert_eq!(parse_size("12.3 GiB"), 13_207_024_435);
        assert_eq!(parse_size("938 KiB"), 960_512);
        assert_eq!(parse_size("2.5 TiB"), 2_748_779_069_440);
        assert_eq!(parse_size("1503238553"), 1_503_238_553, "bytes still parse");
        // Unknown is 0, never a guess: the UI renders 0 as "—".
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size("0 Bytes"), 0);
    }

    #[test]
    fn pub_dates_convert_to_unix_seconds_including_offsets() {
        assert_eq!(
            pub_date_to_unix("Sat, 08 Aug 2026 14:30:00 -0000"),
            Some(1_786_199_400),
            "nyaa's `-0000` is UTC"
        );
        assert_eq!(
            pub_date_to_unix("Sat, 08 Aug 2026 23:30:00 +0900"),
            Some(1_786_199_400)
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0, "the epoch is day zero");
        // Garbage is unknown, not epoch — a wrong date sorts worse than none.
        assert_eq!(pub_date_to_unix(""), None);
        assert_eq!(pub_date_to_unix("sometime last week"), None);
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let source = Nyaa::new();
        assert_eq!(source.def().id, SourceId::Nyaa);
        assert_eq!(source.def().groups, &[SourceGroup::Anime]);
        assert!(
            source.def().reports_health,
            "the nyaa namespace carries real seed counts"
        );
        assert_eq!(HOSTS, &["nyaa.si"], "mirrors are unverified by policy");
    }
}
