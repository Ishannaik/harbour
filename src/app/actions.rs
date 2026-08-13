//! Download/queue actions and engine-event application: selection moves,
//! download/retry/remove/pause, magnet resolution and enqueueing, ledger
//! persistence, and folding engine events into the UI state.

use std::time::Instant;

use crate::core::paths;
use crate::core::types::{EngineEvent, QueueStatus, SearchCtx, SourceStatus, TorrentResult};
use crate::queue::{AddInput, AddOutcome};
use crate::ui::Screen;

use super::{App, now_ms};

pub(crate) fn move_selection(app: &mut App, delta: isize) {
    let (len, selected) = match app.state.screen {
        // The downloads selection indexes the *visible* tab's rows — the
        // view renders only the active or seeding subset, so a raw items
        // index would highlight an invisible row (and let p/r/x act on one).
        Screen::Downloads => (app.visible_items().len(), &mut app.state.downloads.selected),
        _ => (
            app.state.search.results.len(),
            &mut app.state.search.selected,
        ),
    };
    if len == 0 {
        *selected = 0;
        return;
    }
    // Wrap at both ends: a list you cannot leave by holding a key feels stuck.
    let next = (*selected as isize + delta).rem_euclid(len as isize);
    *selected = next as usize;
}

pub(crate) async fn download_selected(app: &mut App) {
    let Some(result) = app.selected_result().cloned() else {
        app.warn("nothing selected to download");
        return;
    };

    // A row from a detail-page source arrives without a magnet; resolve it now
    // that the user has actually asked for it (`plan-engine.md` T4).
    let magnet = match &result.magnet {
        Some(magnet) => Some(magnet.clone()),
        None => resolve_magnet(app, &result).await,
    };

    let Some(magnet) = magnet else {
        app.warn(format!("could not get a magnet link for {}", result.name));
        return;
    };

    // Re-key on the magnet's own infohash rather than the row's.
    //
    // The detail-page sources (1337x, FitGirl, BitTorrented) cannot know a
    // torrent's real infohash from the list page, so they carry the site's own
    // id in that field as a placeholder until resolution. Enqueuing under the
    // placeholder would file the item under an id the engine never reports
    // back — librqbit keys by the real hash — so the row would sit at 0% for
    // ever while the download actually ran. The magnet is authoritative.
    let id = crate::core::magnet::info_hash_from_magnet(&magnet)
        .unwrap_or_else(|| result.info_hash.clone());

    let outcome = app
        .queue
        .add(
            AddInput {
                id,
                name: result.name.clone(),
                source: Some(result.source),
                magnet: Some(magnet),
                dir: app.config.download_dir.clone(),
                size_bytes: result.size_bytes,
            },
            now_ms(),
        )
        .await;

    match outcome {
        AddOutcome::Duplicate => {
            app.warn(format!("{} is already in your downloads", result.name));
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Started | AddOutcome::Retried => {
            app.state.error_banner = None;
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Queued => app.warn(format!(
            "{} is queued — it starts when a slot frees",
            result.name
        )),
    }
    persist(app);
    app.refresh_downloads();
}

/// Asks the owning source for a magnet it did not supply at search time.
async fn resolve_magnet(app: &App, result: &TorrentResult) -> Option<String> {
    // The registry is a single `HttpSource`; a result's `source` is the *site*
    // it came from (the indexer tags rows with it), so match by id when
    // possible and otherwise fall back to the lone source — the indexer.
    let source = app
        .search
        .sources()
        .iter()
        .find(|s| s.def().id == result.source)
        .or_else(|| app.search.sources().first())?
        .clone();
    let ctx = SearchCtx {
        total_deadline: paths::source_timeout(),
        ..SearchCtx::default()
    };
    source.resolve_magnet(result, &ctx).await.ok()
}

pub(crate) async fn toggle_pause(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    let paused = app
        .queue
        .get(&id)
        .is_some_and(|i| i.status == QueueStatus::Paused);
    let outcome = if paused {
        app.queue.resume(&id, Instant::now()).await
    } else {
        app.queue.pause(&id).await
    };
    if let Err(err) = outcome {
        app.warn(err.to_string());
    }
    persist(app);
    app.refresh_downloads();
}

pub(crate) async fn retry_selected(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    let Some(item) = app.queue.get(&id).cloned() else {
        return;
    };
    if item.status != QueueStatus::Failed {
        return;
    }
    app.queue
        .add(
            AddInput {
                id: item.id.clone(),
                name: item.name.clone(),
                source: item.source,
                magnet: item.magnet.clone(),
                dir: item.dir.clone(),
                size_bytes: item.total_bytes,
            },
            item.added_at_epoch_ms,
        )
        .await;
    persist(app);
    app.refresh_downloads();
}

pub(crate) async fn remove_selected(app: &mut App) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    // Files are never deleted from here: removal forgets the item, and deleting
    // someone's data needs a deliberate, separate confirmation.
    if let Err(err) = app.queue.remove(&id, false).await {
        app.warn(err.to_string());
    }
    persist(app);
    app.refresh_downloads();
}

/// Writes the ledger, surfacing a failure without stopping anything.
fn persist(app: &mut App) {
    if let Err(err) = app.store.save_ledger(app.queue.items()) {
        app.warn(format!("could not save your downloads list: {err}"));
    }
}

/// True while any source is still working.
fn still_searching(app: &App) -> bool {
    app.state
        .search
        .source_health
        .values()
        .any(|s| *s == SourceStatus::Checking)
}

/// Folds one engine or search event into the UI state.
pub(crate) fn apply_event(app: &mut App, event: EngineEvent) {
    match event {
        EngineEvent::SourceStatus { source, status } => {
            app.state.search.source_health.insert(source, status);
        }
        EngineEvent::SourceAnswered { source, count } => {
            app.state.search.source_counts.insert(source, count);
            // Reachable-but-empty is not the same as failed: the dot must say
            // "nothing matched" rather than "this source is down".
            app.state.search.source_health.insert(
                source,
                if count == 0 {
                    SourceStatus::Empty
                } else {
                    SourceStatus::Online
                },
            );
            app.state.search.searching = still_searching(app);
        }
        EngineEvent::SourceResults { source, results } => {
            // A disabled source's late answer is dropped, never merged: the
            // toggle already re-ran the query, so its batch has no place here.
            if !app.disabled_sources.contains(&source) {
                app.partial.insert(source, results);
                app.remerge();
            }
        }
        EngineEvent::SourceFailed {
            source, message, ..
        } => {
            app.state
                .search
                .source_health
                .insert(source, SourceStatus::Offline);
            app.state.search.searching = still_searching(app);
            // One dead source is normal and must not shout at the user — the
            // sidebar dot already says so. Only a total failure earns a banner.
            let probed: Vec<SourceStatus> = app
                .state
                .search
                .source_health
                .values()
                .copied()
                .filter(|s| *s != SourceStatus::Unknown)
                .collect();
            if !probed.is_empty() && probed.iter().all(|s| *s == SourceStatus::Offline) {
                app.warn(format!("every source is unreachable — {message}"));
            }
        }
        EngineEvent::SearchComplete => app.state.search.searching = false,
        EngineEvent::Metadata { .. } | EngineEvent::Progress { .. } => {}
        EngineEvent::Done { .. } => persist(app),
        EngineEvent::Failed { id, message } => {
            let name = item_name(app, &id);
            app.warn(format!("{name}: {message}"));
            persist(app);
        }
        EngineEvent::Missing { id } => {
            let name = item_name(app, &id);
            app.warn(format!(
                "{name}: the downloaded files are gone, so seeding stopped. \
                 Nothing was re-downloaded."
            ));
            persist(app);
        }
    }
}

fn item_name(app: &App, id: &str) -> String {
    app.queue
        .get(id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| id.to_owned())
}

/// Enqueues a magnet handed to us on the command line (`FR-02`).
pub(crate) async fn enqueue_magnet(app: &mut App, magnet: &str) {
    let Some(info_hash) = crate::core::magnet::info_hash_from_magnet(magnet) else {
        app.warn("that magnet link has no usable infohash");
        return;
    };
    app.queue
        .add(
            AddInput {
                id: info_hash.clone(),
                name: info_hash.clone(),
                source: None,
                magnet: Some(magnet.to_owned()),
                dir: app.config.download_dir.clone(),
                size_bytes: 0,
            },
            now_ms(),
        )
        .await;
    app.state.screen = Screen::Downloads;
    persist(app);
    app.refresh_downloads();
}
