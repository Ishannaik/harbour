//! Download/queue actions and engine-event application: selection moves,
//! download/retry/remove/pause, magnet resolution and enqueueing, ledger
//! persistence, and folding engine events into the UI state.

use std::time::Instant;

use crate::core::paths;
use crate::core::types::{EngineEvent, QueueStatus, SearchCtx, SourceStatus, TorrentResult};
use crate::queue::{AddInput, AddOutcome, DeleteBlock};
use crate::ui::{ConfirmAction, ConfirmPrompt, Screen};

use super::{App, now_ms};

pub(crate) fn move_selection(app: &mut App, delta: isize) {
    if app.state.screen == Screen::Search {
        // If focus was on the search input and user moves Down, blur search and focus results
        if app.state.search.focus && delta > 0 {
            if !app.state.search.results.is_empty() {
                app.state.search.focus = false;
                app.state.search.selected = 0;
            }
            return;
        }
        // If focus is on the results list at top and user moves Up, focus back to search input
        if !app.state.search.focus && delta < 0 && app.state.search.selected == 0 {
            app.state.search.focus = true;
            return;
        }
    }

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
    // Clamp at list boundaries: scrolling stops at top and bottom rather than looping.
    let cur = *selected as isize;
    let next = (cur + delta).clamp(0, (len as isize).saturating_sub(1));
    *selected = next as usize;
}

pub(crate) fn move_selection_to(app: &mut App, target: usize) {
    if app.state.screen == Screen::Search && app.state.search.focus {
        app.state.search.focus = false;
    }
    let (len, selected) = match app.state.screen {
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
    *selected = target.min(len.saturating_sub(1));
}

/// Approximate number of visible rows in the main results pane.
pub(crate) fn page_size() -> usize {
    let (_, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
    (term_h as usize).saturating_sub(8).max(5)
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

    // Check if torrent contains multiple files (e.g. season pack / batch release)
    let video_files = app.queue.engine().list_video_files(&id).await;
    if video_files.len() > 1 {
        app.batch_picker.open_for(
            id,
            result.name.clone(),
            Some(magnet),
            app.config.download_dir.clone(),
            video_files,
        );
        return;
    }

    let outcome = app
        .queue
        .add(
            AddInput {
                id,
                name: result.name.clone(),
                source: Some(result.source),
                magnet: Some(magnet),
                bytes: None,
                dir: app.config.download_dir.clone(),
                size_bytes: result.size_bytes,
                only_files: None,
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

/// Confirms selection in batch picker and begins downloading chosen files.
pub(crate) async fn confirm_batch_download(app: &mut App) {
    if !app.batch_picker.open {
        return;
    }
    let id = app.batch_picker.torrent_id.clone();
    let name = app.batch_picker.torrent_name.clone();
    let magnet = app.batch_picker.magnet.clone();
    let dir = app.batch_picker.dir.clone();
    let checked = app.batch_picker.checked.clone();
    let size = app.batch_picker.selected_size_bytes();
    let total_files = app.batch_picker.files.len();
    app.batch_picker.open = false;

    if checked.is_empty() {
        app.warn("no files selected to download");
        return;
    }

    let only_files = if checked.len() < total_files {
        Some(checked)
    } else {
        None
    };

    let outcome = app
        .queue
        .add(
            AddInput {
                id,
                name: name.clone(),
                source: None,
                magnet,
                bytes: None,
                dir,
                size_bytes: size,
                only_files,
            },
            now_ms(),
        )
        .await;

    match outcome {
        AddOutcome::Duplicate => {
            app.warn(format!("{name} is already in your downloads"));
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Started | AddOutcome::Retried => {
            app.state.error_banner = None;
            app.state.screen = Screen::Downloads;
        }
        AddOutcome::Queued => app.warn(format!(
            "{name}: queued — starts when a download slot frees"
        )),
    }
    persist(app);
    app.refresh_downloads();
}

/// Asks the owning source for a magnet it did not supply at search time.
pub(crate) async fn resolve_magnet(app: &App, result: &TorrentResult) -> Option<String> {
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
    // A `Missing` item re-checks exactly like a failed one retries (FR-46):
    // re-adding it restarts the torrent against whatever is on disk.
    if !matches!(item.status, QueueStatus::Failed | QueueStatus::Missing) {
        return;
    }
    app.queue
        .add(
            AddInput {
                id: item.id.clone(),
                name: item.name.clone(),
                source: item.source,
                magnet: item.magnet.clone(),
                bytes: item.bytes.clone(),
                dir: item.dir.clone(),
                size_bytes: item.total_bytes,
                only_files: item.only_files.clone(),
            },
            item.added_at_epoch_ms,
        )
        .await;
    persist(app);
    app.refresh_downloads();
}

pub(crate) fn open_remove_confirm(app: &mut App, delete_files: bool) {
    let Some(id) = app.selected_item_id() else {
        return;
    };
    let Some(item) = app.queue.get(&id) else {
        return;
    };
    app.confirm = if delete_files {
        ConfirmPrompt::delete_files(&item.name, &item.dir, id)
    } else {
        ConfirmPrompt::forget(&item.name, id)
    };
}

pub(crate) async fn apply_confirm(app: &mut App, accept: bool) {
    let pending = app.confirm.on_confirm.take();
    app.confirm = ConfirmPrompt::default();
    if !accept {
        return;
    }
    let Some(action) = pending else {
        return;
    };
    match action {
        ConfirmAction::Forget { id } => remove_item(app, &id, false).await,
        ConfirmAction::ForgetAndDelete { id } => remove_item(app, &id, true).await,
        ConfirmAction::ClearCache => run_clear_cache(app),
        ConfirmAction::Quit => app.quitting = true,
    }
}

async fn remove_item(app: &mut App, id: &str, delete_files: bool) {
    let mut delete_files = delete_files;
    if delete_files {
        if app.state.now_playing.as_ref().is_some_and(|np| np.id == id) {
            app.warn("kept the files — that download is playing");
            delete_files = false;
        } else if app.queue.delete_files_blocked_by(id) == Some(DeleteBlock::SharedDirectory) {
            app.warn("kept the files — another download uses that folder");
            delete_files = false;
        }
        // Missing: Queue::remove skips deletion silently (FR-79).
    }
    if let Err(err) = app.queue.remove(id, delete_files).await {
        app.warn(err.to_string());
        return;
    }
    // Watch-now scratch for this hash is cache, not the download dir.
    if let Err(err) = app.store.remove_watch_cache(id) {
        app.warn(format!(
            "could not clear leftover cache for this torrent: {err}"
        ));
    }
    persist(app);
    app.refresh_downloads();
}

/// Opens the clear-cache confirm (settings row or shift+C). Defaults to No.
pub(crate) fn open_clear_cache_confirm(app: &mut App) {
    app.confirm = ConfirmPrompt::clear_cache();
}

/// Opens the quit confirm (`q`, FR-81). Defaults to No.
pub(crate) fn open_quit_confirm(app: &mut App) {
    app.confirm = ConfirmPrompt::quit();
}

/// First `q` opens the overlay; a second `q` while it is already the quit
/// confirm really leaves. `q` on a forget/cache overlay replaces it.
pub(crate) async fn request_quit(app: &mut App) {
    if app.confirm.open && matches!(app.confirm.on_confirm, Some(ConfirmAction::Quit)) {
        apply_confirm(app, true).await;
        return;
    }
    open_quit_confirm(app);
}

/// Search JSON + unused .torrent files + leftover watch scratch.
/// Download-dir files and the ledger stay. Active watch-now scratch is kept
/// so a ledger-free stream is not deleted out from under the player.
fn run_clear_cache(app: &mut App) {
    let mut keep: std::collections::HashSet<String> = app
        .queue
        .items()
        .iter()
        .map(|item| item.id.clone())
        .collect();
    if let Some(np) = &app.state.now_playing {
        keep.insert(np.id.clone());
    }
    // Drop memory hits first: the disk search cache is about to go, and a
    // later torrent/watch wipe failure must not keep serving stale results.
    app.query_cache.clear();
    match app.store.clear_cache(&keep) {
        Ok(freed) => app.warn(format!(
            "cache cleared — {} freed",
            crate::persist::format_bytes(freed)
        )),
        Err(err) => app.warn(format!("could not clear cache: {err}")),
    }
}

/// Writes the ledger, surfacing a failure without stopping anything.
fn persist(app: &mut App) {
    if let Err(err) = app.store.save_ledger(app.queue.items()) {
        app.warn(format!("could not save your downloads list: {err}"));
    }
}

/// Copy per-site dots off a batch Indexer answer (FR-15/18).
fn paint_indexer_site_dots(app: &mut App, source: crate::core::types::SourceId) {
    if source != crate::core::types::SourceId::Indexer {
        return;
    }
    for (site, (status, count)) in app.search.reported_source_health() {
        if status == SourceStatus::Unknown {
            continue;
        }
        app.state.search.source_health.insert(site, status);
        app.state.search.source_counts.insert(site, count as usize);
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
            // Batch `/search` answers as the Indexer proxy: fold the report
            // for this query only. Per-site stream lines must not replay
            // leftover Empty/Offline from the previous search.
            paint_indexer_site_dots(app, source);
            app.state.search.searching = still_searching(app);
        }
        EngineEvent::SourceResults { source, results } => {
            // A disabled source's late answer is dropped, never merged: the
            // toggle already filtered it, so its batch has no place here.
            if !app.disabled_sources.contains(&source) {
                merge_source_results(app, source, results);
            }
        }
        EngineEvent::SourceFailed {
            source, message, ..
        } => {
            log_to_file(&format!(
                "[SOURCE FAILED] source={:?} error='{}'",
                source, message
            ));
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
        EngineEvent::SearchComplete => {
            app.state.search.searching = false;
            let elapsed_ms = app
                .state
                .search
                .search_started
                .map(|s| s.elapsed().as_millis() as u64)
                .unwrap_or(0);
            app.state.search.latency_ms = Some(elapsed_ms);
            log_to_file(&format!(
                "[SEARCH COMPLETE] query='{}' total_results={} elapsed={}ms",
                app.state.search.query,
                app.state.search.results.len(),
                elapsed_ms
            ));
        }
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

fn merge_source_results(
    app: &mut App,
    source: crate::core::types::SourceId,
    results: Vec<crate::core::types::TorrentResult>,
) {
    if source == crate::core::types::SourceId::Indexer {
        for r in results {
            if !app.disabled_sources.contains(&r.source) {
                app.partial.entry(r.source).or_default().push(r);
            }
        }
    } else {
        app.partial.insert(source, results);
    }
    app.remerge();
    let query = if app.state.search.browsing {
        String::new()
    } else {
        app.state.search.query.clone()
    };
    app.query_cache.insert(
        query,
        (
            std::time::Instant::now(),
            app.state.search.results.clone(),
            app.state.search.source_counts.clone(),
            app.state.search.source_health.clone(),
        ),
    );
    if let Some(started) = app.state.search.search_started {
        let ms = started.elapsed().as_millis() as u64;
        app.state.search.latency_ms = Some(ms);
        log_to_file(&format!(
            "search query='{}' finished in {}ms with {} results",
            app.state.search.query,
            ms,
            app.state.search.results.len()
        ));
    }
}

pub(crate) fn log_to_file(line: &str) {
    let log_path = crate::core::paths::state_dir().join("harbour.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "[{ts}] {line}");
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
                bytes: None,
                dir: app.config.download_dir.clone(),
                size_bytes: 0,
                only_files: None,
            },
            now_ms(),
        )
        .await;
    app.state.screen = Screen::Downloads;
    persist(app);
    app.refresh_downloads();
}

/// Enqueues a `.torrent` file handed to us on the command line
/// (`FR-02`/`FR-39`).
///
/// The infohash lives inside the file, so the engine parses it before the
/// queue sees it: the item is keyed by the real hash from the start, which is
/// what keeps FR-56 dedupe and the engine poll honest. An unreadable or
/// unparseable file is user error and gets a loud banner, never a silent no-op.
pub(crate) async fn enqueue_torrent(app: &mut App, path: &std::path::Path) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            app.warn(format!("could not read {}: {err}", path.display()));
            return;
        }
    };
    let Some(info_hash) = app.queue.engine().torrent_info_hash(&bytes) else {
        app.warn(format!(
            "{} is not a readable .torrent file",
            path.display()
        ));
        return;
    };
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| info_hash.clone());
    app.queue
        .add(
            AddInput {
                id: info_hash,
                name,
                source: None,
                magnet: None,
                bytes: Some(bytes),
                dir: app.config.download_dir.clone(),
                size_bytes: 0,
                only_files: None,
            },
            now_ms(),
        )
        .await;
    app.state.screen = Screen::Downloads;
    persist(app);
    app.refresh_downloads();
}

/// Clears all completed / seeding items from the queue (files remain on disk).
pub(crate) async fn clear_completed(app: &mut App) {
    let cleared = app.queue.clear_completed().await;
    if !cleared.is_empty() {
        app.refresh_downloads();
        persist(app);
    }
}

/// Opens the selected downloaded item or its directory in the OS file manager.
pub(crate) fn open_selected_item(app: &mut App) {
    let Some(item) = app
        .visible_items()
        .get(app.state.downloads.selected)
        .map(|v| &v.item)
    else {
        return;
    };
    let dir = &item.dir;
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::core::types::{Engine as _, SearchFuture, Source, SourceDef, SourceId};
    use crate::engine::fake::FakeEngine;
    use crate::persist::{Config, Store};
    use crate::queue::Queue;
    use crate::search::SearchEngine;
    use crate::theme::Theme;
    use crate::ui::player::PlayerPicker;
    use crate::ui::settings::SettingsState;
    use crate::ui::{AppState, Screen};

    /// A fully wired App over a FakeEngine, with its state rooted in a scratch
    /// directory so the ledger writes land nowhere near a real profile.
    ///
    /// `label` keeps parallel tests in separate directories — the temp root is
    /// otherwise shared per-process.
    fn test_app(engine: Arc<FakeEngine>, label: &str) -> (App, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "harbour-actions-test-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        let config = Config {
            download_dir: root.join("dl"),
            ..Config::default()
        };
        let app = App {
            state: AppState::default(),
            queue: Queue::new(engine, 0),
            search: SearchEngine::new(vec![]),
            store: Store::new(&root),
            disabled_sources: HashSet::new(),
            config,
            partial: HashMap::new(),
            search_cancel: None,
            events_tx: tokio::sync::mpsc::unbounded_channel().0,
            history: Vec::new(),
            help_open: false,
            confirm: crate::ui::ConfirmPrompt::default(),
            settings_open: false,
            settings: SettingsState::default(),
            theme: Arc::new(Mutex::new(Theme::titanium())),
            watch: None,
            picker: PlayerPicker::default(),
            picker_pending: None,
            episode_picker: crate::ui::EpisodePicker::default(),
            batch_picker: crate::ui::BatchPicker::default(),
            query_cache: HashMap::new(),
            last_search_click: None,
            quitting: false,
        };
        (app, root)
    }

    #[tokio::test]
    async fn download_selected_switches_to_the_downloads_screen() {
        // FR-29 / #71: `d` (and a result double-click, which maps to the
        // same action) must leave search so the user sees the new queue row.
        let engine = Arc::new(FakeEngine::new());
        let (mut app, _root) = test_app(engine, "download-selected-screen");
        let info_hash = "0123456789abcdef0123456789abcdef01234567".to_string();
        app.state.screen = Screen::Search;
        app.state.search.results = vec![TorrentResult {
            info_hash: info_hash.clone(),
            name: "Dune".into(),
            size_bytes: 1_000,
            seeders: 10,
            leechers: 1,
            num_files: None,
            source: SourceId::Yts,
            magnet: Some(crate::core::magnet::build_magnet(&info_hash, "Dune")),
            added: None,
        }];
        app.state.search.selected = 0;

        download_selected(&mut app).await;

        assert_eq!(app.state.screen, Screen::Downloads);
        assert!(
            app.queue.get(&info_hash).is_some(),
            "the selected result is enqueued"
        );
    }

    #[tokio::test]
    async fn a_torrent_file_is_enqueued_from_its_bytes() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "enqueue");
        let file = root.join("movie.torrent");
        std::fs::write(&file, b"d4:infod4:name3:fooe").expect("fixture");

        enqueue_torrent(&mut app, &file).await;

        let hash = engine
            .torrent_info_hash(b"d4:infod4:name3:fooe")
            .expect("the fake parses the payload");
        assert_eq!(app.state.screen, Screen::Downloads);
        let item = app
            .queue
            .get(&hash)
            .expect("enqueued under the file's own infohash");
        assert_eq!(item.name, "movie", "named from the file stem");
        assert_eq!(
            item.status,
            QueueStatus::Downloading,
            "FR-02/.39: a launch .torrent starts immediately"
        );
        assert!(
            engine.contains(&hash),
            "the engine keys the torrent identically"
        );
    }

    #[tokio::test]
    async fn forgetting_a_partial_download_stops_writing_and_drops_watch_scratch() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "forget-watch-scratch");
        let id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();
        let dl = root.join("dl");
        std::fs::create_dir_all(&dl).expect("dl");
        std::fs::write(dl.join("video.mkv"), b"keep").expect("file");
        let watch = crate::core::paths::watch_cache_dir(&root, &id).expect("hash");
        std::fs::create_dir_all(&watch).expect("watch");
        std::fs::write(watch.join("scratch"), b"tmp").expect("scratch");

        app.queue
            .add(
                AddInput {
                    id: id.clone(),
                    name: "Partial".into(),
                    source: Some(SourceId::Yts),
                    magnet: Some(format!("magnet:?xt=urn:btih:{id}")),
                    bytes: None,
                    dir: dl.clone(),
                    size_bytes: 1000,
                    only_files: None,
                },
                1,
            )
            .await;
        engine.set_progress(&id, 0.10, 3.0);
        app.refresh_downloads();
        app.state.screen = Screen::Downloads;
        app.state.downloads.selected = 0;

        open_remove_confirm(&mut app, false);
        apply_confirm(&mut app, true).await;

        assert!(app.queue.get(&id).is_none(), "the ledger row is gone");
        assert!(!engine.is_writing(&id), "the engine stopped writing");
        assert!(
            dl.join("video.mkv").exists(),
            "download-dir files stay (#77)"
        );
        assert!(!watch.exists(), "watch-now scratch for this hash is cache");
    }

    #[tokio::test]
    async fn clear_cache_confirm_defaults_to_no_and_yes_reports_bytes_freed() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine, "clear-cache-banner");
        let keep = "ffffffffffffffffffffffffffffffffffffffff";
        let drop = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let search = crate::core::paths::search_cache_dir(&root, SourceId::Yts).join("q.json");
        let unused = crate::core::paths::torrent_cache_file(&root, drop).expect("hash");
        std::fs::create_dir_all(search.parent().expect("parent")).expect("dir");
        std::fs::write(&search, b"cached-search").expect("search");
        std::fs::create_dir_all(unused.parent().expect("parent")).expect("dir");
        std::fs::write(&unused, b"unused-torrent").expect("torrent");
        app.store
            .save_ledger(&[crate::core::types::QueueItem::new(
                keep.into(),
                "Keep".into(),
                Some(SourceId::Yts),
                None,
                root.join("dl"),
                1,
            )])
            .expect("ledger");

        open_clear_cache_confirm(&mut app);
        assert!(app.confirm.open);
        assert!(!app.confirm.yes_selected, "loud confirm defaults to No");

        apply_confirm(&mut app, true).await;

        assert!(!app.confirm.open);
        let banner = app.state.error_banner.as_deref().unwrap_or("");
        assert!(
            banner.contains("freed"),
            "the banner reports bytes freed, got: {banner}"
        );
        assert!(!search.exists());
        assert!(!unused.exists());
        assert_eq!(
            app.store.load_ledger().value().len(),
            1,
            "the ledger is untouched"
        );
    }

    #[tokio::test]
    async fn q_opens_quit_confirm_and_no_is_a_noop() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, _root) = test_app(engine, "quit-confirm-open");
        request_quit(&mut app).await;
        assert!(app.confirm.open);
        assert!(!app.confirm.yes_selected, "FR-81: default highlight is No");
        assert!(!app.quitting, "q alone must not leave");
        apply_confirm(&mut app, false).await;
        assert!(!app.confirm.open);
        assert!(!app.quitting);
    }

    #[tokio::test]
    async fn confirming_quit_or_a_second_q_leaves() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, _root) = test_app(engine, "quit-confirm-yes");
        request_quit(&mut app).await;
        apply_confirm(&mut app, true).await;
        assert!(app.quitting);

        let engine = Arc::new(FakeEngine::new());
        let (mut app, _root) = test_app(engine, "quit-confirm-qq");
        request_quit(&mut app).await;
        request_quit(&mut app).await;
        assert!(app.quitting, "qq confirms Yes");
        assert!(!app.confirm.open);
    }

    #[tokio::test]
    async fn clear_cache_keeps_an_active_ephemeral_watch_dir() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine, "clear-cache-keep-watch");
        let id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let watch = crate::core::paths::watch_cache_dir(&root, id).expect("hash");
        std::fs::create_dir_all(&watch).expect("watch");
        std::fs::write(watch.join("piece"), b"live").expect("piece");
        app.state.now_playing = Some(crate::ui::NowPlaying {
            id: id.into(),
            name: "Live".into(),
            stream_url: "http://127.0.0.1/x".into(),
            ephemeral: true,
            subtitle: None,
        });

        open_clear_cache_confirm(&mut app);
        apply_confirm(&mut app, true).await;

        assert!(
            watch.join("piece").exists(),
            "active watch-now scratch must survive clear-cache"
        );
    }

    #[tokio::test]
    async fn an_unreadable_torrent_file_warns_instead_of_enqueueing() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "unreadable");
        let missing = root.join("nope.torrent");

        enqueue_torrent(&mut app, &missing).await;

        assert!(
            app.state
                .error_banner
                .as_deref()
                .is_some_and(|m| m.contains("could not read")),
            "a missing file is a loud warning, not a silent no-op"
        );
        assert!(app.queue.items().is_empty());
        assert!(engine.is_empty());
    }

    /// A source that reports per-site health without a network: the merge path
    /// in `apply_event` reads it after `SourceAnswered`, exactly like the lone
    /// `HttpSource` does with the indexer's `sources` array.
    struct ReportingSource;

    const REPORTING_DEF: SourceDef = SourceDef {
        id: SourceId::Indexer,
        label: "Reporting",
        groups: &[],
        homepage: "http://127.0.0.1:8765",
        reports_health: true,
    };

    impl Source for ReportingSource {
        fn def(&self) -> &'static SourceDef {
            &REPORTING_DEF
        }
        fn search<'a>(&'a self, _query: &'a str, _ctx: &'a SearchCtx) -> SearchFuture<'a> {
            Box::pin(async move { Ok(Vec::new()) })
        }
        fn reported_source_health(&self) -> HashMap<SourceId, (SourceStatus, u32)> {
            HashMap::from([
                (SourceId::Yts, (SourceStatus::Online, 3)),
                (SourceId::Nyaa, (SourceStatus::Empty, 0)),
                (SourceId::FitGirl, (SourceStatus::Offline, 0)),
            ])
        }
    }

    #[test]
    fn reported_site_health_merges_into_the_sidebar_after_a_search() {
        // FR-15/18: when the indexer answers, its per-site report must paint
        // the ten sidebar dots — not just the proxy source's own dot.
        let (mut app, _root) = test_app(Arc::new(FakeEngine::new()), "health-merge");
        app.search = SearchEngine::new(vec![Arc::new(ReportingSource)]);
        apply_event(
            &mut app,
            EngineEvent::SourceAnswered {
                source: SourceId::Indexer,
                count: 2,
            },
        );
        assert_eq!(
            app.state.search.source_health.get(&SourceId::Yts),
            Some(&SourceStatus::Online),
            "an online site gets a live dot"
        );
        assert_eq!(
            app.state.search.source_health.get(&SourceId::Nyaa),
            Some(&SourceStatus::Empty),
            "an empty site is reachable-but-nothing-matched"
        );
        assert_eq!(
            app.state.search.source_health.get(&SourceId::FitGirl),
            Some(&SourceStatus::Offline),
            "a failed site reads as down"
        );
        assert_eq!(
            app.state.search.source_counts.get(&SourceId::Yts),
            Some(&3),
            "the reported count lands too"
        );
        assert_eq!(
            app.state.search.source_health.get(&SourceId::Indexer),
            Some(&SourceStatus::Online),
            "the proxy source's own dot still comes from the event"
        );
    }

    async fn enqueue_named(app: &mut App, id: &str, name: &str, dir: PathBuf) {
        app.queue
            .add(
                AddInput {
                    id: id.to_string(),
                    name: name.into(),
                    source: None,
                    magnet: Some(format!("magnet:?xt=urn:btih:{id}")),
                    bytes: None,
                    dir,
                    size_bytes: 1,
                    only_files: None,
                },
                0,
            )
            .await;
        app.refresh_downloads();
        app.state.downloads.selected = 0;
        app.state.screen = Screen::Downloads;
    }

    #[tokio::test]
    async fn x_opens_forget_confirm_and_keeps_files_until_yes() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "forget-confirm");
        let id = "a".repeat(40);
        enqueue_named(&mut app, &id, "Dune", root.join("dl-a")).await;

        open_remove_confirm(&mut app, false);
        assert!(app.confirm.open);
        assert!(!app.confirm.yes_selected, "FR-76: default No");
        assert!(app.queue.get(&id).is_some());

        apply_confirm(&mut app, false).await;
        assert!(app.queue.get(&id).is_some(), "Esc/No leaves the item");
        assert!(!app.confirm.open);

        open_remove_confirm(&mut app, false);
        apply_confirm(&mut app, true).await;
        assert!(app.queue.get(&id).is_none(), "Yes forgets the item");
        assert_eq!(engine.last_remove_deleted_files(), Some(false));
    }

    #[tokio::test]
    async fn shift_x_deletes_files_when_the_dir_is_unshared() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "delete-files");
        let id = "b".repeat(40);
        enqueue_named(&mut app, &id, "Dune", root.join("dl-b")).await;

        open_remove_confirm(&mut app, true);
        assert!(app.confirm.destructive);
        assert!(app.confirm.body.contains("dl-b"));
        apply_confirm(&mut app, true).await;
        assert!(app.queue.get(&id).is_none());
        assert_eq!(engine.last_remove_deleted_files(), Some(true));
    }

    #[tokio::test]
    async fn shift_x_with_a_shared_dir_forgets_but_keeps_files() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "shared-dir");
        let a = "c".repeat(40);
        let b = "d".repeat(40);
        let shared = root.join("shared");
        enqueue_named(&mut app, &a, "One", shared.clone()).await;
        enqueue_named(&mut app, &b, "Two", shared).await;

        open_remove_confirm(&mut app, true);
        apply_confirm(&mut app, true).await;
        assert!(app.queue.get(&a).is_none(), "the item is forgotten");
        assert!(app.queue.get(&b).is_some());
        assert_eq!(engine.last_remove_deleted_files(), Some(false));
        assert!(
            app.state
                .error_banner
                .as_deref()
                .is_some_and(|m| m.contains("another download")),
            "FR-77: overlapping dirs surface a banner"
        );
    }

    #[tokio::test]
    async fn shift_x_keeps_the_torrent_cache_file() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine, "keep-cache");
        let id = "e".repeat(40);
        enqueue_named(&mut app, &id, "Dune", root.join("dl-e")).await;
        let cache = crate::core::paths::torrent_cache_file(&root, &id).expect("40-hex");
        std::fs::create_dir_all(cache.parent().expect("cache dir")).expect("cache dir");
        std::fs::write(&cache, b"d4:infod4:name3:fooe").expect("cache bytes");

        open_remove_confirm(&mut app, true);
        apply_confirm(&mut app, true).await;
        assert!(
            cache.exists(),
            "FR-78: delete must not remove cache/torrents/<id>.torrent"
        );
    }

    #[tokio::test]
    async fn shift_x_skips_files_while_the_item_is_playing() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "watch-block");
        let id = "f".repeat(40);
        enqueue_named(&mut app, &id, "Dune", root.join("dl-f")).await;
        app.state.now_playing = Some(crate::ui::NowPlaying {
            id: id.clone(),
            name: "Dune".into(),
            stream_url: "http://127.0.0.1:9/stream".into(),
            ephemeral: false,
            subtitle: None,
        });

        open_remove_confirm(&mut app, true);
        apply_confirm(&mut app, true).await;
        assert!(app.queue.get(&id).is_none());
        assert_eq!(engine.last_remove_deleted_files(), Some(false));
        assert!(
            app.state
                .error_banner
                .as_deref()
                .is_some_and(|m| m.contains("playing")),
            "FR-77: a live watch session keeps the files"
        );
    }
}
