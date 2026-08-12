//! EZTV (TV, RSS) — one recent-episode feed, filtered locally.
//!
//! EZTV publishes a feed of the last few hundred episodes rather than a search
//! API, and its mirrors do not agree on a server-side query parameter
//! (`docs/sources.md` §3.5). So the fetch is always the same URL and the query is
//! applied here as a title filter. That has a consequence worth stating plainly:
//! a search for an old season legitimately returns nothing, because the episode
//! is simply not in the feed. That is the source's limit, not a parse failure.
//!
//! The feed carries an `xmlns:torrent` extension with the infohash, the byte
//! length and real swarm counts, so `reports_health` is true. Which of those
//! elements a mirror actually emits varies, which is why every field here is
//! optional and a missing one never fails an item — only a missing infohash or
//! a missing name does (`FR-14`).

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::{Reader, XmlVersion};

use crate::core::error::SourceError;
use crate::core::magnet::{build_magnet, info_hash_from_magnet, normalize_info_hash};
use crate::core::types::{
    SearchCtx, SearchFuture, Source, SourceDef, SourceGroup, SourceId, TorrentResult,
};
use crate::sources::net::SourceClient;

/// Mirrors, in preference order. EZTV domains rot within months — this list is
/// expected to churn, and the engine's sticky hint reorders it at runtime.
pub const HOSTS: &[&str] = &["eztvx.to", "eztv.re", "eztv1.xyz", "eztv.wf"];

/// The only feed EZTV publishes. There is no search path the mirrors agree on,
/// which is why [`filter_by_title`] exists.
const FEED_PATH: &str = "/ezrss.xml";

const DEF: SourceDef = SourceDef {
    id: SourceId::Eztv,
    label: "EZTV",
    groups: &[SourceGroup::Tv],
    homepage: "https://eztvx.to",
    reports_health: true,
};

/// The EZTV adapter.
pub struct Eztv {
    client: SourceClient,
}

impl Eztv {
    /// Builds the adapter with its own connection pool (see [`SourceClient`]).
    pub fn new() -> Self {
        Self {
            client: SourceClient::new(),
        }
    }
}

impl Default for Eztv {
    fn default() -> Self {
        Self::new()
    }
}

/// One `<item>` while it is being read.
///
/// Every field is optional because mirrors disagree about which extension
/// elements they emit; the row is judged once, in [`Item::finish`], and only on
/// the two things that make it unusable.
#[derive(Default)]
struct Item {
    title: String,
    /// From `<torrent:infoHash>` when the mirror bothers to emit it.
    info_hash: Option<String>,
    /// Every text that might hold a magnet, newline-joined so a single scan
    /// finds the hash and cannot splice one across two fields.
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
            "magneturi" | "description" | "link" => {
                self.magnet_text.push('\n');
                self.magnet_text.push_str(value);
            }
            "contentlength" => self.set_size(parse_size(value)),
            // `seeds`/`peers` is the ezrss spelling; some mirrors ship the
            // commoner `seeders`/`leechers` instead.
            "seeds" | "seeders" => self.seeders = value.parse().unwrap_or(0),
            "peers" | "leechers" => self.leechers = value.parse().unwrap_or(0),
            "pubdate" => self.added = pub_date_to_unix(value),
            _ => {}
        }
    }

    /// Reads `<enclosure>`, whose payload lives entirely in attributes.
    fn absorb_enclosure(&mut self, tag: &BytesStart<'_>) -> Result<(), SourceError> {
        for attribute in tag.attributes() {
            let attribute = attribute.map_err(|e| SourceError::Parse(e.to_string()))?;
            let key = local_name(attribute.key);
            // The feed is XML 1.0, so `&amp;` in the magnet URL resolves under
            // 1.0 rules — which matters, because the hash would otherwise run
            // straight into `dn` and the row would be silently dropped.
            let value = attribute
                .normalized_value(XmlVersion::Explicit1_0)
                .map_err(|e| SourceError::Parse(e.to_string()))?;
            match key.as_str() {
                "url" => {
                    self.magnet_text.push('\n');
                    self.magnet_text.push_str(&value);
                }
                "length" => self.set_size(parse_size(&value)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Records a size, ignoring zeroes.
    ///
    /// 0 means "this mirror did not say", so it must never overwrite a real
    /// number that arrived from another element — which also makes the order of
    /// `<enclosure>` and `<torrent:contentLength>` irrelevant.
    fn set_size(&mut self, size: u64) {
        if size > 0 {
            self.size_bytes = size;
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
            // Rebuilt rather than reused: the feed's own magnet carries trackers
            // and a `dn` we did not choose, and the cache compares these strings.
            magnet: Some(build_magnet(&info_hash, &name)),
            info_hash,
            name,
            size_bytes: self.size_bytes,
            seeders: self.seeders,
            leechers: self.leechers,
            num_files: None,
            source: SourceId::Eztv,
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
/// A feed with no `<item>`s is `Ok(vec![])`, never an error: a reachable mirror
/// with nothing to say must not be reported offline.
pub fn parse(body: &str) -> Result<Vec<TorrentResult>, SourceError> {
    let mut reader = Reader::from_str(body);
    // Feeds ship bare `&` inside episode titles. Rejecting the whole document
    // over one stray ampersand would lose every good row to fix nothing.
    reader.config_mut().allow_dangling_amp = true;

    let mut out = Vec::new();
    let mut depth = 0usize;
    // `Some(d)` while inside an `<item>` that opened at depth `d`. Tracking the
    // depth rather than a boolean is what keeps a nested `<b>` in a description
    // from being mistaken for a field of its own.
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
                    if name == "enclosure" {
                        item.absorb_enclosure(&tag)?;
                    }
                    field = Some(name);
                    text.clear();
                }
            }
            // `<enclosure …/>` is usually self-closing, and self-closing tags
            // change no depth.
            Event::Empty(tag) => {
                if item_depth == Some(depth) && local_name(tag.name()) == "enclosure" {
                    item.absorb_enclosure(&tag)?;
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
                // quick-xml reports `&amp;` as its own event instead of folding
                // it into the surrounding text, so `Tom &amp; Jerry` arrives as
                // three events and has to be reassembled here.
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
                    // `&nbsp;` is not worth losing the episode over.
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
/// Matching the local name rather than the literal `torrent:seeds` keeps parsing
/// alive on mirrors that bind the extension namespace to a different prefix,
/// which is the documented EZTV drift (`docs/sources.md` §3.5).
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

/// Parses a size stated either as a byte count or in human units.
///
/// EZTV mirrors disagree: most write `<torrent:contentLength>` as plain bytes,
/// some write "700 MB". Accepting both is cheaper than losing the size column on
/// half the mirrors, and anything unreadable becomes 0 — which the UI renders as
/// unknown rather than as a confidently wrong number.
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

/// Converts an RSS `pubDate` ("Tue, 04 Aug 2026 09:15:00 +0000") to unix seconds.
///
/// Hand-rolled rather than taking a date-time dependency: `TorrentResult::added`
/// is an integer precisely so no calendar type has to exist downstream
/// (`core::types`). Anything unreadable becomes `None` — an unknown publication
/// date is not a reason to drop an otherwise usable episode.
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
/// abbreviations are not worth carrying for feeds that always write an offset.
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

impl Source for Eztv {
    fn def(&self) -> &'static SourceDef {
        &DEF
    }

    fn search<'a>(&'a self, query: &'a str, ctx: &'a SearchCtx) -> SearchFuture<'a> {
        Box::pin(async move {
            // Always the same URL: the query cannot be pushed to the server, so
            // the fetch is identical for a browse and for a search.
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

    const FIXTURE: &str = include_str!("fixtures/eztv.xml");

    #[test]
    fn parses_every_field_the_extension_namespace_provides() {
        let rows = parse(FIXTURE).expect("fixture parses");
        let first = &rows[0];
        assert_eq!(first.name, "Example Show S01E01 1080p WEB h264-HARBOUR");
        assert_eq!(first.info_hash, "1111111111111111111111111111111111111111");
        assert_eq!(first.size_bytes, 1_503_238_553);
        assert_eq!(first.seeders, 431);
        assert_eq!(first.leechers, 27);
        assert_eq!(first.num_files, None, "the feed never states a file count");
        assert_eq!(first.source, SourceId::Eztv);
        assert_eq!(first.added, Some(1_785_834_900));
        assert_eq!(
            first.magnet.as_deref(),
            Some(build_magnet(&first.info_hash, &first.name).as_str()),
            "the magnet is rebuilt, not copied out of the feed"
        );
    }

    #[test]
    fn a_mirror_without_the_extension_still_yields_a_usable_row() {
        // The second item has no torrent:infoHash and no torrent:contentLength:
        // both have to come off the enclosure, and the hash arrives uppercased.
        let rows = parse(FIXTURE).expect("parses");
        let second = &rows[1];
        assert_eq!(second.name, "Another Series S03E07 720p HDTV x264-GROUP");
        assert_eq!(
            second.info_hash, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "an uppercase hash must be canonicalized at the boundary"
        );
        assert_eq!(second.size_bytes, 734_003_200, "from enclosure/@length");
        assert_eq!(second.added, Some(1_785_796_811));
    }

    #[test]
    fn an_item_missing_everything_optional_is_still_rendered() {
        // Last item: no pubDate, no swarm counts, no size — none of which make a
        // row unusable, so it must survive with neutral values.
        let rows = parse(FIXTURE).expect("parses");
        let minimal = rows.last().expect("a last row");
        assert_eq!(minimal.name, "Minimal Mirror Show S05E12 1080p WEB");
        assert_eq!(
            minimal.info_hash,
            "5555555555555555555555555555555555555555"
        );
        assert_eq!(minimal.size_bytes, 0);
        assert_eq!(minimal.seeders, 0);
        assert_eq!(minimal.added, None, "unknown, not epoch");
    }

    #[test]
    fn drops_rows_that_could_never_be_downloaded_or_named() {
        let rows = parse(FIXTURE).expect("parses");
        assert_eq!(rows.len(), 4, "six items, two of them unusable");
        assert!(
            rows.iter().all(|r| r.info_hash.len() == 40),
            "a truncated infohash must be dropped, not rendered"
        );
        assert!(
            rows.iter().all(|r| !r.name.trim().is_empty()),
            "an unnameable row must be dropped (FR-14)"
        );
        assert!(
            !rows.iter().any(|r| r.name.contains("Broken Row")),
            "the item with a `deadbeef` hash must not appear"
        );
        assert!(rows.iter().all(|r| r.magnet.is_some()));
    }

    #[test]
    fn an_escaped_ampersand_in_a_title_survives_intact() {
        // quick-xml reports entities as their own events; losing them would
        // silently corrupt both the name and any magnet read out of an element.
        let rows = parse(FIXTURE).expect("parses");
        let tom = rows
            .iter()
            .find(|r| r.name.starts_with("Tom"))
            .expect("the escaped-title row");
        assert_eq!(tom.name, "Tom & Jerry Tales S02E05 480p DVDRip x264");
        assert_eq!(tom.size_bytes, 734_003_200, "a human contentLength parses");
        let magnet = tom.magnet.as_deref().unwrap_or_default();
        assert!(
            !magnet
                .split("&dn=")
                .nth(1)
                .unwrap_or_default()
                .contains('&'),
            "the ampersand must be percent-encoded in the magnet"
        );
    }

    #[test]
    fn an_empty_feed_is_not_an_error() {
        // A reachable mirror with nothing to say is a successful empty search and
        // must never mark the source offline.
        let empty = concat!(
            "<?xml version=\"1.0\"?><rss version=\"2.0\">",
            "<channel><title>EZTV</title></channel></rss>"
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
            4,
            "an empty query is the curated browse, not a filter to nothing"
        );
        assert_eq!(filter_by_title(rows.clone(), "   ").len(), 4);
        assert_eq!(filter_by_title(rows.clone(), "s01e01").len(), 1);
        assert_eq!(
            filter_by_title(rows.clone(), "EXAMPLE SHOW").len(),
            1,
            "matching is case-insensitive in both directions"
        );
        assert!(
            filter_by_title(rows, "a show that is not in the feed").is_empty(),
            "an old-season miss is empty, not an error"
        );
    }

    #[test]
    fn sizes_parse_from_both_byte_counts_and_human_units() {
        assert_eq!(parse_size("1503238553"), 1_503_238_553);
        assert_eq!(parse_size("700 MB"), 734_003_200);
        assert_eq!(parse_size("700MB"), 734_003_200, "the space is optional");
        assert_eq!(parse_size("1.4 GiB"), 1_503_238_553);
        assert_eq!(parse_size(" 2 TiB "), 2_199_023_255_552);
        assert_eq!(parse_size("512 bytes"), 512);
        // Unknown is 0, never a guess: the UI renders 0 as "—".
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size("unknown"), 0);
        assert_eq!(parse_size("0 Bytes"), 0);
    }

    #[test]
    fn pub_dates_convert_to_unix_seconds_including_offsets() {
        assert_eq!(
            pub_date_to_unix("Tue, 04 Aug 2026 09:15:00 +0000"),
            Some(1_785_834_900)
        );
        assert_eq!(
            pub_date_to_unix("04 Aug 2026 09:15:00 GMT"),
            Some(1_785_834_900),
            "the weekday is optional and a named zone is UTC"
        );
        // An offset east of UTC is earlier in absolute time by that much.
        assert_eq!(
            pub_date_to_unix("Tue, 04 Aug 2026 14:45:00 +0530"),
            Some(1_785_834_900)
        );
        assert_eq!(
            pub_date_to_unix("Tue, 04 Aug 2026 04:15:00 -0500"),
            Some(1_785_834_900)
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0, "the epoch is day zero");
        // Garbage is unknown, not epoch — a wrong date sorts worse than none.
        assert_eq!(pub_date_to_unix(""), None);
        assert_eq!(pub_date_to_unix("yesterday"), None);
        assert_eq!(pub_date_to_unix("Tue, 04 Xxx 2026 09:15:00 +0000"), None);
    }

    #[test]
    fn the_definition_matches_the_source_matrix() {
        let source = Eztv::new();
        assert_eq!(source.def().id, SourceId::Eztv);
        assert_eq!(source.def().groups, &[SourceGroup::Tv]);
        assert!(
            source.def().reports_health,
            "the torrent namespace carries real seed counts"
        );
        assert!(!HOSTS.is_empty(), "mirrors are config, not code");
    }
}
