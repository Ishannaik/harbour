//! Watch mode (FR-57): streaming torrents to an external player — swarm
//! streaming first, then the file-serving fallback, the player-picker
//! overlay, and the ephemeral watch-now (2.3) path with its
//! stream-and-delete cleanup.

use std::path::{Path, PathBuf};

use crate::core::types::AddRequest;
use crate::ui::Screen;
use crate::ui::player::PickerMode;

use super::{App, PendingWatch};

/// The player to use for watch mode: the configured one when set, else the
/// first installed player. None means "no player at all" — the caller opens
/// the picker instead of guessing.
fn resolve_player(app: &App) -> Option<String> {
    match &app.config.player {
        Some(p) if !p.trim().is_empty() => Some(p.clone()),
        _ => crate::watch::find_player(),
    }
}

/// Opens the player-picker overlay, listing every installed player and
/// highlighting the current `config.player` choice.
pub(crate) fn open_player_picker(app: &mut App) {
    app.picker.options = crate::watch::find_players();
    app.picker.mode = PickerMode::List;
    app.picker.selected = app
        .picker
        .options
        .iter()
        .position(|(_, command)| app.config.player.as_deref() == Some(command.as_str()))
        .unwrap_or(0);
    app.picker.custom.clear();
    app.picker.message = None;
    app.picker.open = true;
}

/// Applies a picker choice: persists it to `config.player` (the config stays
/// the fallback/override), then launches any watch that was waiting on the
/// choice. A custom path must be absolute and exist — a failure is a loud
/// picker message, never a silent fallback.
pub(crate) async fn choose_player(app: &mut App) {
    let player = match app.picker.mode {
        PickerMode::List => {
            let Some((_, command)) = app.picker.options.get(app.picker.selected) else {
                app.picker.message =
                    Some("no players found — press c to enter a player path".into());
                return;
            };
            command.clone()
        }
        PickerMode::Custom => {
            let path = app.picker.custom.trim().to_string();
            if path.is_empty() {
                app.picker.message =
                    Some("enter an absolute path to a player, then press enter".into());
                return;
            }
            let candidate = Path::new(&path);
            if !candidate.is_absolute() || !candidate.exists() {
                app.picker.message = Some(format!("'{path}' is not an existing absolute path"));
                return;
            }
            path
        }
    };

    app.config.player = Some(player.clone());
    if let Err(err) = app.store.save_config(&app.config) {
        app.warn(format!("could not save player setting: {err}"));
    }
    app.picker.open = false;

    let Some(pending) = app.picker_pending.take() else {
        return;
    };
    if pending.ephemeral {
        launch_ephemeral_session(app, pending.id, pending.name, pending.dir, &player).await;
    } else {
        launch_watch(app, pending.id, pending.name, pending.dir, player).await;
    }
}

/// `w` on the selected downloads item: stream it to an external player (mpv
/// → VLC, or `config.player`). Two paths, engine-first (FR-57):
///
/// 1. **Swarm streaming** — the engine serves the torrent's largest video
///    file over its loopback HTTP API while pieces arrive (Stremio-style:
///    watch before the download finishes). This is the primary path.
/// 2. **File serving** — items with a media file already on disk but no
///    live session (or a fake/queued item) fall back to the local Range
///    server.
///
/// With no player at all, the picker opens with this watch pending — the
/// picker IS the guidance, not an error banner. Every launch failure is a
/// loud error banner — never a silent no-op.
pub(crate) async fn start_watch(app: &mut App) {
    // The selection indexes the visible tab, so resolve through it; clone
    // the fields (a ref into `items` would fight the mutations below).
    let Some(item) = app
        .visible_items()
        .get(app.state.downloads.selected)
        .map(|v| &v.item)
    else {
        return;
    };
    let id = item.id.clone();
    let name = item.name.clone();
    let dir = item.dir.clone();
    let Some(player) = resolve_player(app) else {
        // No player yet: still refuse archives before opening the picker, so
        // a zip pack never becomes "pick mpv, then fail" (issue #76).
        if nothing_watchable(app, &id, &dir).await {
            app.warn(crate::watch::unplayable_watch_banner(&dir));
            return;
        }
        app.picker_pending = Some(PendingWatch {
            id,
            name,
            dir,
            ephemeral: false,
        });
        open_player_picker(app);
        return;
    };
    launch_watch(app, id, name, dir, player).await;
}

/// True when the engine lists no video and the download dir has no media file.
/// Watch must not open a player (or the picker) in that case.
async fn nothing_watchable(app: &App, id: &str, dir: &Path) -> bool {
    app.queue.engine().list_video_files(id).await.is_empty()
        && crate::watch::primary_media(dir).is_none()
}

/// Launches a watch session for `id`/`name`/`dir` with `player`: swarm
/// streaming first, then the file-serving fallback. Shared by `start_watch`
/// and the player picker's pending launch.
async fn launch_watch(app: &mut App, id: String, name: String, dir: PathBuf, player: String) {
    let files = app.queue.engine().list_video_files(&id).await;
    if files.len() > 1 {
        app.episode_picker = crate::ui::EpisodePicker {
            open: true,
            torrent_id: id,
            torrent_name: name,
            player,
            ephemeral: false,
            episodes: files,
            selected: 0,
        };
        return;
    }

    // No engine-listed video: play a file on disk, or refuse. Never call
    // stream_url here — that is how a zip used to reach mpv (issue #76).
    if files.is_empty() {
        watch_file_or_refuse(app, id, name, &dir, &player, false);
        return;
    }

    // Path 1: stream from the swarm while it downloads. librqbit blocks on
    // missing pieces and prioritizes the requested ones — seek works.
    if let Some(url) = app.queue.engine().stream_url(&id).await {
        if let Err(reason) = probe_stream(&url).await {
            app.warn(format!("watch: '{name}' is not streaming — {reason}"));
            return;
        }
        match crate::watch::WatchSession::launch_remote(&player, &url) {
            Ok(session) => enter_watch(app, id, name, session, false),
            Err(err) => app.warn(format!("watch: cannot start player: {err}")),
        }
        return;
    }

    // Path 2: a file already on disk (completed item outside the session).
    let Some(file) = crate::watch::primary_media(&dir) else {
        app.warn(format!(
            "watch: no media file for '{name}' and the swarm cannot stream it yet"
        ));
        return;
    };
    match crate::watch::WatchSession::start(&file, &player) {
        Ok(session) => enter_watch(app, id, name, session, false),
        Err(err) => app.warn(format!("watch: cannot start player: {err}")),
    }
}

/// File-serving fallback when the swarm has no video to stream. Archives and
/// other non-media land on a banner rather than a player (issue #76).
fn watch_file_or_refuse(
    app: &mut App,
    id: String,
    name: String,
    dir: &Path,
    player: &str,
    ephemeral: bool,
) {
    let Some(file) = crate::watch::primary_media(dir) else {
        app.warn(crate::watch::unplayable_watch_banner(dir));
        return;
    };
    match crate::watch::WatchSession::start(&file, player) {
        Ok(session) => enter_watch(app, id, name, session, ephemeral),
        Err(err) => app.warn(format!("watch: cannot start player: {err}")),
    }
}

/// Enters watch mode with an already-launched session: records it on the app
/// state and flips the screen. `ephemeral` marks a watch-now (2.3) session.
///
/// FR-59: the stream URL recorded here is the playback state harbour
/// carries — every harbour stream is Range-served (the file server and the
/// engine's loopback HTTP API both honor `Range`), so seeking works, while
/// position belongs to the external player, which is launched with a bare
/// URL and never reports it back. The now-playing view renders exactly these
/// facts and never invents a progress bar.
fn enter_watch(
    app: &mut App,
    id: String,
    name: String,
    session: crate::watch::WatchSession,
    ephemeral: bool,
) {
    app.state.now_playing = Some(crate::ui::NowPlaying {
        id,
        name,
        stream_url: session.url.clone(),
        ephemeral,
    });
    app.watch = Some(session);
    app.state.screen = Screen::NowPlaying;
}

/// `w` on the search screen with an empty query (2.3): watch-now. The magnet
/// goes straight to the engine — no queue item, no ledger, no download
/// slot — so the files live under the cache dir and die with the session.
/// That is the stream-and-delete contract: a real download of the same
/// infohash is never touched (its `engine.add` would be a no-op re-add).
pub(crate) async fn start_watch_ephemeral(app: &mut App) {
    let Some(result) = app.selected_result().cloned() else {
        app.warn("nothing selected to watch");
        return;
    };
    let magnet = match &result.magnet {
        Some(magnet) => Some(magnet.clone()),
        None => super::actions::resolve_magnet(app, &result).await,
    };
    let Some(magnet) = magnet else {
        app.warn(format!("could not get a magnet link for {}", result.name));
        return;
    };
    // Re-key on the magnet's own infohash, exactly like `download_selected`.
    let id = crate::core::magnet::info_hash_from_magnet(&magnet)
        .unwrap_or_else(|| result.info_hash.clone());
    let dir = app.store.root().join("cache").join(&id);
    if let Err(err) = app
        .queue
        .engine()
        .add(AddRequest {
            id: id.clone(),
            magnet,
            dir: dir.clone(),
            trackers: app.config.trackers.clone(),
            only_files: None,
        })
        .await
    {
        app.warn(format!("watch: cannot start streaming: {err}"));
        return;
    }
    let Some(player) = resolve_player(app) else {
        if nothing_watchable(app, &id, &dir).await {
            app.warn(crate::watch::unplayable_watch_banner(&dir));
            return;
        }
        app.picker_pending = Some(PendingWatch {
            id,
            name: result.name.clone(),
            dir,
            ephemeral: true,
        });
        open_player_picker(app);
        return;
    };
    launch_ephemeral_session(app, id, result.name.clone(), dir, &player).await;
}

/// Launches the remote stream session for a watch-now (2.3): the torrent is
/// already added to the engine, so only the live stream URL is needed.
async fn launch_ephemeral_session(
    app: &mut App,
    id: String,
    name: String,
    dir: PathBuf,
    player: &str,
) {
    let files = app.queue.engine().list_video_files(&id).await;
    if files.len() > 1 {
        app.episode_picker = crate::ui::EpisodePicker {
            open: true,
            torrent_id: id,
            torrent_name: name,
            player: player.to_string(),
            ephemeral: true,
            episodes: files,
            selected: 0,
        };
        return;
    }

    if files.is_empty() {
        watch_file_or_refuse(app, id, name, &dir, player, true);
        return;
    }

    let Some(url) = app.queue.engine().stream_url(&id).await else {
        app.warn(format!("watch: the swarm cannot stream '{name}' yet"));
        return;
    };
    if let Err(reason) = probe_stream(&url).await {
        app.warn(format!("watch: '{name}' is not streaming — {reason}"));
        return;
    }
    match crate::watch::WatchSession::launch_remote(player, &url) {
        Ok(session) => enter_watch(app, id, name, session, true),
        Err(err) => app.warn(format!("watch: cannot start player: {err}")),
    }
}

/// Launches playback for a specific chosen episode from the episode picker modal.
pub(crate) async fn choose_episode(app: &mut App, opt_idx: Option<usize>) {
    let idx = opt_idx.unwrap_or(app.episode_picker.selected);
    let Some(ep) = app.episode_picker.episodes.get(idx).cloned() else {
        return;
    };
    let id = app.episode_picker.torrent_id.clone();
    let name = format!("{} - {}", app.episode_picker.torrent_name, ep.name);
    let player = app.episode_picker.player.clone();
    let ephemeral = app.episode_picker.ephemeral;
    app.episode_picker.open = false;

    let Some(url) = app.queue.engine().stream_file_url(&id, ep.id).await else {
        app.warn(format!("watch: cannot stream episode '{}'", ep.name));
        return;
    };
    if let Err(reason) = probe_stream(&url).await {
        app.warn(format!("watch: '{name}' is not streaming — {reason}"));
        return;
    }
    match crate::watch::WatchSession::launch_remote(&player, &url) {
        Ok(session) => enter_watch(app, id, name, session, ephemeral),
        Err(err) => app.warn(format!("watch: cannot start player: {err}")),
    }
}

/// Asks the stream endpoint for its first byte before launching the player.
/// librqbit's stream blocks on missing pieces — a dead swarm would hang the
/// player on a baffling "unable to open MRL". Probing turns that into our
/// own banner with the real reason, and a live swarm answers in seconds.
///
/// The probe is a `Range: bytes=0-0` request: a successful answer is also
/// what lets the now-playing view honestly state "seeking supported" (FR-59)
/// — the endpoint proved it honors Range, which is what player seeking is.
async fn probe_stream(url: &str) -> Result<(), String> {
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_millis(800))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("cannot build probe client: {e}")),
    };

    for _ in 0..6 {
        if let Ok(resp) = client.get(url).header("Range", "bytes=0-1024").send().await {
            let status = resp.status();
            if status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    Ok(())
}

/// Ends the session and returns to the downloads screen (FR-59: player exit
/// or `q`/esc). Kills the player and stops the stream server. A watch-now
/// session (2.3) also drops its torrent and cache dir — the stream-and-delete
/// contract — but never when the same infohash is a real queue item, whose
/// files the ephemeral re-add must not touch.
pub(crate) async fn end_watch(app: &mut App) {
    if let Some(mut session) = app.watch.take() {
        session.stop();
    }
    let ephemeral = app
        .state
        .now_playing
        .as_ref()
        .is_some_and(|np| np.ephemeral);
    let id = app.state.now_playing.as_ref().map(|np| np.id.clone());
    app.state.now_playing = None;
    app.state.screen = Screen::Downloads;

    let Some(id) = id else {
        return;
    };
    if !ephemeral {
        return;
    }
    // The dedupe guard: a queue item with this infohash owns its files.
    if app.queue.get(&id).is_some() {
        return;
    }
    if let Err(err) = app.queue.engine().remove(&id, true).await {
        app.warn(format!("watch: could not clean up the cache: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::core::error::EngineError;
    use crate::core::types::{AddRequest, Engine, EngineFuture, EngineSnapshot};
    use crate::engine::fake::FakeEngine;
    use crate::persist::{Config, Store};
    use crate::queue::{AddInput, Queue};
    use crate::search::SearchEngine;
    use crate::theme::Theme;
    use crate::ui::player::PlayerPicker;
    use crate::ui::settings::SettingsState;
    use crate::ui::{AppState, Screen};

    /// An engine that would happily hand the player a stream URL even when
    /// there is no video — the regression that launched mpv on a zip.
    struct ZipStreamEngine(FakeEngine);

    impl Engine for ZipStreamEngine {
        fn add<'a>(&'a self, req: AddRequest) -> EngineFuture<'a, Result<(), EngineError>> {
            self.0.add(req)
        }
        fn pause<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
            self.0.pause(id)
        }
        fn resume<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
            self.0.resume(id)
        }
        fn remove<'a>(
            &'a self,
            id: &'a str,
            delete_files: bool,
        ) -> EngineFuture<'a, Result<(), EngineError>> {
            self.0.remove(id, delete_files)
        }
        fn snapshot(&self) -> Vec<EngineSnapshot> {
            self.0.snapshot()
        }
        fn stream_url<'a>(&'a self, _id: &'a str) -> EngineFuture<'a, Option<String>> {
            Box::pin(async move { Some("http://127.0.0.1:1/must-not-play-zip".into()) })
        }
    }

    fn test_app(engine: Arc<dyn Engine>, label: &str) -> (App, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("harbour-watch-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        let config = Config {
            download_dir: root.join("dl"),
            player: Some("harbour-bug-76-not-a-player".into()),
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
            settings_open: false,
            settings: SettingsState::default(),
            theme: Arc::new(Mutex::new(Theme::titanium())),
            watch: None,
            picker: PlayerPicker::default(),
            picker_pending: None,
            episode_picker: crate::ui::EpisodePicker::default(),
            batch_picker: crate::ui::BatchPicker::default(),
            query_cache: HashMap::new(),
            quitting: false,
        };
        (app, root)
    }

    #[tokio::test]
    async fn watching_a_zip_only_torrent_never_launches_a_player() {
        let engine = Arc::new(ZipStreamEngine(FakeEngine::new()));
        let (mut app, root) = test_app(engine, "zip-pack");
        let dir = root.join("dl").join("zip-pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("game.zip"), b"PK\x03\x04").unwrap();

        app.queue
            .add(
                AddInput {
                    id: "ziphash".into(),
                    name: "GamesHub Pack".into(),
                    source: None,
                    magnet: Some("magnet:?xt=urn:btih:ziphash".into()),
                    bytes: None,
                    dir,
                    size_bytes: 4,
                    only_files: None,
                },
                0,
            )
            .await;
        app.state.screen = Screen::Downloads;
        app.refresh_downloads();

        start_watch(&mut app).await;

        assert!(
            app.watch.is_none(),
            "w on a zip pack must never spawn a player"
        );
        assert!(
            app.state.now_playing.is_none(),
            "w on a zip pack must not enter now-playing"
        );
        let banner = app.state.error_banner.as_deref().unwrap_or("");
        assert!(
            banner.contains("archive"),
            "zip packs need an archive banner, got: {banner}"
        );
        assert!(
            !banner.contains("cannot start player"),
            "the player must not have been invoked, got: {banner}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
