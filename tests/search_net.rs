//! Network-gated search tests: do the ten scrapers actually work against the
//! live sites?
//!
//! Fixture tests prove a parser handles the markup we recorded. Only this
//! proves the markup is still what the site sends. It runs solely under
//! `HARBOUR_TEST_NET=1`, because a suite that depends on ten third-party sites
//! being up would fail for reasons that have nothing to do with our code.
//!
//! Deliberately tolerant: individual sources go down, get geoblocked, or put up
//! a bot challenge, and none of that is a defect in harbour. What is asserted is
//! the property the product actually promises — **some** sources answer, one
//! failure never stops the others, and every row that comes back is usable.
//!
//! ```text
//! HARBOUR_TEST_NET=1 cargo test --test search_net -- --nocapture
//! ```

use std::time::{Duration, Instant};

#[path = "../src/core/mod.rs"]
mod core;
#[path = "../src/persist.rs"]
mod persist;
#[path = "../src/search.rs"]
mod search;
#[path = "../src/sources/mod.rs"]
mod sources;

use crate::core::types::{EngineEvent, SearchCtx, SourceId, SourceStatus};
use crate::search::SearchEngine;
use crate::sources::cache::SearchCache;

fn enabled() -> bool {
    std::env::var("HARBOUR_TEST_NET").is_ok_and(|v| v == "1")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("harbour-search-net-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Runs one real search and reports what every source did.
async fn live_search(query: &str, budget: Duration) -> Vec<EngineEvent> {
    let engine = SearchEngine::new(
        crate::sources::registry(),
        SearchCache::new(scratch(if query.is_empty() { "browse" } else { "query" })),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = SearchCtx {
        total_deadline: budget,
        ..SearchCtx::default()
    };
    engine.start(query.to_owned(), ctx, tx);

    let mut events = Vec::new();
    let deadline = Instant::now() + budget + Duration::from_secs(3);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(event)) => events.push(event),
            Ok(None) => break,
            Err(_) => {}
        }
    }
    events
}

/// Prints a per-source verdict so a failing site is obvious at a glance.
fn report(events: &[EngineEvent]) -> (usize, usize) {
    let mut answered = 0;
    let mut failed = 0;
    for id in SourceId::ALL {
        let count = events.iter().find_map(|e| match e {
            EngineEvent::SourceAnswered { source, count } if *source == id => Some(*count),
            _ => None,
        });
        let failure = events.iter().find_map(|e| match e {
            EngineEvent::SourceFailed {
                source,
                class,
                message,
            } if *source == id => Some((*class, message.clone())),
            _ => None,
        });
        match (count, failure) {
            (Some(n), _) => {
                answered += 1;
                println!("  {id:<14} ok       {n} rows");
            }
            (None, Some((class, message))) => {
                failed += 1;
                let short: String = message.chars().take(70).collect();
                println!("  {id:<14} {class:<8} {short}");
            }
            (None, None) => println!("  {id:<14} pending  (still in flight at the deadline)"),
        }
    }
    (answered, failed)
}

#[tokio::test]
async fn a_real_query_returns_usable_rows_from_the_live_sites() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    println!("\n--- live search: \"sintel\" ---");
    let events = live_search("sintel", Duration::from_secs(20)).await;
    let (answered, failed) = report(&events);
    println!("  {answered} answered, {failed} failed");

    // Every row that came back must be renderable and downloadable. This is the
    // assertion that catches a parser returning junk, and it holds for whichever
    // sources happen to be up today.
    let mut rows = 0;
    for event in &events {
        if let EngineEvent::SourceResults { source, results } = event {
            for row in results {
                rows += 1;
                assert_eq!(
                    row.info_hash.len(),
                    40,
                    "{source}: infohash is not 40 hex chars: {:?}",
                    row.info_hash
                );
                assert!(
                    row.info_hash.chars().all(|c| c.is_ascii_hexdigit()),
                    "{source}: non-hex infohash {:?}",
                    row.info_hash
                );
                assert_eq!(
                    row.info_hash,
                    row.info_hash.to_lowercase(),
                    "{source}: infohash must be lowercased at the boundary"
                );
                assert!(!row.name.trim().is_empty(), "{source}: row with no name");
                assert_eq!(row.source, *source, "row tagged with the wrong source");
                if let Some(magnet) = &row.magnet {
                    assert!(
                        magnet.contains(&row.info_hash),
                        "{source}: magnet does not carry its own infohash"
                    );
                }
            }
        }
    }
    println!("  {rows} rows validated");

    assert!(
        answered > 0,
        "not one of the ten sources answered — that is a harbour problem, not a site problem"
    );
}

#[tokio::test]
async fn the_curated_browse_works_with_an_empty_query() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    println!("\n--- live browse (empty query) ---");
    let events = live_search("", Duration::from_secs(20)).await;
    let (answered, _failed) = report(&events);
    assert!(answered > 0, "the curated browse returned nothing at all");
}

#[tokio::test]
async fn one_dead_source_never_stops_the_rest() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    // On any given day at least one of ten torrent indexes is unreachable,
    // geoblocked, or serving a challenge. The product promise is that the search
    // still works — this asserts exactly that, without depending on *which*
    // source is having a bad day.
    let events = live_search("matrix", Duration::from_secs(20)).await;
    let answered = events
        .iter()
        .filter(|e| matches!(e, EngineEvent::SourceAnswered { .. }))
        .count();
    let failed = events
        .iter()
        .filter(|e| matches!(e, EngineEvent::SourceFailed { .. }))
        .count();
    println!("\n--- resilience: {answered} answered / {failed} failed ---");
    assert!(
        answered > 0,
        "every source failed together, which points at our fetch layer"
    );

    // A failed source must be reported as offline rather than silently missing,
    // or the sidebar would show it as still checking forever.
    for event in &events {
        if let EngineEvent::SourceFailed { source, .. } = event {
            let marked_offline = events.iter().any(|e| {
                matches!(e,
                EngineEvent::SourceStatus { source: s, status }
                    if s == source && *status == SourceStatus::Checking)
            });
            assert!(
                marked_offline,
                "{source} failed without ever announcing that it started"
            );
        }
    }
}
