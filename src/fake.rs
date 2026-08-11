//! Fake-data engine — roadmap Phase 2.
//!
//! Replaces the real sources (Phase 3, Dhruv) and engine (Phase 4, Sarthak)
//! with deterministic, seeded generators so the UI contract is final and
//! testable before any network or swarm code exists. The shape mirrors the
//! real world: a query fans out to 10 sources, each answers after its own
//! latency, one is offline, one comes back empty, and results dedupe by
//! info_hash across sources with per-source tags (design.md §2.2 / §6).
//!
//! Everything is a pure function of `(seed, query)`: the same query on the
//! same seed produces the same plan, hashes, sizes, and latencies forever.
//! The app and the tests share one seed so buffer snapshots stay stable.
//!
//! When Phase 3 lands, this module is deleted — the registry below is a
//! mirror of `docs/sources.md`'s ids, and the sidebar reads the same
//! [`SourceDef`]s, so nothing else changes.

use crate::types::{SourceDef, SourceGroup, SourceStatus, TorrentResult};

/// The 10-source registry, in sidebar order (design.md §2.2). Ids match
/// `docs/sources.md` exactly so the fake swaps out without touching the
/// sidebar or the health-dot logic.
pub const SOURCES: &[SourceDef] = &[
    SourceDef {
        id: "fitgirl",
        label: "FitGirl",
        groups: &[SourceGroup::Games],
        homepage: "fitgirl-repacks.site",
        reports_health: false,
    },
    SourceDef {
        id: "yts",
        label: "YTS",
        groups: &[SourceGroup::Movies],
        homepage: "yts.mx",
        reports_health: true,
    },
    SourceDef {
        id: "tpb-movies",
        label: "TPB",
        groups: &[SourceGroup::Movies],
        homepage: "apibay.org",
        reports_health: true,
    },
    SourceDef {
        id: "x1337-movies",
        label: "1337x",
        groups: &[SourceGroup::Movies],
        homepage: "1337x.to",
        reports_health: true,
    },
    SourceDef {
        id: "eztv",
        label: "EZTV",
        groups: &[SourceGroup::Tv],
        homepage: "eztv.re",
        reports_health: true,
    },
    SourceDef {
        id: "tpb-tv",
        label: "TPB",
        groups: &[SourceGroup::Tv],
        homepage: "apibay.org",
        reports_health: true,
    },
    SourceDef {
        id: "x1337-tv",
        label: "1337x",
        groups: &[SourceGroup::Tv],
        homepage: "1337x.to",
        reports_health: true,
    },
    SourceDef {
        id: "nyaa",
        label: "Nyaa",
        groups: &[SourceGroup::Anime],
        homepage: "nyaa.si",
        reports_health: true,
    },
    SourceDef {
        id: "subsplease",
        label: "SubsPlease",
        groups: &[SourceGroup::Anime],
        homepage: "subsplease.org",
        reports_health: true,
    },
    SourceDef {
        id: "bittorrented",
        label: "BitTorrented",
        groups: &[SourceGroup::Movies],
        homepage: "bittorrented.com",
        reports_health: true,
    },
];

/// Look up a source by id; the sidebar and health maps use this instead of
/// indexing so an unknown id degrades to "not a source" instead of panicking.
pub fn source_by_id(id: &str) -> Option<&'static SourceDef> {
    SOURCES.iter().find(|s| s.id == id)
}

/// The sidebar order of groups (design.md §2.2), each with its sources.
pub const GROUP_ORDER: &[SourceGroup] = &[
    SourceGroup::Games,
    SourceGroup::Movies,
    SourceGroup::Tv,
    SourceGroup::Anime,
];

/// Sources belonging to one group, in registry order.
pub fn sources_in_group(group: SourceGroup) -> impl Iterator<Item = &'static SourceDef> {
    SOURCES.iter().filter(move |s| s.groups.contains(&group))
}

/// The deterministic plan for one query: what each source will do, when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPlan {
    pub per_source: Vec<SourcePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePlan {
    pub source: &'static SourceDef,
    /// Simulated network latency before the source answers (ms).
    pub latency_ms: u64,
    /// What the source will report: results, an empty set, or offline.
    pub status: SourceStatus,
    /// Number of results the source will stream (0 when empty/offline).
    pub result_count: usize,
}

/// Deterministic fake engine. The seed is fixed by default so every run of
/// the app shows the same search; tests may override it.
#[derive(Debug, Clone, Copy)]
pub struct FakeEngine {
    seed: u64,
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngine {
    pub fn new() -> Self {
        // "harb" — fixed so the app is reproducible; tests pass their own.
        Self::with_seed(0x6861_7262_0000_0001)
    }

    pub fn with_seed(seed: u64) -> Self {
        Self { seed }
    }

    /// Builds the fan-out plan for `query`. Exactly one source is offline and
    /// one (different) source comes back empty, chosen deterministically from
    /// the query — the per-source isolation every search exercises.
    pub fn plan(&self, query: &str) -> SearchPlan {
        let mut rng = SeededRng::new(seed_from(query) ^ self.seed);
        let n = SOURCES.len();
        let offline = rng.range(0, n as u64) as usize;
        let mut empty = rng.range(0, n as u64) as usize;
        if empty == offline {
            empty = (empty + 1) % n;
        }
        let per_source = SOURCES
            .iter()
            .enumerate()
            .map(|(i, def)| {
                let status = if i == offline {
                    SourceStatus::Offline
                } else if i == empty {
                    SourceStatus::Empty
                } else {
                    SourceStatus::Online
                };
                SourcePlan {
                    source: def,
                    latency_ms: base_latency_ms(def.id) + rng.range(0, 280),
                    status,
                    result_count: if status == SourceStatus::Empty {
                        0
                    } else {
                        kind_count(def.id)
                    },
                }
            })
            .collect();
        SearchPlan { per_source }
    }

    /// The results one source will stream for `query`. Deterministic per
    /// (seed, query, source): two sources carrying the same release kind
    /// produce the same info_hash, which is what lets the search view show
    /// staggered per-source tags on one row (design.md §2.2).
    pub fn results(&self, query: &str, source_id: &'static str) -> Vec<TorrentResult> {
        // Seeded per (seed, query) — NOT per source: every kind's data is
        // generated once with one rng stream, so a kind shared by two
        // sources produces the identical info_hash from each. That identity
        // is what the search view's dedupe + staggered tags rely on.
        let mut rng = SeededRng::new(seed_from(query) ^ self.seed);
        let base = normalize(query);
        let mut out = Vec::new();
        for (kind, ids) in KIND_SOURCES {
            let title = title_for(*kind, &base, &mut rng);
            let hash = hex40(&mut rng);
            let size_bytes = size_for(*kind, &mut rng);
            let seeders = seeders_for(*kind, &mut rng);
            let leechers = (f64::from(seeders) * (0.3 + rng.next_f64() * 0.5)) as u32;
            let num_files = 1 + rng.range(0, 8) as u32;
            if !ids.contains(&source_id) {
                continue;
            }
            // The magnet is built before the push because `title` moves into
            // `name` below.
            let magnet = format!("magnet:?xt=urn:btih:{hash}&dn={}", urlenc(&title));
            out.push(TorrentResult {
                info_hash: hash,
                name: title,
                size_bytes,
                seeders,
                leechers,
                num_files: Some(num_files),
                source: source_id,
                magnet,
                added: None,
            });
        }
        out
    }

    /// Curated top lists for an empty `Enter` (design.md §1): a fixed set of
    /// picks, each reported by two sources with the same info_hash so the
    /// results list shows staggered tags immediately.
    pub fn curated(&self) -> Vec<TorrentResult> {
        const PICKS: &[(&str, &str, &str)] = &[
            (
                "Elden Ring — Shadow of the Erdtree",
                "fitgirl",
                "x1337-movies",
            ),
            ("Interstellar (2014) 1080p REMUX", "yts", "tpb-movies"),
            ("Severance — Season 2 (2160p)", "x1337-tv", "tpb-tv"),
            (
                "Frieren: Beyond Journey's End — Batch",
                "nyaa",
                "subsplease",
            ),
            ("Dune: Part Two 4K DV HDR10", "x1337-movies", "yts"),
            ("The Bear — Season 3 (1080p)", "eztv", "tpb-tv"),
            (
                "Baldur's Gate 3 (v7 + DLC, Repack)",
                "fitgirl",
                "x1337-movies",
            ),
            ("Oppenheimer (2023) IMAX WEBRip", "bittorrented", "yts"),
        ];
        let mut out = Vec::new();
        for (title, a, b) in PICKS {
            let mut rng = SeededRng::new(seed_from(title) ^ self.seed);
            let hash = hex40(&mut rng);
            let size_bytes = size_for(Kind::Remux, &mut rng);
            let seeders = 5000 + rng.range(0, 9000) as u32;
            let leechers = (f64::from(seeders) * (0.3 + rng.next_f64() * 0.4)) as u32;
            for src in [*a, *b] {
                out.push(TorrentResult {
                    info_hash: hash.clone(),
                    name: (*title).to_string(),
                    size_bytes,
                    seeders,
                    leechers,
                    num_files: Some(4),
                    source: src,
                    magnet: format!("magnet:?xt=urn:btih:{hash}&dn={}", urlenc(title)),
                    added: None,
                });
            }
        }
        out
    }

    /// Deterministic simulated download speed (MiB/s), seeded by the item's
    /// info_hash: the same torrent always downloads at the same rate, so
    /// progress is reproducible in tests and demos (replaces the engine's
    /// stats poll until phase 4).
    pub fn download_speed(&self, id: &str) -> f64 {
        let mut rng = SeededRng::new(seed_from(id) ^ self.seed);
        0.8 + rng.next_f64() * 7.0
    }

    /// Deterministic simulated upload speed (MiB/s) for a seeding item.
    pub fn upload_speed(&self, id: &str) -> f64 {
        let mut rng = SeededRng::new(seed_from(id) ^ self.seed ^ 0x5EED);
        0.3 + rng.next_f64() * 3.0
    }

    /// Deterministic peer count for an active download.
    pub fn peer_count(&self, id: &str) -> u32 {
        4 + (seed_from(id) % 40) as u32
    }
}

// ---------------------------------------------------------------------------
// Deterministic primitives
// ---------------------------------------------------------------------------

/// Release "kinds" a source can carry. Each kind maps to 1-3 sources, so the
/// same release (same info_hash) appears from multiple sources — the search
/// view's staggered tag set (design.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Remux,
    Webrip,
    Uhd,
    Director,
    Complete,
    Repack,
    Ost,
    Dual,
    Episode,
    Anime,
}

/// Which sources carry each kind, and the order results stream in.
const KIND_SOURCES: &[(Kind, &[&str])] = &[
    (
        Kind::Remux,
        &["yts", "tpb-movies", "x1337-movies", "bittorrented"],
    ),
    (Kind::Webrip, &["yts", "tpb-movies", "bittorrented"]),
    (Kind::Uhd, &["x1337-movies"]),
    (Kind::Director, &["tpb-movies", "x1337-movies"]),
    (Kind::Complete, &["tpb-tv", "x1337-tv"]),
    (Kind::Repack, &["fitgirl"]),
    (Kind::Ost, &["x1337-movies"]),
    (Kind::Dual, &["yts", "bittorrented"]),
    (Kind::Episode, &["eztv", "tpb-tv", "x1337-tv"]),
    (Kind::Anime, &["nyaa", "subsplease"]),
];

/// How many kinds a source carries — its result count when online.
fn kind_count(source_id: &str) -> usize {
    KIND_SOURCES
        .iter()
        .filter(|(_, ids)| ids.contains(&source_id))
        .count()
}

/// Base latency per source (ms) before deterministic jitter: RSS feeds are
/// fast, HTML scrapers slow — matches what the real sources will feel like
/// (docs/sources.md).
fn base_latency_ms(source_id: &str) -> u64 {
    match source_id {
        "eztv" => 180,
        "yts" => 220,
        "nyaa" => 240,
        "subsplease" => 260,
        "tpb-movies" => 300,
        "tpb-tv" => 320,
        "fitgirl" => 650,
        "bittorrented" => 700,
        "x1337-movies" => 850,
        "x1337-tv" => 880,
        _ => 400,
    }
}

/// Per-kind title flavor; `base` is the normalized query.
fn title_for(kind: Kind, base: &str, rng: &mut SeededRng) -> String {
    match kind {
        Kind::Remux => format!("{base} ({}) 1080p REMUX", 2014 + rng.range(0, 12)),
        Kind::Webrip => format!("{base} ({}) 720p WEBRip", 2014 + rng.range(0, 12)),
        Kind::Uhd => format!("{base} 4K DV HDR10"),
        Kind::Director => format!("{base} — Director's Cut"),
        Kind::Complete => format!("{base} — Complete Series"),
        Kind::Repack => format!("{base} (v1.12 + All DLC, Repack)"),
        Kind::Ost => format!("{base} — Original Soundtrack"),
        Kind::Dual => format!("{base} [ENG/JP] Dual Audio"),
        Kind::Episode => format!("{base} S01E{:02}", 1 + rng.range(0, 12)),
        Kind::Anime => format!("[SubsPlease] {base} — 01-12 (1080p) [Batch]"),
    }
}

/// Per-kind size in GB before jitter.
fn size_for(kind: Kind, rng: &mut SeededRng) -> u64 {
    let gb = match kind {
        Kind::Remux => 20.0,
        Kind::Webrip => 2.5,
        Kind::Uhd => 55.0,
        Kind::Director => 28.0,
        Kind::Complete => 30.0,
        Kind::Repack => 45.0,
        Kind::Ost => 0.6,
        Kind::Dual => 12.0,
        Kind::Episode => 1.2,
        Kind::Anime => 8.0,
    };
    (gb * 1024.0 * 1024.0 * 1024.0 * (0.6 + rng.next_f64() * 0.8)) as u64
}

/// Per-kind seeder base before jitter.
fn seeders_for(kind: Kind, rng: &mut SeededRng) -> u32 {
    let base = match kind {
        Kind::Remux => 8000,
        Kind::Webrip => 2000,
        Kind::Uhd => 4000,
        Kind::Director => 1500,
        Kind::Complete => 900,
        Kind::Repack => 12000,
        Kind::Ost => 300,
        Kind::Dual => 1200,
        Kind::Episode => 2500,
        Kind::Anime => 6000,
    };
    (f64::from(base) * (0.7 + rng.next_f64() * 0.6)) as u32
}

/// Normalizes a query for title weaving: trimmed, whitespace collapsed,
/// capitalized. Empty queries use "harbour" (the curated path never calls
/// this with an empty query, so this is just a safety net).
fn normalize(query: &str) -> String {
    let joined = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return "harbour".to_string();
    }
    let mut chars = joined.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase()).unwrap_or('H');
    let rest: String = chars.collect();
    format!("{first}{rest}")
}

/// Percent-encodes the parts of a name that would break a magnet URI.
fn urlenc(s: &str) -> String {
    s.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('—', "%E2%80%94")
        .replace('\'', "%27")
}

/// FNV-1a — a fast, stable string hash used to seed per-query randomness.
fn seed_from(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 40 lowercase hex chars — a BitTorrent infohash (types.rs `InfoHash`).
fn hex40(rng: &mut SeededRng) -> String {
    let mut s = String::with_capacity(40);
    for _ in 0..40 {
        s.push(char::from_digit(rng.range(0, 16) as u32, 16).expect("hex digit"));
    }
    s
}

/// Tiny xorshift64* PRNG. Seeded per (seed, query[, source]) so every draw
/// sequence is reproducible; the app never reads the system clock here.
pub(crate) struct SeededRng(u64);

impl SeededRng {
    pub(crate) fn new(seed: u64) -> Self {
        // Zero would stick the xorshift in a fixed point; nudge it.
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub(crate) fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `[lo, hi)`.
    pub(crate) fn range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(hi > lo);
        lo + self.next_u64() % (hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_deterministic_and_well_formed() {
        let engine = FakeEngine::new();
        let a = engine.plan("elden ring");
        let b = engine.plan("elden ring");
        assert_eq!(a, b, "same query, same plan");

        assert_eq!(a.per_source.len(), SOURCES.len(), "all 10 sources");
        let offline = a
            .per_source
            .iter()
            .filter(|p| p.status == SourceStatus::Offline)
            .count();
        let empty = a
            .per_source
            .iter()
            .filter(|p| p.status == SourceStatus::Empty)
            .count();
        assert_eq!(offline, 1, "exactly one source offline");
        assert_eq!(empty, 1, "exactly one source empty");
        for p in &a.per_source {
            assert!(
                (120..=1200).contains(&p.latency_ms),
                "latency {} in range",
                p.latency_ms
            );
            if p.status == SourceStatus::Empty {
                assert_eq!(p.result_count, 0);
            } else if p.status == SourceStatus::Online {
                assert!(p.result_count >= 1, "online sources have results");
            }
        }
    }

    #[test]
    fn results_are_deterministic_and_well_formed() {
        let engine = FakeEngine::new();
        let a = engine.results("dune", "yts");
        let b = engine.results("dune", "yts");
        assert_eq!(a, b, "same query+source, same results");
        assert!(!a.is_empty(), "yts carries at least one kind");
        for r in &a {
            assert_eq!(r.info_hash.len(), 40);
            assert!(r.info_hash.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(
                r.magnet
                    .starts_with(&format!("magnet:?xt=urn:btih:{}&dn=", r.info_hash)),
                "magnet embeds the hash"
            );
            assert!(r.size_bytes > 0);
        }
    }

    #[test]
    fn shared_kind_dedupes_across_sources() {
        // Remux appears on yts and tpb-movies with the same info_hash, so the
        // search view can merge them into one row with two source tags.
        let engine = FakeEngine::new();
        let yts = engine.results("interstellar", "yts");
        let tpb = engine.results("interstellar", "tpb-movies");
        let yts_remux = yts
            .iter()
            .find(|r| r.name.contains("REMUX"))
            .expect("yts remux");
        let tpb_remux = tpb
            .iter()
            .find(|r| r.name.contains("REMUX"))
            .expect("tpb remux");
        assert_eq!(yts_remux.info_hash, tpb_remux.info_hash);
        assert_eq!(yts_remux.name, tpb_remux.name);
        assert_ne!(yts_remux.source, tpb_remux.source);
    }

    #[test]
    fn curated_is_deterministic_and_tagged() {
        let engine = FakeEngine::new();
        let a = engine.curated();
        let b = engine.curated();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "8 picks x 2 sources");
        let mut unique = std::collections::HashSet::new();
        for r in &a {
            unique.insert(r.info_hash.clone());
        }
        assert_eq!(unique.len(), 8, "each pick is one deduped row");
    }

    #[test]
    fn different_queries_differ() {
        let engine = FakeEngine::new();
        assert_ne!(
            engine.results("elden ring", "yts"),
            engine.results("dune", "yts"),
            "query changes the stream"
        );
    }

    #[test]
    fn seed_changes_the_plan() {
        // `with_seed` is the knob tests/demos use to get a different (still
        // deterministic) search without touching the shared default.
        let a = FakeEngine::with_seed(1).plan("dune");
        let b = FakeEngine::with_seed(2).plan("dune");
        assert_ne!(a, b, "a different seed changes offline/empty/latency");
        let again = FakeEngine::with_seed(1).plan("dune");
        assert_eq!(a, again, "same seed, same plan");
    }
}
