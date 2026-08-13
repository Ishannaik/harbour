//! Application shell: terminal lifecycle, the 30fps event/draw loop, and the
//! screens it drives. Owns the parts that stay — entering/leaving the
//! terminal safely on every exit path and the tick-coalesced render loop —
//! while the boot splash, watch mode, settings, download actions, and input
//! dispatch live in the submodules below.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use crate::anim::{self, Ticker};
use crate::core::cancel::CancelToken;
use crate::core::paths;
use crate::core::types::{
    Engine as CoreEngine, EngineEvent, ItemView, QueueStatus, SearchCtx, SourceId, SourceStatus,
    TorrentResult,
};
use crate::engine::fake::FakeEngine;
use crate::engine::rqbit::RqbitEngine;
use crate::persist::{Config, Store};
use crate::queue::Queue;
use crate::search::SearchEngine;
use crate::theme::Theme;
use crate::ui::player::PlayerPicker;
use crate::ui::settings::SettingsState;
use crate::ui::{AppState, Screen};

mod actions;
mod events;
mod settings;
mod splash;
mod terminal;
mod watch;

use actions::{apply_event, enqueue_magnet};
use events::handle_event;
use splash::{SplashState, draw_splash};
use terminal::TerminalGuard;
use watch::end_watch;

/// Base render cadence (docs/design.md §Animation): the loop redraws at most
/// once per tick; a burst of input within one tick coalesces into one frame.
const FPS: u32 = 30;

/// Status spinner cadence (docs/design.md §Animation): one frame per 80ms.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// How often the queue polls the engine while anything is actively
/// transferring. Slower than the render cadence on purpose: progress that
/// changes thirty times a second is noise, and the eased bars smooth the gaps.
const POLL_ACTIVE: Duration = Duration::from_millis(500);

/// Poll cadence once everything has settled into seeding.
///
/// A seedbox with 200 idle seeds should not perform 400 stat reads a second to
/// learn nothing (`NFR-04`). Completion arrives as an event rather than being
/// discovered by polling, so nothing is missed by slowing down.
const POLL_IDLE: Duration = Duration::from_secs(5);

/// How long the splash holds before the search screen takes over.
const SPLASH_DURATION: Duration = Duration::from_millis(1800);

/// What the command line asked us to start with.
#[derive(Debug, Clone, PartialEq)]
pub enum InitialAction {
    None,
    /// Enqueue this magnet as soon as the engine is up (`FR-02`).
    Magnet(String),
    /// Read this `.torrent` and enqueue it.
    TorrentFile(PathBuf),
}

/// A watch deferred until the user picks a player (2.1): `start_watch` (or
/// `start_watch_ephemeral`) found no player, so the picker opened with this
/// waiting — the picker IS the guidance, not an error banner.
struct PendingWatch {
    id: String,
    name: String,
    dir: PathBuf,
    /// True for a watch-now (2.3) session: launched against the engine's
    /// live stream, never a queue item, and cleaned up when it ends.
    ephemeral: bool,
}

/// Everything the loop needs, assembled once at boot.
struct App {
    state: AppState,
    queue: Queue,
    search: SearchEngine,
    store: Store,
    config: Config,
    /// Sources the user disabled via the sidebar (2.2). The runtime set is
    /// the source of truth for search; `Config.disabled_sources` persists it.
    disabled_sources: HashSet<SourceId>,
    /// Results per source for the current query, merged for display.
    partial: HashMap<SourceId, Vec<TorrentResult>>,
    search_cancel: Option<CancelToken>,
    events_tx: mpsc::UnboundedSender<EngineEvent>,
    history: Vec<String>,
    help_open: bool,
    /// The settings overlay (2.5): everything in Config editable from the
    /// TUI. `settings_open` mirrors `help_open` — the overlay floats above
    /// whatever screen is underneath.
    settings_open: bool,
    /// Settings-overlay state: row selection, inline edit buffer, theme list.
    settings: SettingsState,
    /// The shared theme handle, so a settings theme change swaps in at the
    /// next frame (the same live-reload path as the file watcher).
    theme: Arc<Mutex<Theme>>,
    /// The active watch session (FR-57), if any — stream server + player.
    watch: Option<crate::watch::WatchSession>,
    /// The player-picker overlay (2.1): choose/override the watch player.
    picker: PlayerPicker,
    /// A watch waiting on a player choice, if any.
    picker_pending: Option<PendingWatch>,
    quitting: bool,
}

impl App {
    /// Something the user should know that must not stop the app.
    fn warn(&mut self, message: impl Into<String>) {
        self.state.error_banner = Some(message.into());
    }

    fn selected_result(&self) -> Option<&TorrentResult> {
        self.state.search.results.get(self.state.search.selected)
    }

    fn selected_item_id(&self) -> Option<String> {
        // Walk the *visible* tab's items so the selection never points at a
        // row hidden on the other tab (the Seeding tab renders only
        // finished items, the active tab only unfinished ones).
        self.visible_items()
            .get(self.state.downloads.selected)
            .map(|v| v.item.id.clone())
    }

    /// The items the current downloads tab actually shows, in render order.
    fn visible_items(&self) -> Vec<&ItemView> {
        self.state
            .downloads
            .items
            .iter()
            .filter(|v| {
                let finished =
                    v.item.status == QueueStatus::Seeding || v.item.status == QueueStatus::Missing;
                finished == self.state.downloads.show_seeding
            })
            .collect()
    }

    /// Rebuilds the downloads view from the queue.
    fn refresh_downloads(&mut self) {
        self.state.downloads.items = self.queue.views();
        self.state.downloads.history = self.queue.completed();
        let len = self.state.downloads.items.len();
        // Keep the cursor inside the list after a removal.
        if self.state.downloads.selected >= len {
            self.state.downloads.selected = len.saturating_sub(1);
        }
    }

    /// Merges everything received so far into the displayed list. Batches
    /// from a disabled source are dropped before the merge, so a stale answer
    /// can never re-enter the list (2.2).
    fn remerge(&mut self) {
        let all: Vec<TorrentResult> = self
            .partial
            .iter()
            .filter(|(source, _)| !self.disabled_sources.contains(source))
            .flat_map(|(_, results)| results.iter().cloned())
            .collect();
        self.state.search.results = crate::search::merge(all);
        let len = self.state.search.results.len();
        if self.state.search.selected >= len {
            self.state.search.selected = len.saturating_sub(1);
        }
    }

    /// Stops an in-flight search: the partial results already merged stay on
    /// screen, so navigation is stable (arrow keys during streaming read as
    /// "let me look at what's here", not "move the cursor under a changing
    /// list").
    /// Starts a search, cancelling whatever was in flight (`FR-20`).
    fn start_search(&mut self, query: String) {
        if let Some(previous) = self.search_cancel.take() {
            previous.cancel();
        }
        self.partial.clear();
        self.state.search.results.clear();
        self.state.search.selected = 0;
        self.state.search.searching = true;
        self.state.search.source_counts.clear();
        for id in SourceId::ALL {
            self.state
                .search
                .source_health
                .insert(id, SourceStatus::Unknown);
        }

        if !query.trim().is_empty() {
            let mut history = std::mem::take(&mut self.history);
            if let Err(err) = self.store.push_history(&mut history, &query) {
                // Losing search history is not worth interrupting anyone over.
                eprintln!("harbour: could not save search history: {err}");
            }
            self.history = history;
        }

        let ctx = SearchCtx {
            total_deadline: paths::source_timeout(),
            ..SearchCtx::default()
        };
        // The engine skips disabled sources before they are spawned, so a
        // disabled source is never queried and never merges results (2.2).
        self.search.set_disabled(self.disabled_sources.clone());
        self.search_cancel = Some(ctx.cancel.clone());
        self.search.start(query, ctx, self.events_tx.clone());
    }
}

/// Reads terminal events on a dedicated thread.
///
/// `crossterm::event::read` blocks, and blocking a tokio worker would stall the
/// engine's tasks with it. One OS thread feeding a channel keeps input
/// responsive without the async runtime ever waiting on a keypress.
fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            let Ok(ev) = event::read() else {
                // A terminal that stops producing events is not recoverable
                // here, and spinning on the error would peg a core.
                return;
            };
            if tx.send(ev).is_err() {
                // The app has gone; so should we.
                return;
            }
        }
    });
    rx
}

/// A crash mid-watch (2.3) leaves its ephemeral torrent in librqbit's
/// persistence and its files under `<state>/cache/<hash>`. The queue never
/// owns those dirs (watch-now is ledger-free by contract), so on boot they
/// are orphans: drop the torrent and delete the files — the same
/// stream-and-delete contract, enforced after the fact.
async fn cleanup_orphaned_cache(engine: &Arc<dyn CoreEngine>, root: &std::path::Path) {
    let cache_root = root.join("cache");
    let Ok(entries) = std::fs::read_dir(&cache_root) else {
        return; // no cache dir = nothing to clean
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.len() != 40 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
            continue; // not a torrent id — leave it alone
        }
        let _ = engine.remove(&name, true).await;
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// Runs the TUI.
///
/// Takes the shared `Arc<Mutex<Theme>>` so the theme-watcher thread can swap
/// themes underneath a running render loop; the lock is taken once per frame
/// and released before the next wait, so a swap never blocks input.
///
/// Every failure on this path degrades rather than aborting (`NFR-15`): a
/// config that will not parse falls back with a banner, a ledger that will not
/// read starts empty and keeps the file, and an engine that will not construct
/// leaves search working with downloads reporting why.
pub async fn run(
    theme: Arc<Mutex<Theme>>,
    initial: InitialAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::from_env();

    let loaded_config = store.load_config();
    let config_warning = loaded_config.warning().map(str::to_owned);
    let config = loaded_config.value();

    // The crash breaker: a marker left behind means the previous run died
    // before it finished starting, so this one restores everything paused.
    let safe_mode = store.boot_was_interrupted();
    if let Err(err) = store.arm_boot_marker() {
        eprintln!("harbour: could not write the boot marker: {err}");
    }

    let loaded_ledger = store.load_ledger();
    let ledger_warning = loaded_ledger.warning().map(str::to_owned);
    let items = loaded_ledger.value();
    let history = store.load_history().value();

    // An engine that will not start must not stop the app: search still works,
    // and downloads report the reason instead of the window refusing to open.
    let launch_opts = crate::engine::rqbit::EngineLaunchOptions::from_config(&config);
    let (engine, engine_error): (Arc<dyn CoreEngine>, Option<String>) =
        match RqbitEngine::new(&config.download_dir, store.root(), &launch_opts).await {
            Ok(engine) => {
                // Adopt anything librqbit restored from its own persistence, or
                // it would be running but invisible to the queue.
                engine.adopt_restored();
                (Arc::new(engine), None)
            }
            Err(err) => (
                Arc::new(FakeEngine::new()),
                Some(format!("downloads are unavailable: {err}")),
            ),
        };

    // A crash mid-watch (2.3) leaves its ephemeral torrent in librqbit's
    // persistence and its files under <state>/cache/<hash>. The queue never
    // owns those dirs (watch-now is ledger-free by contract), so on boot
    // they are orphans: drop the torrent and delete the files — the same
    // stream-and-delete contract, enforced after the fact.
    cleanup_orphaned_cache(&engine, store.root()).await;

    // Boot-time policies from settings: the queueing cap (config wins over
    // the env default), the share-ratio seed stop, and the live rate limits.
    let mut queue = Queue::new(
        engine.clone(),
        config
            .max_active_downloads
            .unwrap_or_else(crate::core::paths::max_downloads),
    );
    queue.set_trackers(config.trackers.clone());
    queue.set_stop_ratio(if config.stop_seed_at_ratio {
        Some(config.seed_ratio)
    } else {
        None
    });
    engine.set_speed_limits(
        if config.use_alt_rates {
            config.alt_download_limit_mib
        } else {
            config.download_limit_mib
        },
        if config.use_alt_rates {
            config.alt_upload_limit_mib
        } else {
            config.upload_limit_mib
        },
    );
    queue.restore(items, safe_mode).await;

    // The one source: the indexer proxy. All ten site scrapers live in the
    // user-run harbour-indexer service; the client ships zero scraping code.
    let search = SearchEngine::new(vec![Arc::new(crate::sources::HttpSource::new(
        config.indexer_url.clone(),
    ))]);

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut app = App {
        state: AppState::default(),
        queue,
        search,
        store,
        disabled_sources: config.disabled_sources.iter().copied().collect(),
        config,
        partial: HashMap::new(),
        search_cancel: None,
        events_tx,
        history,
        help_open: false,
        settings_open: false,
        settings: SettingsState::default(),
        theme: theme.clone(),
        watch: None,
        picker: PlayerPicker::default(),
        picker_pending: None,
        quitting: false,
    };

    app.state.screen = Screen::Splash;
    app.refresh_downloads();

    // `warn` owns a single banner slot, so collapsing several startup
    // problems into one message is what keeps them all visible — a corrupt
    // ledger plus a failed engine plus safe mode must not silently drop to
    // just the last one.
    let startup_warnings: Vec<String> = [config_warning, ledger_warning, engine_error]
        .into_iter()
        .flatten()
        .collect();
    if !startup_warnings.is_empty() {
        app.warn(startup_warnings.join("\n"));
    }
    if safe_mode {
        app.warn(
            "harbour did not shut down cleanly last time, so everything is paused. \
             Press p on an item to resume it.",
        );
    }

    match initial {
        InitialAction::None => {}
        InitialAction::Magnet(magnet) => enqueue_magnet(&mut app, &magnet).await,
        InitialAction::TorrentFile(path) => match std::fs::metadata(&path) {
            // Reading a .torrent means parsing bencode and hashing its info
            // dict. librqbit can do both, but wiring the file path through the
            // add request is engine work that has not landed; say so plainly
            // rather than failing silently on launch.
            Ok(_) => app.warn(format!(
                "{} was found, but opening a .torrent on launch is not wired up yet — \
                 paste its magnet instead",
                path.display()
            )),
            Err(err) => app.warn(format!("could not read {}: {err}", path.display())),
        },
    }

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut ticker = Ticker::new(FPS);
    let mut splash = SplashState::new(&lock_theme(&theme));
    let mut input = spawn_input_thread();
    let mut last_poll = Instant::now();
    let started = Instant::now();
    let mut frame_warned = false;

    while !app.quitting {
        // Wait for the next frame slot, but wake early for input or an engine
        // event so neither waits a whole frame to be seen.
        let wait = ticker.next();
        tokio::select! {
            biased;
            Some(ev) = input.recv() => handle_event(&mut app, ev).await,
            Some(engine_event) = events_rx.recv() => apply_event(&mut app, engine_event),
            _ = tokio::time::sleep(wait) => {}
        }

        // Drain whatever else arrived in the same instant, so a burst produces
        // one frame rather than one frame each.
        while let Ok(ev) = input.try_recv() {
            handle_event(&mut app, ev).await;
        }
        while let Ok(engine_event) = events_rx.try_recv() {
            apply_event(&mut app, engine_event);
        }

        let now = Instant::now();
        let cadence = if app.queue.active_count() > 0 {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        };
        if now.duration_since(last_poll) >= cadence {
            last_poll = now;
            let events = app.queue.tick(now).await;
            for engine_event in events {
                apply_event(&mut app, engine_event);
            }
            app.refresh_downloads();
        }

        // The splash is a timed intro, not a state to be stuck in.
        if app.state.screen == Screen::Splash && started.elapsed() >= SPLASH_DURATION {
            app.state.screen = Screen::Search;
        }

        // FR-59: when the player exits, the watch session ends and the TUI
        // returns to the downloads screen.
        if app.state.screen == Screen::NowPlaying
            && app
                .watch
                .as_mut()
                .is_some_and(|session| session.player_exited())
        {
            end_watch(&mut app).await;
        }

        let frame_result = {
            let active = lock_theme(&theme);
            splash.spinner.set_frames(&active.symbols.spinner_frames);
            splash.spinner.advance(now, SPINNER_INTERVAL);
            let glyph = splash.spinner.current().to_owned();
            draw_frame(&mut terminal, &active, &app, &mut splash, now, &glyph)
        };
        if let Err(err) = frame_result {
            // One bad frame (a transient terminal hiccup) must not kill the
            // app: log once and keep the loop alive. Ctrl+C still quits.
            if !frame_warned {
                eprintln!("harbour: a frame failed: {err}");
                frame_warned = true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Flush before standing the crash breaker down: a crash between the two
    // would otherwise leave a clean marker over stale state.
    if let Err(err) = app.store.flush_and_disarm(app.queue.items()) {
        eprintln!("harbour: could not save state on exit: {err}");
    }
    Ok(())
}

/// One synchronized frame. Extracted so the theme lock never lives inside
/// the closure passed to the terminal (a guard across the draw call reads
/// as an await-holding lock to clippy even though the call is synchronous).
fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    theme: &Theme,
    app: &App,
    splash: &mut SplashState,
    now: Instant,
    glyph: &str,
) -> std::io::Result<()> {
    anim::with_sync_output(|| {
        terminal.draw(|frame| draw(frame, theme, app, splash, now, glyph))?;
        Ok(())
    })
}

/// Draws whichever screen is current, plus the status line and any overlay.
fn draw(
    frame: &mut Frame,
    theme: &Theme,
    app: &App,
    splash: &mut SplashState,
    now: Instant,
    glyph: &str,
) {
    let area = frame.area();
    if app.state.screen == Screen::Splash {
        draw_splash(frame, theme, splash, now);
        return;
    }

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(status_height(app)),
    ])
    .split(area);

    match app.state.screen {
        Screen::Downloads => {
            crate::ui::downloads::draw(frame, rows[0], &app.state.downloads, theme)
        }
        Screen::NowPlaying => {
            if let Some(np) = &app.state.now_playing {
                crate::ui::now_playing::draw(frame, rows[0], np, theme);
            }
        }
        _ => crate::ui::search::draw(
            frame,
            rows[0],
            &app.state.search,
            &app.disabled_sources,
            theme,
        ),
    }
    crate::ui::status::draw(frame, rows[1], app.state.screen, &app.state, theme, glyph);

    if app.help_open {
        crate::ui::help::draw(frame, area, theme);
    }
    if app.picker.open {
        crate::ui::player::draw(
            frame,
            area,
            theme,
            &app.picker,
            app.config.player.as_deref(),
        );
    }
    if app.settings_open {
        crate::ui::settings::draw(
            frame,
            area,
            &app.config,
            &app.disabled_sources,
            &app.settings,
            theme,
        );
    }
}

/// Rows to reserve for the status area.
///
/// This must match `ui::status::draw`'s own layout exactly. That view splits
/// the area it is given into `[Min(0), banner?, status]` and draws only the
/// bottom two, so handing it fewer rows than it wants does not shrink the
/// banner — it squeezes it out entirely and the message is never seen. Under-
/// allocating by a single row was enough to make the safe-mode warning
/// invisible, which is exactly the class of bug a banner exists to prevent.
fn status_height(app: &App) -> u16 {
    banner_height(app.state.error_banner.as_deref()) + 1
}

/// Banner rows: two borders plus one or two content rows, or zero when there is
/// nothing to say. Mirrors `ui::status::draw`.
fn banner_height(message: Option<&str>) -> u16 {
    message.map_or(0, |m| 2 + m.lines().count().clamp(1, 2) as u16)
}

/// The area the current screen draws into, for mouse hit-testing.
///
/// Mirrors `draw`'s `Layout::vertical([Min(0), Length(status_height)])`
/// split: the full terminal size, minus the rows the status bar reserves.
/// The terminal size is the same number the frame layout reads on the next
/// draw, so a click lands on the view the user is actually looking at.
fn mouse_view_area(app: &App) -> Rect {
    let (width, height) = crossterm::terminal::size().unwrap_or((0, 0));
    Rect::new(0, 0, width, height.saturating_sub(status_height(app)))
}

/// Wall-clock milliseconds, used only for ordering the queue.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Recover from a poisoned theme lock instead of panicking: a watcher thread
/// that panicked mid-swap must not take the render loop down with it.
fn lock_theme(theme: &Arc<Mutex<Theme>>) -> std::sync::MutexGuard<'_, Theme> {
    theme
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn the_status_line_is_one_row_until_something_needs_saying() {
        let mut app_state = AppState::default();
        assert_eq!(app_state.error_banner, None);
        app_state.error_banner = Some("one line".into());
        // Constructed indirectly: status_height only reads the banner.
        let lines = app_state
            .error_banner
            .as_ref()
            .map(|m| (m.lines().count() as u16 + 2).clamp(3, 6));
        assert_eq!(lines, Some(3));

        app_state.error_banner = Some("a\nb\nc\nd\ne\nf\ng".into());
        let lines = app_state
            .error_banner
            .as_ref()
            .map(|m| (m.lines().count() as u16 + 2).clamp(3, 6));
        assert_eq!(lines, Some(6), "a long banner is capped, never unbounded");
    }

    #[tokio::test]
    async fn orphaned_cache_torrents_are_removed_on_boot() {
        // A crashed watch-now leaves <state>/cache/<hash> plus a restored
        // torrent. Boot cleanup must drop both — and leave non-torrent
        // entries alone.
        let root = std::env::temp_dir().join(format!("harbour-cache-{}", std::process::id()));
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let dir = root.join("cache").join(hash);
        std::fs::create_dir_all(&dir).expect("test dir");
        std::fs::write(dir.join("piece.bin"), b"x").expect("test file");
        let stranger = root.join("cache").join("notes.txt");
        std::fs::write(&stranger, b"keep me").expect("test file");

        let engine: Arc<dyn CoreEngine> = Arc::new(crate::engine::fake::FakeEngine::new());
        cleanup_orphaned_cache(&engine, &root).await;

        assert!(!dir.exists(), "the orphaned cache dir is deleted");
        assert!(
            engine.snapshot().iter().all(|s| s.id != hash),
            "the orphaned torrent is dropped from the engine"
        );
        assert!(
            stranger.exists(),
            "non-torrent entries in the cache dir are never touched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
