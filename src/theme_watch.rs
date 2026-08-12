//! Live-reload watcher for custom themes (docs/theming.md §Custom themes).
//!
//! Watches the themes directory non-recursively and re-parses the active
//! theme on modify/create: a valid edit swaps in at the next render frame
//! (the shared [`Theme`] sits behind an `Arc<Mutex<_>>`, so the render loop
//! picks it up on its next lock), an invalid edit keeps the last valid theme
//! and prints a loud error — never a silent partial apply.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecursiveMode, Watcher, recommended_watcher};

use crate::theme::Theme;

/// Theme directory: `$HARBOUR_STATE_DIR/themes` when `HARBOUR_STATE_DIR` is
/// set (testing escape hatch per AGENTS.md), else `~/.harbour/themes`
/// (`%USERPROFILE%\.harbour\themes` on Windows). Creates the dir.
pub fn theme_dir() -> PathBuf {
    let dir = match std::env::var_os("HARBOUR_STATE_DIR") {
        Some(state) => PathBuf::from(state).join("themes"),
        // `dirs::home_dir()` is None only in degenerate environments (no
        // USERPROFILE/HOME); fall back to a relative dir rather than panic.
        None => dirs::home_dir()
            .unwrap_or_else(|| {
                eprintln!("theme watcher: no home dir; using ./.harbour/themes");
                PathBuf::from(".harbour")
            })
            .join("themes"),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        // Loud here, then the watch call in [`spawn_theme_watcher`] fails with
        // its own message — the watcher degrades to a no-op, never a panic.
        eprintln!("theme watcher: cannot create {}: {e}", dir.display());
    }
    dir
}

/// Spawns a detached thread watching [`theme_dir`] non-recursively. On
/// modify/create it re-parses any changed `<name>.json` whose `name` matches
/// the current theme's name and swaps the result into `theme`: a valid edit
/// applies at the next render frame; an invalid edit keeps the last valid
/// theme and prints `theme watcher: {error}` to stderr (theming.md: never
/// silent). Returns immediately — the thread owns the watcher, keeping it
/// alive for the process lifetime. If the dir cannot be created/watched, it
/// prints a loud error and returns (degraded gracefully, no panics).
pub fn spawn_theme_watcher(theme: Arc<Mutex<Theme>>) {
    let dir = theme_dir();

    let (tx, rx) = mpsc::channel();
    let mut watcher = match recommended_watcher(move |res: notify::Result<notify::Event>| {
        // A dead receiver means the thread exited; nothing left to surface.
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("theme watcher: cannot start watcher: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        eprintln!("theme watcher: cannot watch {}: {e}", dir.display());
        return;
    }

    std::thread::spawn(move || {
        // `watcher` must live here: dropping it would stop event delivery.
        // An Err from recv means the channel disconnected — the thread is
        // done; watch-delivery errors inside the channel are skipped.
        while let Ok(Ok(event)) = rx.recv() {
            // Editors rewrite via temp-file + rename; reloading on any
            // create/modify whose path matches the active theme suffices.
            if !handle_theme_event(&theme, &dir, &event) {
                return;
            }
        }
    });
}

/// Applies one watcher event to the shared theme. Returns `false` only when
/// the theme mutex is poisoned — reloading would fight a broken process, so
/// the watcher stops loudly instead of limping on.
fn handle_theme_event(theme: &Arc<Mutex<Theme>>, dir: &Path, event: &notify::Event) -> bool {
    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
        return true;
    }
    let current = match theme.lock() {
        Ok(guard) => guard.name.clone(),
        // A poisoned mutex means the app panicked elsewhere; reloading
        // would fight a broken process, so stop watching loudly.
        Err(_) => {
            eprintln!("theme watcher: theme mutex poisoned; reload disabled");
            return false;
        }
    };
    for path in &event.paths {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue; // no stem or non-UTF-8 name — not a theme file
        };
        if name != current {
            continue;
        }
        match Theme::load_custom(dir, name) {
            Ok(fresh) => {
                let Ok(mut guard) = theme.lock() else {
                    eprintln!("theme watcher: theme mutex poisoned; reload disabled");
                    return false;
                };
                *guard = fresh;
            }
            // Loud, and the previous theme stays in place.
            Err(e) => eprintln!("theme watcher: {e}"),
        }
    }
    true
}
