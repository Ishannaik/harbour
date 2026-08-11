//! Deterministic fake data for the phase-2 views (docs/design.md §2).
//!
//! The engine and source tracks land later, so the UI wires against seeded
//! fakes for search results, the queue, and recently-downloaded history.
//! Every function is a pure function of its inputs — same input, same list,
//! same order — so smoke tests can assert on concrete rows. No `std::time`,
//! no global state: determinism is the contract.

use std::path::PathBuf;

use crate::types::{HistoryItem, QueueItem, QueueStatus, SourceGroup, SourceId, TorrentResult};

/// Sidebar source matrix (docs/sources.md §2): id, chip label, name suffix,
/// and the size band in bytes that source plausibly produces — all bands
/// inside the 700 MiB–60 GiB fake-data contract.
const SOURCES: &[(&str, &str, &str, u64, u64)] = &[
    (
        "fitgirl",
        "FitGirl",
        "(2026) [Repack] [Multi]",
        20 << 30,
        60 << 30,
    ),
    ("yts", "YTS", "(2026) [1080p] [WEBRip]", 1 << 30, 4 << 30),
    (
        "tpb-movies",
        "TPB",
        "(2026) [1080p] [BluRay]",
        734_003_200,
        4 << 30,
    ),
    (
        "x1337-movies",
        "1337x",
        "(2026) [1080p] [BluRay]",
        2 << 30,
        8 << 30,
    ),
    (
        "eztv",
        "EZTV",
        "S01E01 [1080p] [WEBRip]",
        734_003_200,
        3 << 30,
    ),
    (
        "tpb-tv",
        "TPB",
        "S01E01 [1080p] [H.264]",
        734_003_200,
        3 << 30,
    ),
    (
        "nyaa",
        "Nyaa",
        "- 01 [1080p] [HEVC] [AAC]",
        734_003_200,
        2 << 30,
    ),
    (
        "subsplease",
        "SubsPlease",
        "- 01 [1080p] [HEVC]",
        734_003_200,
        (3 << 30) / 2,
    ),
    (
        "bittorrented",
        "BitTorrented",
        "(2026) [1080p] [BluRay]",
        1 << 30,
        6 << 30,
    ),
];

/// Tiny xorshift64* PRNG — the same pattern as app.rs's `Rng`, seeded per
/// call so every output is a deterministic function of the seed.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in [0, 1), normalized exactly like app.rs.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// FNV-1a fold of the query bytes — any stable string hash works, the goal
/// is only that identical queries produce identical seeds.
fn seed_from(query: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in query.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 40 lowercase hex chars derived from `seed` — a stand-in infohash for ids
/// and magnets. Two hex chars per LCG draw keeps it cheap and deterministic.
fn hash40(seed: u64) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rng = Rng(seed);
    let mut out = String::with_capacity(40);
    for _ in 0..20 {
        let byte = rng.next_u64() as u8;
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// `magnet:?` URI for a fake infohash; `dn` uses `+` for spaces like the
/// real magnet builder, so a fake URL stays paste-able into any client.
fn magnet(hash: &str, name: &str) -> String {
    format!("magnet:?xt=urn:btih:{hash}&dn={}", name.replace(' ', "+"))
}

/// Demo-only query → category heuristic (FR-25's grouped layout). The REAL
/// per-source relevance comes from Dhruv's scrapers (FR-14/15: each source
/// searches and returns only what it actually carries); until then this tiny
/// lexicon decides which sources *participate* so a demo search for an anime
/// doesn't show a 55 GB FitGirl repack on top. Honest scope: it routes the
/// group, it never invents rows a source wouldn't plausibly carry.
fn query_category(query: &str) -> SourceGroup {
    let q = query.to_ascii_lowercase();
    if GAME_WORDS.iter().any(|w| q.contains(w)) {
        SourceGroup::Games
    } else if TV_WORDS.iter().any(|w| q.contains(w)) {
        SourceGroup::Tv
    } else if ANIME_WORDS.iter().any(|w| q.contains(w)) {
        SourceGroup::Anime
    } else {
        SourceGroup::Movies
    }
}

/// Words that route a query to the Games group (FitGirl repacks).
const GAME_WORDS: &[&str] = &[
    "repack",
    "game",
    "crack",
    "iso",
    "elden",
    "zelda",
    "witcher",
    "gta",
    "fifa",
    "sims",
    "skyrim",
    "cyberpunk",
];

/// Episode markers are TV-first (most shows); anime-only terms live in
/// [`ANIME_WORDS`], so "slime s01e01" routes to TV but "slime" routes to
/// Anime. Honest demo heuristic — real relevance is the scrapers' job.
const TV_WORDS: &[&str] = &[
    "show", "series", "sitcom", "episode", "s01e", "s1e", "season",
];

/// Words that route a query to the Anime group (nyaa/subsplease).
const ANIME_WORDS: &[&str] = &[
    "anime",
    "sub",
    "dub",
    "ova",
    "manga",
    "slime",
    "tensura",
    "one piece",
    "naruto",
    "jujutsu",
    "demon slayer",
    "frieren",
    "shogun",
];

/// Which sources answer for a category. Browse mode (empty query) skips this
/// and uses the whole matrix — a curated library is cross-source by design.
fn category_sources(category: SourceGroup) -> &'static [&'static str] {
    match category {
        SourceGroup::Games => &["fitgirl"],
        SourceGroup::Movies => &["yts", "tpb-movies", "x1337-movies", "bittorrented"],
        SourceGroup::Tv => &["eztv", "tpb-tv", "x1337-tv"],
        SourceGroup::Anime => &["nyaa", "subsplease"],
    }
}

/// Deterministic fake search results for `query`: 8–12 hits, seeded by the
/// query so re-searching the same term shows the same list (smoke tests can
/// depend on it). Sources are routed by [`query_category`]: an anime query
/// answers from nyaa/subsplease only, a game query from FitGirl — the
/// sidebar stagger then lights exactly the group that answered. Browse mode
/// (empty query, FR-20) spans the whole source matrix. Names look like
/// "{query} {suffix}"; sizes span 700 MiB–60 GiB; seeders 3–9000; leechers
/// 1–300; each magnet is built from a local 40-hex hash from the seed.
pub fn fake_results(query: &str) -> Vec<TorrentResult> {
    let seed = seed_from(query);
    let mut rng = Rng(seed);
    // Hit count is part of the seeded output: 8..=12.
    let count = 8 + (seed % 5) as usize;
    // Which sources answer this query; browse spans the whole matrix.
    let sources: &[&str] = if query.is_empty() {
        &SOURCES.iter().map(|(id, ..)| *id).collect::<Vec<&str>>()
    } else {
        category_sources(query_category(query))
    };
    (0..count)
        .map(|i| {
            let id = sources[i % sources.len()];
            let (_id, _label, suffix, min_size, max_size) =
                *SOURCES.iter().find(|(sid, ..)| *sid == id).unwrap();
            // Unique infohash per hit: golden-ratio mix of the seed and the
            // index spreads hits apart without consuming RNG state.
            let info_hash = hash40(seed ^ (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            // Browse mode (empty query, FR-20) has no search term to paste
            // into the name — the suffix alone reads as a curated row.
            let name = if query.is_empty() {
                suffix.trim().to_string()
            } else {
                format!("{query} {suffix}")
            };
            TorrentResult {
                info_hash: info_hash.clone(),
                name: name.clone(),
                size_bytes: (min_size as f64 + rng.next_f64() * (max_size - min_size) as f64)
                    as u64,
                seeders: 3 + (rng.next_f64() * 8_998.0) as u32,
                leechers: 1 + (rng.next_f64() * 300.0) as u32,
                num_files: Some(1 + (rng.next_f64() * 49.0) as u32),
                source: id,
                magnet: magnet(&info_hash, &name),
                // Unix seconds, staggered an hour per hit so ordering is
                // visible in the list.
                added: Some(1_786_000_000 + i as i64 * 3_600),
            }
        })
        .collect()
}

/// Builds one queue item from a fixed id seed — ids and magnets derive from
/// the seed, everything else is spelled out at the call site so each render
/// branch reads as data.
///
/// The long argument list is the point: this is fixture data, and spelling
/// every field at the call site is what makes each render branch readable as
/// a table. A builder or params struct here would add indirection to test
/// data that has exactly three call sites.
#[allow(clippy::too_many_arguments)]
fn queue_item(
    id_seed: u64,
    name: &str,
    source: Option<SourceId>,
    status: QueueStatus,
    finished: bool,
    progress: f64,
    total_bytes: u64,
    speed_mib: f64,
    upload_speed_mib: f64,
    peers: Option<u32>,
    eta_secs: Option<u64>,
    added_at_epoch_ms: i64,
) -> QueueItem {
    let id = hash40(id_seed);
    QueueItem {
        id: id.clone(),
        name: name.to_owned(),
        // The helper takes a `SourceId` so callers stay terse; the ledger
        // field is owned (see `QueueItem::source`), so convert at this boundary.
        source: source.map(str::to_owned),
        magnet: magnet(&id, name),
        dir: PathBuf::from("~/harbour/downloads"),
        status,
        finished,
        progress,
        total_bytes,
        downloaded_bytes: (total_bytes as f64 * progress) as u64,
        speed_mib,
        upload_speed_mib,
        uploaded_bytes: if finished { total_bytes * 2 } else { 0 },
        peers,
        eta_secs,
        error: None,
        added_at_epoch_ms,
    }
}

/// Three seeded fake queue items exercising every render branch of the
/// downloads view: one Downloading (~42% eased), one Paused, one Seeding
/// (finished = true). ids are stable 40-hex strings — never change the
/// seeds, tests may key on the hashes. The Paused item reports `peers` and
/// `eta_secs` as None: librqbit can't report either while paused, and the
/// view renders the em-dash, never 0 (types.rs contract B1/B2).
pub fn fake_queue() -> Vec<QueueItem> {
    vec![
        queue_item(
            0x1_01, // stable id seed
            "Oppenheimer (2023) [1080p] [WEBRip]",
            Some("yts"),
            QueueStatus::Downloading,
            false,
            0.4213,  // ~42% — the eased display value the bar renders
            3 << 30, // 3 GiB
            8.4,     // MiB/s down
            0.0,
            Some(137),
            Some(1_800), // ~30 min at the current speed
            1_786_200_000_000,
        ),
        queue_item(
            0x2_02, // stable id seed
            "Shogun S01E01 [1080p] [WEBRip]",
            Some("eztv"),
            QueueStatus::Paused,
            false,
            0.13,
            2 << 30,
            0.0,
            0.0,
            None, // paused — peers unknown, the view renders '—'
            None, // paused — no ETA, the view renders '—'
            1_786_100_000_000,
        ),
        queue_item(
            0x3_03, // stable id seed
            "Frieren: Beyond Journey's End - 01 [1080p] [HEVC]",
            Some("nyaa"),
            QueueStatus::Seeding,
            true, // completed → seed tab row
            1.0,
            1 << 30,
            0.0,
            2.1, // MiB/s up — the one live number on the seed row
            Some(42),
            None, // a seed has no ETA
            1_785_900_000_000,
        ),
    ]
}

/// One fake history entry for the recently-downloaded section.
pub fn fake_history() -> Vec<HistoryItem> {
    let id = hash40(0xfeed_face_cafe_beef);
    vec![HistoryItem {
        id,
        name: "Dune: Part Two (2024) [1080p] [BluRay]".to_owned(),
        size_bytes: 4 << 30,
        source: Some("x1337-movies".to_owned()),
        completed_at_epoch_ms: 1_785_950_000_000,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SourceGroup;

    #[test]
    fn anime_query_answers_only_anime_sources() {
        let results = fake_results("tensura slime");
        assert!(!results.is_empty());
        for r in &results {
            assert!(
                r.source == "nyaa" || r.source == "subsplease",
                "anime query leaked source {}",
                r.source
            );
        }
    }

    #[test]
    fn game_query_answers_only_fitgirl() {
        let results = fake_results("elden ring repack");
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.source, "fitgirl", "game query leaked source {}", r.source);
        }
    }

    #[test]
    fn default_query_routes_to_movies() {
        assert_eq!(query_category("interstellar"), SourceGroup::Movies);
        assert_eq!(query_category("dune"), SourceGroup::Movies);
        let results = fake_results("interstellar");
        for r in &results {
            assert!(
                r.source != "fitgirl" && r.source != "nyaa",
                "movie query leaked source {}",
                r.source
            );
        }
    }

    #[test]
    fn tv_and_anime_lexicon_routes() {
        assert_eq!(query_category("the boys s01e05"), SourceGroup::Tv);
        assert_eq!(query_category("frieren season 1"), SourceGroup::Tv);
        assert_eq!(query_category("frieren"), SourceGroup::Anime);
        assert_eq!(query_category("tensura slime"), SourceGroup::Anime);
    }

    #[test]
    fn browse_mode_spans_the_whole_matrix() {
        let results = fake_results("");
        let mut seen: Vec<&str> = results.iter().map(|r| r.source).collect();
        seen.sort();
        seen.dedup();
        assert!(seen.len() > 3, "browse should span groups, got {seen:?}");
    }
}
