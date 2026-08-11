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

use crate::types::{HistoryItem, QueueItem, QueueStatus, SourceId, TorrentResult};

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

/// Deterministic fake search results for `query`: 8–12 hits across the
/// sidebar sources (fitgirl, yts, tpb-movies, x1337-movies, eztv, tpb-tv,
/// nyaa, subsplease, bittorrented), seeded by the query so re-searching the
/// same term shows the same list (smoke tests can depend on it). Names look
/// like "{query} (2026) [1080p] [WEBRip]"; sizes span 700 MiB–60 GiB;
/// seeders 3–9000; leechers 1–300; each magnet is built from a local
/// 40-hex hash derived from the same seed.
pub fn fake_results(query: &str) -> Vec<TorrentResult> {
    let seed = seed_from(query);
    let mut rng = Rng(seed);
    // Hit count is part of the seeded output: 8..=12, cycling the source
    // matrix so larger counts give the leading sources a second hit.
    let count = 8 + (seed % 5) as usize;
    (0..count)
        .map(|i| {
            let (id, _label, suffix, min_size, max_size) = SOURCES[i % SOURCES.len()];
            // Unique infohash per hit: golden-ratio mix of the seed and the
            // index spreads hits apart without consuming RNG state.
            let info_hash = hash40(seed ^ (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let name = format!("{query} {suffix}");
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
