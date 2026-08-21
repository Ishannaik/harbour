//! Deterministic fake data for the phase-2 views (docs/design.md §2).
//!
//! The engine and source tracks land later, so the UI wires against seeded
//! fakes for search results, the queue, and recently-downloaded history.
//! Every function is a pure function of its inputs — same input, same list,
//! same order — so smoke tests can assert on concrete rows. No `std::time`,
//! no global state: determinism is the contract.

//! `dead_code` is allowed module-wide until `app.rs` wires the phase-2
//! views to this generator; mirrors `theme_watch.rs`'s staged-API allow.
//! Remove it as the wiring lands.

#![allow(dead_code)]

use std::path::PathBuf;

use std::time::Duration;

use crate::core::magnet::build_magnet;
use crate::core::types::{
    CompletedItem, EngineStats, ItemView, QueueItem, QueueStatus, SourceGroup, SourceId,
    TorrentResult,
};

/// Sidebar source matrix (docs/sources.md §2): id, chip label, name suffix,
/// and the size band in bytes that source plausibly produces — all bands
/// inside the 700 MiB–60 GiB fake-data contract.
const SOURCES: &[(SourceId, &str, &str, u64, u64)] = &[
    (
        SourceId::GamesHub,
        "GamesHub",
        "(2026) [Repack] [Multi]",
        20 << 30,
        60 << 30,
    ),
    (
        SourceId::CineVault,
        "CineVault",
        "(2026) [1080p] [WEBRip]",
        1 << 30,
        4 << 30,
    ),
    (
        SourceId::VaultMovies,
        "VaultIndex",
        "(2026) [1080p] [BluRay]",
        734_003_200,
        4 << 30,
    ),
    (
        SourceId::ReelSource,
        "ReelIndex",
        "(2026) [1080p] [BluRay]",
        2 << 30,
        8 << 30,
    ),
    (
        SourceId::ShowPort,
        "ShowPort",
        "S01E01 [1080p] [WEBRip]",
        734_003_200,
        3 << 30,
    ),
    (
        SourceId::VaultTv,
        "VaultIndex",
        "S01E01 [1080p] [H.264]",
        734_003_200,
        3 << 30,
    ),
    (
        SourceId::TsukiBase,
        "TsukiBase",
        "- 01 [1080p] [HEVC] [AAC]",
        734_003_200,
        2 << 30,
    ),
    (
        SourceId::FanSubs,
        "FanSubs",
        "- 01 [1080p] [HEVC]",
        734_003_200,
        (3 << 30) / 2,
    ),
    (
        SourceId::TorrentHub,
        "TorrentHub",
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

/// Fake data goes through the *real* magnet builder rather than formatting its
/// own string, so the fixtures exercise the same encoding the product uses.
fn magnet(hash: &str, name: &str) -> String {
    build_magnet(hash, name)
}

/// Demo-only query → category heuristic (FR-25's grouped layout). The REAL
/// per-source relevance comes from the scrapers (FR-14/15: each source
/// searches and returns only what it actually carries); this tiny lexicon
/// only decides which fake sources *participate*, so a demo search for an
/// anime doesn't show a 55 GB GamesHub repack on top.
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

/// Words that route a query to the Games group (GamesHub repacks).
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
    "show", "series", "sitcom", "episode", "s01e", "s1e", "season", "shogun",
];

/// Words that route a query to the Anime group (tsukibase/fansubs).
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
];

/// Which fake sources answer for a category. Only sources in the SOURCES
/// matrix — browse mode (empty query) skips this and uses the whole matrix,
/// because a curated library is cross-source by design.
fn category_sources(category: SourceGroup) -> &'static [SourceId] {
    match category {
        SourceGroup::Games => &[SourceId::GamesHub],
        SourceGroup::Movies => &[
            SourceId::CineVault,
            SourceId::VaultMovies,
            SourceId::TorrentHub,
        ],
        SourceGroup::Tv => &[SourceId::ShowPort, SourceId::VaultTv],
        SourceGroup::Anime => &[SourceId::TsukiBase, SourceId::FanSubs],
    }
}

/// One demo-catalog entry: keywords that select it (lowercased substrings)
/// and the real release title the fake rows should show. The catalog exists
/// so a demo search reads like a real search — "tensura slime" yields
/// "That Time I Got Reincarnated as a Slime - 01 [1080p] [HEVC]", not
/// "tensura slime - 01 [...]". Real titles come from the scrapers; this is
/// display sugar for the fake generator only.
struct CatalogEntry {
    category: SourceGroup,
    keywords: &'static [&'static str],
    title: &'static str,
}

const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["slime", "tensura", "reincarnated"],
        title: "That Time I Got Reincarnated as a Slime",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["frieren", "sousou no"],
        title: "Frieren: Beyond Journey's End",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["one piece"],
        title: "One Piece",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["jujutsu"],
        title: "Jujutsu Kaisen",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["demon slayer", "kimetsu"],
        title: "Demon Slayer: Kimetsu no Yaiba",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["naruto"],
        title: "Naruto",
    },
    CatalogEntry {
        category: SourceGroup::Anime,
        keywords: &["attack on titan", "shingeki"],
        title: "Attack on Titan",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["interstellar"],
        title: "Interstellar",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["dune"],
        title: "Dune: Part Two",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["oppenheimer"],
        title: "Oppenheimer",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["batman"],
        title: "The Batman",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["inception"],
        title: "Inception",
    },
    CatalogEntry {
        category: SourceGroup::Movies,
        keywords: &["matrix"],
        title: "The Matrix",
    },
    CatalogEntry {
        category: SourceGroup::Tv,
        keywords: &["the boys", "boys"],
        title: "The Boys",
    },
    CatalogEntry {
        category: SourceGroup::Tv,
        keywords: &["shogun"],
        title: "Shogun",
    },
    CatalogEntry {
        category: SourceGroup::Tv,
        keywords: &["severance"],
        title: "Severance",
    },
    CatalogEntry {
        category: SourceGroup::Tv,
        keywords: &["last of us"],
        title: "The Last of Us",
    },
    CatalogEntry {
        category: SourceGroup::Tv,
        keywords: &["game of thrones"],
        title: "Game of Thrones",
    },
    CatalogEntry {
        category: SourceGroup::Games,
        keywords: &["elden"],
        title: "Elden Ring",
    },
    CatalogEntry {
        category: SourceGroup::Games,
        keywords: &["witcher"],
        title: "The Witcher 3: Wild Hunt",
    },
    CatalogEntry {
        category: SourceGroup::Games,
        keywords: &["cyberpunk"],
        title: "Cyberpunk 2077",
    },
    CatalogEntry {
        category: SourceGroup::Games,
        keywords: &["gta", "grand theft"],
        title: "Grand Theft Auto V",
    },
    CatalogEntry {
        category: SourceGroup::Games,
        keywords: &["red dead"],
        title: "Red Dead Redemption 2",
    },
];

/// Real release title when the query matches the demo catalog, else None
/// (caller falls back to "{query} {suffix}").
fn release_title(query: &str, category: SourceGroup) -> Option<&'static str> {
    let q = query.to_ascii_lowercase();
    CATALOG
        .iter()
        .find(|e| e.category == category && e.keywords.iter().any(|k| q.contains(k)))
        .map(|e| e.title)
}

/// Deterministic fake search results for `query`: 8–12 hits, seeded by the
/// query so re-searching the same term shows the same list (smoke tests can
/// depend on it). Sources are routed by [`query_category`]: an anime query
/// answers from tsukibase/fansubs only, a game query from GamesHub — the
/// sidebar stagger then lights exactly the group that answered. A matched
/// catalog title reads like a real release, not the typed term repeated.
/// Browse mode (empty query, FR-20) spans the whole source matrix.
pub fn fake_results(query: &str) -> Vec<TorrentResult> {
    let seed = seed_from(query);
    let mut rng = Rng(seed);
    // Hit count is part of the seeded output: 8..=12.
    let count = 8 + (seed % 5) as usize;
    // Which sources answer this query; browse spans the whole matrix.
    let category = if query.is_empty() {
        None
    } else {
        Some(query_category(query))
    };
    let sources: Vec<SourceId> = match category {
        Some(cat) => category_sources(cat).to_vec(),
        None => SOURCES.iter().map(|(id, ..)| *id).collect(),
    };
    (0..count)
        .map(|i| {
            let id = sources[i % sources.len()];
            // `id` is drawn from `SOURCES` itself (directly or via
            // `category_sources`), so the lookup cannot miss.
            let (_id, _label, suffix, min_size, max_size) = *SOURCES
                .iter()
                .find(|(sid, ..)| *sid == id)
                .unwrap_or_else(|| unreachable!("{id} is drawn from SOURCES"));
            // Unique infohash per hit: golden-ratio mix of the seed and the
            // index spreads hits apart without consuming RNG state.
            let info_hash = hash40(seed ^ (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            // Browse mode has no search term to paste into the name — the
            // suffix alone reads as a curated row. A matched catalog title
            // reads like a real release; unmatched queries fall back.
            let name = match (
                query.is_empty(),
                category.and_then(|c| release_title(query, c)),
            ) {
                (true, _) => suffix.trim().to_string(),
                (false, Some(title)) => format!("{title} {suffix}"),
                (false, None) => format!("{query} {suffix}"),
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
                magnet: Some(magnet(&info_hash, &name)),
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
) -> ItemView {
    let id = hash40(id_seed);
    let mut item = QueueItem::new(
        id.clone(),
        name.to_owned(),
        source,
        Some(magnet(&id, name)),
        PathBuf::from("~/harbour/downloads"),
        added_at_epoch_ms,
    );
    item.status = status;
    item.finished = finished;
    item.total_bytes = total_bytes;

    // A queued item has never reached the engine, so it genuinely has no live
    // statistics — `None` rather than a struct full of zeroes, which is what
    // lets the view render "—" instead of inventing a peer count.
    let stats = if status == QueueStatus::Queued {
        None
    } else {
        Some(EngineStats {
            progress,
            downloaded_bytes: (total_bytes as f64 * progress) as u64,
            total_bytes,
            speed_mib,
            upload_speed_mib,
            uploaded_bytes: if finished { total_bytes * 2 } else { 0 },
            peers,
            eta: eta_secs.map(Duration::from_secs),
        })
    };
    ItemView::new(item, stats)
}

/// Three seeded fake queue items exercising every render branch of the
/// downloads view: one Downloading (~42% eased), one Paused, one Seeding
/// (finished = true). ids are stable 40-hex strings — never change the
/// seeds, tests may key on the hashes. The Paused item reports `peers` and
/// `eta_secs` as None: librqbit can't report either while paused, and the
/// view renders the em-dash, never 0 (types.rs contract B1/B2).
pub fn fake_queue() -> Vec<ItemView> {
    vec![
        queue_item(
            0x1_01, // stable id seed
            "Oppenheimer (2023) [1080p] [WEBRip]",
            Some(SourceId::CineVault),
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
            Some(SourceId::ShowPort),
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
            Some(SourceId::TsukiBase),
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
pub fn fake_history() -> Vec<CompletedItem> {
    let id = hash40(0xfeed_face_cafe_beef);
    vec![CompletedItem {
        id,
        name: "Dune: Part Two (2024) [1080p] [BluRay]".to_owned(),
        size_bytes: 4 << 30,
        source: Some(SourceId::ReelSource),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_ids(results: &[TorrentResult]) -> Vec<String> {
        results
            .iter()
            .map(|r| r.source.as_str().to_string())
            .collect()
    }

    #[test]
    fn anime_query_answers_only_anime_sources() {
        let results = fake_results("tensura slime");
        assert!(!results.is_empty());
        for id in source_ids(&results) {
            assert!(
                id == "tsukibase" || id == "fansubs",
                "anime query leaked source {id}"
            );
        }
    }

    #[test]
    fn game_query_answers_only_gameshub() {
        let results = fake_results("elden ring repack");
        assert!(!results.is_empty());
        for id in source_ids(&results) {
            assert_eq!(id, "gameshub", "game query leaked source {id}");
        }
    }

    #[test]
    fn default_query_routes_to_movies() {
        assert_eq!(query_category("interstellar"), SourceGroup::Movies);
        let results = fake_results("interstellar");
        for id in source_ids(&results) {
            assert!(
                id != "gameshub" && id != "tsukibase",
                "movie query leaked source {id}"
            );
        }
    }

    #[test]
    fn tv_and_anime_lexicon_routes() {
        assert_eq!(query_category("the boys s01e05"), SourceGroup::Tv);
        assert_eq!(query_category("frieren"), SourceGroup::Anime);
        assert_eq!(query_category("tensura slime"), SourceGroup::Anime);
    }

    #[test]
    fn browse_mode_spans_the_whole_matrix() {
        let results = fake_results("");
        let mut seen = source_ids(&results);
        seen.sort();
        seen.dedup();
        assert!(seen.len() > 3, "browse should span groups, got {seen:?}");
    }

    #[test]
    fn catalog_query_shows_real_titles_not_the_typed_term() {
        let results = fake_results("tensura slime");
        let first = &results[0].name;
        assert!(
            first.contains("That Time I Got Reincarnated as a Slime"),
            "expected the real title, got: {first}"
        );
        assert!(
            !first.to_ascii_lowercase().contains("tensura"),
            "typed term must not be pasted into the name: {first}"
        );
        for r in &results {
            assert!(r.name.contains("Slime"), "row: {}", r.name);
        }
    }

    #[test]
    fn movie_catalog_and_unmatched_fallback() {
        let dune = fake_results("dune");
        assert!(dune[0].name.contains("Dune: Part Two"));
        let xyz = fake_results("xyzzy");
        assert!(xyz[0].name.to_ascii_lowercase().starts_with("xyzzy"));
    }

    #[test]
    fn catalog_is_deterministic() {
        let a = fake_results("tensura slime");
        let b = fake_results("tensura slime");
        let names_a: Vec<&str> = a.iter().map(|r| r.name.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names_a, names_b);
    }
}
