//! Watch mode (FR-57): streaming torrents to an external player — swarm
//! streaming first, then the file-serving fallback, the player-picker
//! overlay, and the ephemeral watch-now (2.3) path with its
//! stream-and-delete cleanup.

use std::path::{Path, PathBuf};

use crate::core::types::AddRequest;
use crate::ui::Screen;
use crate::ui::player::PickerMode;

use super::{App, PendingWatch};

/// The persisted watch player, if the user has chosen one. Empty/whitespace
/// counts as unset — first `w` opens the picker even when `find_player`
/// would auto-detect an installed binary (#73).
fn configured_player(player: Option<&str>) -> Option<&str> {
    player.map(str::trim).filter(|p| !p.is_empty())
}

/// Opens the picker with a watch waiting on the choice.
fn defer_watch_until_player_chosen(
    app: &mut App,
    id: String,
    name: String,
    dir: PathBuf,
    ephemeral: bool,
) {
    app.picker_pending = Some(PendingWatch {
        id,
        name,
        dir,
        ephemeral,
    });
    open_player_picker(app);
}

/// Opens the player-picker overlay, listing every installed player and
/// highlighting the current `config.player` choice.
pub(crate) fn open_player_picker(app: &mut App) {
    app.picker.options = crate::watch::find_players();
    app.picker.mode = PickerMode::List;
    let detected = crate::watch::find_player();
    let current = configured_player(app.config.player.as_deref()).or(detected.as_deref());
    app.picker.selected = app
        .picker
        .options
        .iter()
        .position(|(_, command)| current == Some(command.as_str()))
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
/// With no persisted player, the picker opens with this watch pending — even
/// when a default is installed — so the first `w` is a choice, not a surprise
/// launch. The picker IS the guidance, not an error banner. Every launch
/// failure is a loud error banner — never a silent no-op.
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
    // Refuse archives before the picker so a zip never becomes "pick mpv,
    // then fail" (issue #76). Unset config.player always prompts (#73).
    if nothing_watchable(app, &id, &dir).await {
        app.warn(crate::watch::unplayable_watch_banner(&dir));
        return;
    }
    let Some(player) = configured_player(app.config.player.as_deref()).map(str::to_string) else {
        defer_watch_until_player_chosen(app, id, name, dir, false);
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
    if nothing_watchable(app, &id, &dir).await {
        app.warn(crate::watch::unplayable_watch_banner(&dir));
        return;
    }
    let Some(player) = configured_player(app.config.player.as_deref()).map(str::to_string) else {
        defer_watch_until_player_chosen(app, id, result.name.clone(), dir, true);
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
/// The probe is a `Range: bytes=0-0` request (FR-104): only a real `206`
/// proves seeking will work. A bare `200`, a transport error, or six
/// exhausted attempts is `Err` — never `Ok(())` after failure (issue #72).
async fn probe_stream(url: &str) -> Result<(), String> {
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_millis(800))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("cannot build probe client: {e}")),
    };

    let mut last = "no response".to_string();
    for _ in 0..6 {
        match client.get(url).header("Range", "bytes=0-0").send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::PARTIAL_CONTENT {
                    return Ok(());
                }
                last = format!("HTTP {status} (need 206 Partial Content)");
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    Err(format!(
        "not enough data yet — opening Range never returned 206 ({last})"
    ))
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
        // Sequential pieces that already landed stay on disk. Ending watch
        // never deletes a real download (issue #72).
        return;
    }
    // The dedupe guard: a queue item with this infohash owns its files —
    // even if this session was watch-now of the same magnet.
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
    use crate::core::types::{AddRequest, Engine, EngineFuture, EngineSnapshot, TorrentFileView};
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

    async fn enqueue_one(app: &mut App, root: &Path) {
        let dir = root.join("dl");
        std::fs::create_dir_all(&dir).expect("download dir");
        // A watchable file so #76's archive/empty-dir guard does not fire
        // before the picker (#73) can open.
        std::fs::write(dir.join("movie.mkv"), b"fake").expect("watchable file");
        app.queue
            .add(
                AddInput {
                    id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    name: "Movie".into(),
                    source: None,
                    magnet: Some(
                        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    ),
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

    fn mark_watchable(engine: &FakeEngine) {
        engine.set_video_files(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            vec![TorrentFileView {
                id: 0,
                name: "movie.mkv".into(),
                size_bytes: 1,
            }],
        );
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

    #[test]
    fn unset_player_prompts_even_when_one_is_installed() {
        assert!(configured_player(None).is_none());
        assert!(configured_player(Some("")).is_none());
        assert!(configured_player(Some("  ")).is_none());
        assert_eq!(configured_player(Some("mpv")), Some("mpv"));
        assert_eq!(configured_player(Some("vlc")), Some("vlc"));
    }

    #[tokio::test]
    async fn first_watch_with_unset_player_opens_the_picker() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "first-w");
        enqueue_one(&mut app, &root).await;
        mark_watchable(&engine);
        assert!(app.config.player.is_none());

        start_watch(&mut app).await;

        assert!(
            app.picker.open,
            "first w with unset config.player must open the existing picker"
        );
        assert!(app.picker_pending.is_some(), "watch waits on the choice");
        assert!(app.watch.is_none(), "must not launch until Enter confirms");
    }

    #[tokio::test]
    async fn second_watch_after_a_choice_does_not_open_the_picker() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "second-w");
        enqueue_one(&mut app, &root).await;
        mark_watchable(&engine);
        start_watch(&mut app).await;
        assert!(app.picker.open);

        if app.picker.options.is_empty() {
            app.picker.options = vec![("mpv".into(), "mpv".into())];
        }
        choose_player(&mut app).await;
        assert!(app.config.player.is_some());
        assert!(!app.picker.open);

        start_watch(&mut app).await;
        assert!(
            !app.picker.open,
            "second w with a persisted player must not reopen the picker"
        );
    }

    #[tokio::test]
    async fn configured_player_skips_the_picker() {
        let engine = Arc::new(FakeEngine::new());
        let (mut app, root) = test_app(engine.clone(), "configured");
        enqueue_one(&mut app, &root).await;
        mark_watchable(&engine);
        app.config.player = Some("mpv".into());

        start_watch(&mut app).await;

        assert!(!app.picker.open);
        assert!(app.picker_pending.is_none());
    }

    /// Serves one HTTP status for every accepted connection, then closes.
    async fn serve_status(status_line: &'static str, extra_headers: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe stub");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                answer_once(&listener, status_line, extra_headers).await;
            }
        });
        format!("http://127.0.0.1:{port}/stream")
    }

    async fn answer_once(
        listener: &tokio::net::TcpListener,
        status_line: &str,
        extra_headers: &str,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 {status_line}\r\n\
             Content-Length: 1\r\n\
             Connection: close\r\n\
             {extra_headers}\r\nx"
        );
        let _ = stream.write_all(resp.as_bytes()).await;
    }

    #[tokio::test]
    async fn probe_stream_returns_err_after_failed_range_probes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/stream");
        let err = probe_stream(&url)
            .await
            .expect_err("six failed Range probes must be Err, never Ok(())");
        assert!(
            !err.is_empty(),
            "the caller needs a reason to put on the banner"
        );
    }

    #[tokio::test]
    async fn probe_stream_treats_a_bare_200_as_failure() {
        let url = serve_status("200 OK", "").await;
        probe_stream(&url)
            .await
            .expect_err("200 on a Range request means seeking will not work");
    }

    #[tokio::test]
    async fn probe_stream_accepts_206_partial_content() {
        let url = serve_status("206 Partial Content", "Content-Range: bytes 0-0/1\r\n").await;
        probe_stream(&url)
            .await
            .expect("a real Range 206 is the readiness gate");
    }

    struct RecordingEngine {
        inner: FakeEngine,
        removes: Arc<Mutex<Vec<(String, bool)>>>,
    }

    impl Engine for RecordingEngine {
        fn add<'a>(&'a self, req: AddRequest) -> EngineFuture<'a, Result<(), EngineError>> {
            self.inner.add(req)
        }
        fn pause<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
            self.inner.pause(id)
        }
        fn resume<'a>(&'a self, id: &'a str) -> EngineFuture<'a, Result<(), EngineError>> {
            self.inner.resume(id)
        }
        fn remove<'a>(
            &'a self,
            id: &'a str,
            delete_files: bool,
        ) -> EngineFuture<'a, Result<(), EngineError>> {
            self.removes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((id.to_string(), delete_files));
            self.inner.remove(id, delete_files)
        }
        fn snapshot(&self) -> Vec<EngineSnapshot> {
            self.inner.snapshot()
        }
    }

    #[tokio::test]
    async fn ending_a_real_download_watch_does_not_delete_pieces() {
        let removes = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::new(RecordingEngine {
            inner: FakeEngine::new(),
            removes: Arc::clone(&removes),
        });
        let (mut app, root) = test_app(engine, "keep-pieces");
        enqueue_one(&mut app, &root).await;
        let piece = root.join("dl").join("sequential.piece");
        std::fs::create_dir_all(piece.parent().unwrap()).unwrap();
        std::fs::write(&piece, b"landed").unwrap();

        app.state.now_playing = Some(crate::ui::NowPlaying {
            id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "Movie".into(),
            stream_url: "http://127.0.0.1:1/stream".into(),
            ephemeral: false,
        });
        end_watch(&mut app).await;

        assert!(
            piece.exists(),
            "stopping watch must leave sequential pieces of a real download"
        );
        let recorded = removes.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            recorded.is_empty(),
            "engine.remove must not run for a queue item, got {recorded:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn ending_watch_now_of_a_queued_infohash_does_not_delete_pieces() {
        let removes = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::new(RecordingEngine {
            inner: FakeEngine::new(),
            removes: Arc::clone(&removes),
        });
        let (mut app, root) = test_app(engine, "ephemeral-queued");
        enqueue_one(&mut app, &root).await;
        let piece = root.join("dl").join("sequential.piece");
        std::fs::create_dir_all(piece.parent().unwrap()).unwrap();
        std::fs::write(&piece, b"landed").unwrap();

        app.state.now_playing = Some(crate::ui::NowPlaying {
            id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "Movie".into(),
            stream_url: "http://127.0.0.1:1/stream".into(),
            ephemeral: true,
        });
        end_watch(&mut app).await;

        assert!(
            piece.exists(),
            "a real download of the same infohash owns the files"
        );
        let recorded = removes.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            recorded.is_empty(),
            "must not delete through the ephemeral path when the queue owns the hash"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
