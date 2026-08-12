//! SubsPlease (Anime, RSS) — a small release feed, filtered locally.
//!
//! SubsPlease publishes the last few dozen releases and nothing else: no search
//! endpoint, no size, no swarm counts (`docs/sources.md` §3.7). Two consequences
//! shape this adapter, and both are honest limits rather than bugs:
//!
//! * The query is a local title filter, so anything older than the feed's window
//!   returns nothing even though the release exists.
//! * `reports_health` is **false**. The feed states no seeders, so `seeders: 0`
//!   here means *unknown*, not *dead* — an alive-only filter must never drop
//!   these rows and the sidebar shows a neutral dot. `size_bytes` is 0 for the
//!   same reason, which the UI renders as "—".
//!
//! The magnet is the item `<link>`, which makes the infohash the one thing this
//! feed always supplies.

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::name::QName;

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet};
use crate::core::types::{
    SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// The only host. SubsPlease is a small single-domain site with no mirrors.
pub const HOSTS: &[&str] = &["subsplease.org"];

/// The whole feed. There is no query parameter, which is why
/// [`filter_by_title`] exists.
const FEED_PATH: &str = "/rss/";

const DEF: SourceDef = SourceDef {
    id: SourceId::SubsPlease,
    label: "SubsPlease",
    groups: &[SourceGroup::Anime],
    homepage: "https://subsplease.org",
    // False on purpose — see the module docs. This is the flag that stops a
    // seeder-sorted or alive-only view from silently deleting this source.
    reports_health: false,
};

/// The SubsPlease adapter.
pub struct SubsPlease {
    client: SourceClient,
}

impl SubsPlease {
    /// Builds the adapter with its own connection pool (see [`SourceClient`]).
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for SubsPlease {
    fn default() -> Self {
        Self::new()
    }
}

/// One `<item>` while it is being read.
///
/// There are no size or swarm fields to collect: the feed carries a title, a
/// magnet and a date, and that is the entire contract.
#[derive(Default)]
struct Item {
    title: String,
    /// Texts that might hold a magnet, newline-joined so a single scan finds the
    /// hash and cannot splice one across two fields.
    magnet_text: String,
    added: Option<i64>,
}

impl Item {
    /// Stores one direct child of `<item>`, keyed by its lowercased local name.
    fn set(&mut self, field: &str, value: &str) {
        match field {
            "title" => self.title = value.to_string(),
            // `link` is the magnet itself; `description` is checked as well
            // because the feed has carried the magnet there in the past and
            // scanning one extra string is free.
            "link" | "description" => {
                self.magnet_text.push('\n');
                self.magnet_text.push_str(value);
            }
            "pubdate" => self.added = pub_date_to_unix(value),
            _ => {}
        }
    }

    /// Turns the accumulated item into a row, or drops it.
    fn finish(self) -> Option<TorrentResult> {
        let name = self.title.trim().to_string();
        // `FR-14`: a row with no name or no usable infohash could never be shown
        // or handed to the engine, so it is dropped rather than rendered. That
        // covers the site's occasional non-torrent announcement post, whose
        // `link` is an ordinary page URL.
        if name.is_empty() {
            return None;
        }
        let info_hash = info_hash_from_magnet(&self.magnet_text)?;
        Some(TorrentResult {
            // Rebuilt rather than reused: the feed's magnet carries trackers and
            // a `dn` we did not choose, and the cache compares these strings.
            magnet: Some(build_magnet(&info_hash, &name)),
            info_hash,
            name,
            // Both unknown, not zero — see the module docs and `reports_health`.
            size_bytes: 0,
            seeders: 0,
            leechers: 0,
            num_files: None,
            source: SourceId::SubsPlease,
            added: self.added,
        })
    }
}

/// Turns one feed body into rows.
///
/// Free of I/O so it can be tested against a committed fixture — the pattern
/// every source follows, because a parser tested only against the live site is
/// a parser that breaks silently (`FR-22`).
///
/// A feed with no `<item>`s is `Ok(vec![])`, never an error: a reachable site
/// with nothing new must not be reported offline.
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
                    if let Some(row) = std::mem::take(&mut item).finish() {
                        out.push(row);
                    }
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
                // Load-bearing, not cosmetic: the magnet lives in `<link>` and
                // is written `…&amp;dn=…`. quick-xml reports the entity as its
                // own event, so without reassembling it here the hash runs
                // straight into `dn` and every row is silently dropped.
                if field.is_some() {
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
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Keeps the rows whose name contains `query`, case-insensitively.
///
/// Separate from [`parse`] so both halves stay testable: the parser sees only
/// bytes, and the filter sees only rows. An empty query keeps everything, which
/// is the curated browse the search view shows before the user types.
pub fn filter_by_title(rows: Vec<TorrentResult>, query: &str) -> Vec<TorrentResult> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|row| row.name.to_lowercase().contains(&needle))
        .collect()
}

/// Lowercased local name of a tag, with any namespace prefix stripped.
///
/// The feed declares an `atom` namespace for its self-link, so prefixes do
/// appear even though none of the fields we read carry one.
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

/// Months as RFC 2822 spells them, indexed from January.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Converts an RSS `pubDate` ("Sat, 08 Aug 2026 16:00:00 +0000") to unix seconds.
///
/// Hand-rolled rather than taking a date-time dependency: `TorrentResult::added`
/// is an integer precisely so no calendar type has to exist downstream
/// (`core::types`). It matters more here than elsewhere — with no seeders and no
/// size, recency is the only thing this source can be sorted by.
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

/// `+0530` / `-0800` → seconds east of UTC.
///
/// Named zones (`GMT`, `UT`, `Z`) are treated as UTC; the obsolete US zone
/// abbreviations are not worth carrying for a feed that always writes an offset.
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

impl Source for SubsPlease {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            // Always the same URL: the site has no query parameter, so the fetch
            // is identical for a browse and for a search.
            let (body, _host) = self.client.get_text_failover(HOSTS, FEED_PATH, ctx).await?;
            Ok(filter_by_title(parse(&body)?, query))
        })
    }

    // `resolve_magnet` is not implemented: `parse` always builds a magnet, so
    // the trait's default (hand back the one we already have) is correct.
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/subsplease.xml");

    #[test]
    fn parses_the_magnet_out_of_the_item_link() {
        let rows = parse(FIXTURE).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(
            first.name,
            "[SubsPlease] Example Anime - 12 (1080p) [ABCD1234].mkv"
        );
        assert_eq!(first.info_hash, "f00dcafef00dcafef00dcafef00dcafef00dcafe");
        assert_eq!(first.source, SourceId::SubsPlease);
        assert_eq!(first.added, Some(1_786_204_800));
        assert_eq!(
            first.magnet.as_deref(),
            Some(build_magnet(&first.info_hash, &first.name).as_str()),
            "the magnet is rebuilt, not copied out of the feed"
        );
        assert!(
            !first.magnet.as_deref().unwrap_or_default().contains("&tr="),
            "the feed's trackers are dropped; the engine adds its own"
        );
    }

    #[test]
    fn a_zero_here_means_unknown_not_dead() {
        // The whole reason reports_health is false: nothing in this feed states
        // a size or a swarm count, so these zeroes must never be read as facts.
        let rows = parse(FIXTURE).expect("parses");
        assert!(rows.iter().all(|r| r.size_bytes == 0));
        assert!(rows.iter().all(|r| r.seeders == 0));
        assert!(rows.iter().all(|r| r.leechers == 0));
        assert!(rows.iter().all(|r| r.num_files.is_none()));
        assert!(
            !SubsPlease::new().def().reports_health,
            "otherwise an alive-only filter would delete this source"
        );
    }

    #[test]
    fn a_non_ascii_title_and_an_uppercase_hash_are_both_handled() {
        let rows = parse(FIXTURE).expect("parses");
        let second = &rows[1];
        assert!(second.name.contains('進'), "the title must not be mangled");
        assert_eq!(
            second.info_hash, "9a9b9c9d9e9f90919293949596979899a0a1a2a3",
            "an uppercase hash must be canonicalized at the boundary"
        );
        assert_eq!(second.added, Some(1_786_116_600));
        assert!(
            second.magnet.as_deref().unwrap_or_default().is_ascii(),
            "the display name must be percent-encoded"
        );
    }

    #[test]
    fn an_escaped_ampersand_in_a_title_survives_intact() {
        let rows = parse(FIXTURE).expect("parses");
        let tom = rows.last().expect("a last row");
        assert_eq!(
            tom.name,
            "[SubsPlease] Tom & Jerry Anime - 04 (480p) [88888888].mkv"
        );
        assert_eq!(tom.added, None, "no pubDate is unknown, not epoch");
        assert!(
            !tom.magnet
                .as_deref()
                .unwrap_or_default()
                .split("&dn=")
                .nth(1)
                .unwrap_or_default()
                .contains('&'),
            "the ampersand must be percent-encoded in the magnet"
        );
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded_or_named() {
        let rows = parse(FIXTURE).expect("parses");
        assert_eq!(rows.len(), 3, "five items, two of them unusable");
        assert!(
            !rows.iter().any(|r| r.name.contains("Announcement Post")),
            "an item whose link is a page rather than a magnet must be dropped"
        );
        assert!(
            rows.iter().all(|r| !r.name.trim().is_empty()),
            "an unnameable row must be dropped (FR-14)"
        );
        assert!(rows.iter().all(|r| r.info_hash.len() == 40));
        assert!(rows.iter().all(|r| r.magnet.is_some()));
    }

    #[test]
    fn an_empty_feed_is_not_an_error() {
        // A reachable site with nothing new is a successful empty search and
        // must never mark the source offline.
        let empty = concat!(
            "<?xml version=\"1.0\"?><rss version=\"2.0\">",
            "<channel><title>SubsPlease</title></channel></rss>"
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
    fn an_empty_query_browses_and_a_query_filters_case_insensitively() {
        let rows = parse(FIXTURE).expect("parses");
        assert_eq!(
            filter_by_title(rows.clone(), "").len(),
            3,
            "an empty query is the curated browse, not a filter to nothing"
        );
        assert_eq!(filter_by_title(rows.clone(), "  ").len(), 3);
        assert_eq!(filter_by_title(rows.clone(), "TOM").len(), 1);
        assert_eq!(filter_by_title(rows.clone(), "進撃").len(), 1);
        assert!(
            filter_by_title(rows, "a show from five years ago").is_empty(),
            "outside the feed's window is empty, not an error"
        );
    }

    #[test]
    fn pub_dates_convert_to_unix_seconds_including_offsets() {
        assert_eq!(
            pub_date_to_unix("Sat, 08 Aug 2026 16:00:00 +0000"),
            Some(1_786_204_800)
        );
        assert_eq!(
            pub_date_to_unix("Sun, 09 Aug 2026 01:00:00 +0900"),
            Some(1_786_204_800)
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0, "the epoch is day zero");
        // Garbage is unknown, not epoch — a wrong date sorts worse than none.
        assert_eq!(pub_date_to_unix(""), None);
        assert_eq!(pub_date_to_unix("last Tuesday"), None);
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let source = SubsPlease::new();
        assert_eq!(source.def().id, SourceId::SubsPlease);
        assert_eq!(source.def().groups, &[SourceGroup::Anime]);
        assert!(
            !source.def().reports_health,
            "the feed states no swarm data"
        );
        assert_eq!(HOSTS, &["subsplease.org"]);
    }
}
