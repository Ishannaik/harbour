//! Network-gated engine integration tests — the E1 behavioural spike
//! (`docs/plan-engine.md` §5).
//!
//! These are the questions static analysis cannot answer: does metadata
//! actually arrive from a swarm, does a paused torrent really report no peers,
//! does the session restore without a rehash. They talk to the live BitTorrent
//! network, so they run only when `HARBOUR_TEST_NET=1` and are skipped
//! everywhere else — CI included, since a test that depends on strangers'
//! seeders is not a test, it is a coin flip.
//!
//! Run them with:
//!
//! ```text
//! HARBOUR_TEST_NET=1 cargo test --test engine_net -- --nocapture
//! ```
//!
//! The torrent used is Sintel, the Blender Foundation's Creative Commons open
//! movie — a legitimately redistributable file with a long-lived public swarm,
//! chosen so this suite never depends on infringing content.

use std::path::PathBuf;
use std::time::{Duration, Instant};

// The crate is a binary, so the integration test cannot `use harbour::…`.
// Including the modules directly keeps this test honest — it exercises the real
// adapter rather than a copy — at the cost of a little wiring.
#[path = "../src/core/mod.rs"]
mod core;
#[path = "../src/engine/mod.rs"]
mod engine;
#[path = "../src/persist.rs"]
mod persist;
#[path = "../src/queue.rs"]
mod queue;

use crate::core::types::{Engine, EngineItemState, QueueStatus};
use crate::engine::rqbit::{EngineLaunchOptions, RqbitEngine};
use crate::queue::{AddInput, Queue};

const SINTEL: &str = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10\
&dn=Sintel\
&tr=udp%3A%2F%2Fexplodie.org%3A6969\
&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce\
&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce";
const SINTEL_HASH: &str = "08ada5a7a6183aae1e09d831df6748d566095a10";

fn enabled() -> bool {
    std::env::var("HARBOUR_TEST_NET").is_ok_and(|v| v == "1")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harbour-net-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Polls until `f` holds or the budget expires. Returns whether it held.
async fn wait_for(budget: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    f()
}

/// E1 gate 1: a magnet resolves to metadata from a live swarm.
#[tokio::test]
async fn metadata_arrives_from_a_real_swarm() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    let root = scratch("meta");
    let engine = RqbitEngine::new(
        &root.join("downloads"),
        &root,
        &EngineLaunchOptions::default(),
    )
    .await
    .expect("session starts");

    engine
        .add(crate::core::types::AddRequest {
            id: SINTEL_HASH.into(),
            magnet: SINTEL.into(),
            dir: root.join("downloads"),
            trackers: Vec::new(),
        })
        .await
        .expect("add accepted");

    let got_metadata = wait_for(Duration::from_secs(90), || {
        engine
            .snapshot()
            .first()
            .is_some_and(|s| s.stats.total_bytes > 0 && s.name.is_some())
    })
    .await;

    let snap = engine.snapshot().into_iter().next().expect("one torrent");
    assert!(
        got_metadata,
        "no metadata after 90s — state {:?}, error {:?}",
        snap.state, snap.error
    );
    println!(
        "metadata: name={:?} total={} state={:?}",
        snap.name, snap.stats.total_bytes, snap.state
    );

    // The .torrent bytes must be capturable for the local re-seed cache (FR-37).
    let bytes = engine.torrent_bytes(SINTEL_HASH);
    assert!(
        bytes.as_ref().is_some_and(|b| !b.is_empty()),
        "metadata arrived but the .torrent bytes were not capturable"
    );

    engine.shutdown().await;
}

/// E1 gate 4: pause and resume round-trip, and a paused torrent reports no
/// peers rather than zero — the contract `EngineStats::peers` is built on.
#[tokio::test]
async fn pause_reports_unknown_peers_not_zero() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    let root = scratch("pause");
    let engine = RqbitEngine::new(
        &root.join("downloads"),
        &root,
        &EngineLaunchOptions::default(),
    )
    .await
    .expect("session starts");
    engine
        .add(crate::core::types::AddRequest {
            id: SINTEL_HASH.into(),
            magnet: SINTEL.into(),
            dir: root.join("downloads"),
            trackers: Vec::new(),
        })
        .await
        .expect("add accepted");

    wait_for(Duration::from_secs(90), || {
        engine
            .snapshot()
            .first()
            .is_some_and(|s| s.state == EngineItemState::Live)
    })
    .await;

    engine.pause(SINTEL_HASH).await.expect("pause");
    let paused = wait_for(Duration::from_secs(20), || {
        engine
            .snapshot()
            .first()
            .is_some_and(|s| s.state == EngineItemState::Paused)
    })
    .await;
    assert!(paused, "torrent did not reach the paused state");

    let snap = engine.snapshot().into_iter().next().expect("one torrent");
    assert_eq!(
        snap.stats.peers, None,
        "a paused torrent must report peers as unknown, not as 0"
    );
    assert_eq!(snap.stats.speed_mib, 0.0);

    engine.resume(SINTEL_HASH).await.expect("resume");
    let resumed = wait_for(Duration::from_secs(30), || {
        engine
            .snapshot()
            .first()
            .is_some_and(|s| s.state != EngineItemState::Paused)
    })
    .await;
    assert!(resumed, "torrent did not come back after resume");

    engine.shutdown().await;
}

/// E1 gate 2: a restarted session restores its torrent from its own
/// persistence, without being re-added and without rehashing from scratch.
#[tokio::test]
async fn a_restarted_session_restores_its_torrents() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    let root = scratch("restore");
    let downloads = root.join("downloads");

    {
        let engine = RqbitEngine::new(&downloads, &root, &EngineLaunchOptions::default())
            .await
            .expect("session");
        engine
            .add(crate::core::types::AddRequest {
                id: SINTEL_HASH.into(),
                magnet: SINTEL.into(),
                dir: downloads.clone(),
                trackers: Vec::new(),
            })
            .await
            .expect("add accepted");
        wait_for(Duration::from_secs(90), || {
            engine
                .snapshot()
                .first()
                .is_some_and(|s| s.stats.total_bytes > 0)
        })
        .await;
        engine.shutdown().await;
    }

    // Fresh session over the same state directory.
    let engine = RqbitEngine::new(&downloads, &root, &EngineLaunchOptions::default())
        .await
        .expect("session");
    let adopted = engine.adopt_restored();
    assert!(
        adopted.iter().any(|h| h == SINTEL_HASH),
        "the restarted session did not restore the torrent: {adopted:?}"
    );
    println!("restored {} torrent(s) from persistence", adopted.len());
    engine.shutdown().await;
}

/// The queue driving the real engine end to end, rather than the fake.
#[tokio::test]
async fn the_queue_drives_the_real_engine() {
    if !enabled() {
        eprintln!("skipped: set HARBOUR_TEST_NET=1 to run network tests");
        return;
    }
    let root = scratch("queue");
    let downloads = root.join("downloads");
    let engine = std::sync::Arc::new(
        RqbitEngine::new(&downloads, &root, &EngineLaunchOptions::default())
            .await
            .expect("session starts"),
    );
    let mut queue = Queue::new(engine.clone(), 0);

    queue
        .add(
            AddInput {
                id: SINTEL_HASH.into(),
                name: "Sintel".into(),
                source: None,
                magnet: Some(SINTEL.into()),
                bytes: None,
                dir: downloads,
                size_bytes: 0,
            },
            1_786_000_000_000,
        )
        .await;

    assert_eq!(
        queue.get(SINTEL_HASH).map(|i| i.status),
        Some(QueueStatus::Downloading),
        "the queue should have started it"
    );

    // Tick as the app does and confirm the projection produces sane rows.
    let mut saw_metadata = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && !saw_metadata {
        queue.tick(Instant::now()).await;
        saw_metadata = queue.get(SINTEL_HASH).is_some_and(|i| i.total_bytes > 0);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(saw_metadata, "the queue never observed metadata");

    let view = queue
        .views()
        .into_iter()
        .find(|v| v.item.id == SINTEL_HASH)
        .expect("one view");
    println!(
        "queue view: name={:?} progress={:.3} peers={:?} speed={:.2} MiB/s",
        view.item.name,
        view.progress(),
        view.peers(),
        view.speed_mib()
    );
    assert!(view.total_bytes() > 0);
    assert_eq!(view.item.status, QueueStatus::Downloading);

    engine.shutdown().await;
}
