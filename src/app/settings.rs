//! The settings overlay (2.5): row activation, inline text edits, theme
//! cycling with live apply, source toggles, and config persistence.

use std::path::PathBuf;

use crate::core::types::SourceId;
use crate::theme::Theme;
use crate::ui::FolderPromptMode;
use crate::ui::settings::{RowKind, TextField};

use super::App;
use super::actions::download_selected_picking_files;
use super::watch::open_player_picker;

/// The settings overlay's Enter: per row kind, either open the player
/// picker, enter/commit an inline text edit, cycle the theme, or flip a
/// toggle immediately.
pub(crate) fn settings_activate(app: &mut App) {
    let Some(kind) = crate::ui::settings::row_kind(app.settings.selected) else {
        return;
    };
    match kind {
        RowKind::Player => open_player_picker(app),
        RowKind::Text => settings_edit_text(app),
        RowKind::Theme => settings_cycle_theme(app),
        RowKind::Toggle => settings_toggle_row(app),
        RowKind::Source => {
            if let Some(id) = crate::ui::settings::source_at(app.settings.selected) {
                settings_toggle_source(app, id);
            }
        }
        RowKind::Action => super::actions::open_clear_cache_confirm(app),
    }
}

/// Toggle-row Enter: flip the config bit, persist, and apply live whatever
/// can apply live (the queueing cap, the ratio policy, the rate limits).
/// Boot-time knobs (upnp, dht) persist and apply on the next launch, which
/// their labels say.
fn settings_toggle_row(app: &mut App) {
    match app.settings.selected {
        3 => {
            app.config.seed_by_default = !app.config.seed_by_default;
            app.queue.set_seed_by_default(app.config.seed_by_default);
        }
        4 => app.config.ask_save_path = !app.config.ask_save_path,
        10 => {
            app.config.use_alt_rates = !app.config.use_alt_rates;
            apply_rate_limits(app);
        }
        13 => app.config.enable_upnp = !app.config.enable_upnp,
        14 => app.config.enable_dht = !app.config.enable_dht,
        16 => {
            app.config.stop_seed_at_ratio = !app.config.stop_seed_at_ratio;
            app.queue.set_stop_ratio(if app.config.stop_seed_at_ratio {
                Some(app.config.seed_ratio)
            } else {
                None
            });
        }
        _ => return,
    }
    save_settings(app);
}

/// The effective (normal or alt) rate limits from config, applied live.
fn apply_rate_limits(app: &mut App) {
    let (down, up) = if app.config.use_alt_rates {
        (
            app.config.alt_download_limit_mib,
            app.config.alt_upload_limit_mib,
        )
    } else {
        (app.config.download_limit_mib, app.config.upload_limit_mib)
    };
    app.queue.engine().set_speed_limits(down, up);
}

/// Parses a numeric settings value; an empty input means "unlimited/auto".
/// Returns None when the text is not a valid number — the caller warns
/// loudly and keeps the edit open; a bad value never silently becomes
/// something else.
fn parse_opt_number(text: &str) -> Option<Option<u64>> {
    let t = text.trim();
    if t.is_empty() {
        return Some(None);
    }
    match t.parse::<u64>() {
        Ok(n) => Some(Some(n)),
        Err(_) => None,
    }
}

/// Text-row Enter: the first press starts an inline edit seeded with the
/// field's current value, the second commits the buffer into the config and
/// saves. Committing an empty player path means auto-detect; an empty
/// tracker list is no trackers.
fn settings_edit_text(app: &mut App) {
    let Some(field) = crate::ui::settings::text_field(app.settings.selected) else {
        return;
    };
    if !app.settings.editing {
        app.settings.edit_buffer = match field {
            TextField::Player => app.config.player.clone().unwrap_or_default(),
            TextField::DownloadDir => app.config.download_dir.display().to_string(),
            TextField::Trackers => app.config.trackers.join(", "),
            TextField::DownloadLimit => opt_mib(app.config.download_limit_mib),
            TextField::UploadLimit => opt_mib(app.config.upload_limit_mib),
            TextField::AltDownloadLimit => opt_mib(app.config.alt_download_limit_mib),
            TextField::AltUploadLimit => opt_mib(app.config.alt_upload_limit_mib),
            TextField::MaxActiveDownloads => app
                .config
                .max_active_downloads
                .map(|n| n.to_string())
                .unwrap_or_default(),
            TextField::ListenPort => app
                .config
                .listen_port
                .map(|p| p.to_string())
                .unwrap_or_default(),
            TextField::SocksProxy => app.config.socks_proxy_url.clone().unwrap_or_default(),
            TextField::SeedRatio => format!("{:.1}", app.config.seed_ratio),
        };
        app.settings.editing = true;
        return;
    }
    let value = app.settings.edit_buffer.trim();
    match field {
        TextField::Player => {
            app.config.player = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        TextField::DownloadDir => app.config.download_dir = PathBuf::from(value),
        TextField::Trackers => {
            app.config.trackers = value
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();
        }
        // Numeric rows: parse or stay editing with a loud banner — never a
        // silent default.
        TextField::DownloadLimit => match parse_opt_number(value) {
            Some(n) => {
                app.config.download_limit_mib = n;
                apply_rate_limits(app);
            }
            None => {
                app.warn(format!(
                    "'{value}' is not a number — leave empty for unlimited"
                ));
                return;
            }
        },
        TextField::UploadLimit => match parse_opt_number(value) {
            Some(n) => {
                app.config.upload_limit_mib = n;
                apply_rate_limits(app);
            }
            None => {
                app.warn(format!(
                    "'{value}' is not a number — leave empty for unlimited"
                ));
                return;
            }
        },
        TextField::AltDownloadLimit => match parse_opt_number(value) {
            Some(n) => app.config.alt_download_limit_mib = n,
            None => {
                app.warn(format!(
                    "'{value}' is not a number — leave empty for unlimited"
                ));
                return;
            }
        },
        TextField::AltUploadLimit => match parse_opt_number(value) {
            Some(n) => app.config.alt_upload_limit_mib = n,
            None => {
                app.warn(format!(
                    "'{value}' is not a number — leave empty for unlimited"
                ));
                return;
            }
        },
        TextField::MaxActiveDownloads => match parse_opt_number(value) {
            Some(n) => {
                app.config.max_active_downloads = n.map(|v| v as usize);
                app.queue.set_max_downloads(
                    app.config
                        .max_active_downloads
                        .unwrap_or_else(crate::core::paths::max_downloads),
                );
            }
            None => {
                app.warn(format!(
                    "'{value}' is not a number — empty = the env default"
                ));
                return;
            }
        },
        TextField::ListenPort => match parse_opt_number(value) {
            Some(n) => {
                if let Some(port) = n {
                    if port > u16::MAX as u64 {
                        app.warn(format!("{port} is not a valid port (1-65535)"));
                        return;
                    }
                    app.config.listen_port = Some(port as u16);
                } else {
                    app.config.listen_port = None;
                }
            }
            None => {
                app.warn(format!("'{value}' is not a number — empty = auto"));
                return;
            }
        },
        TextField::SocksProxy => {
            app.config.socks_proxy_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        TextField::SeedRatio => match value.parse::<f64>() {
            Ok(r) if r.is_finite() && r >= 0.0 => {
                app.config.seed_ratio = r;
                if app.config.stop_seed_at_ratio {
                    app.queue.set_stop_ratio(Some(r));
                }
            }
            _ => {
                app.warn(format!("'{value}' is not a ratio (a number ≥ 0)"));
                return;
            }
        },
    }
    app.settings.editing = false;
    app.settings.edit_buffer.clear();
    save_settings(app);
}

/// The settings edit buffer's seed form for a MiB/s row ("unlimited" when
/// unset — the same text the row value shows).
fn opt_mib(mib: Option<u64>) -> String {
    mib.map(|m| m.to_string())
        .unwrap_or_else(|| "unlimited".to_string())
}

/// The theme row's Enter: advance to the next installed theme, apply it
/// live, and persist. The cycle wraps around; a theme file that fails to
/// parse keeps the current theme (and the config) unchanged, loudly.
fn settings_cycle_theme(app: &mut App) {
    if app.settings.themes.is_empty() {
        // The overlay usually fills this at open; a directory that vanished
        // mid-session must not strand the row.
        app.settings.themes = crate::ui::settings::installed_themes();
    }
    let position = app
        .settings
        .themes
        .iter()
        .position(|name| *name == app.config.theme)
        .unwrap_or(0);
    let next = app.settings.themes[(position + 1) % app.settings.themes.len()].clone();
    if apply_theme_live(app, &next) {
        app.config.theme = next;
        save_settings(app);
    }
}

/// Swaps the shared theme to `name` so a settings change applies at the
/// next frame — the same load path the file watcher uses (theme_watch), so
/// a settings change and a file edit behave identically. Returns `false`
/// (keeping the current theme and saying so loudly) when the theme fails to
/// parse.
fn apply_theme_live(app: &mut App, name: &str) -> bool {
    let fresh = match name {
        "titanium" => Ok(Theme::titanium()),
        name => Theme::load_custom(&crate::theme_watch::theme_dir(), name),
    };
    let fresh = match fresh {
        Ok(theme) => theme,
        Err(err) => {
            app.warn(format!("settings: theme '{name}' failed to load: {err}"));
            return false;
        }
    };
    match app.theme.lock() {
        Ok(mut guard) => *guard = fresh,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = fresh;
        }
    }
    true
}

/// Toggles one source in the disabled set and persists it. The runtime
/// `disabled_sources` set is re-derived from the saved config so the
/// sidebar, the settings view, and search filtering never disagree.
fn settings_toggle_source(app: &mut App, id: SourceId) {
    if let Some(pos) = app.config.disabled_sources.iter().position(|s| *s == id) {
        app.config.disabled_sources.remove(pos);
    } else {
        app.config.disabled_sources.push(id);
    }
    // Deterministic config output: the persisted list is always sorted.
    app.config.disabled_sources.sort_by_key(|s| s.as_str());
    save_settings(app);
    app.apply_source_filter();
}

/// Persists `app.config` through the existing store, then re-derives the
/// enabled-source set from what was saved. A failed save is a loud banner —
/// the in-memory config stays as the user set it.
fn save_settings(app: &mut App) {
    if let Err(err) = app.store.save_config(&app.config) {
        app.warn(format!("settings: could not save config: {err}"));
    }
    app.disabled_sources = app.config.disabled_sources.iter().copied().collect();
}

/// Opens the folder prompt (FR-29/40), seeded with the current default
/// download folder so the common case is just Enter. `mode` decides what
/// Enter commits: a one-off download target (shift+D) or the persisted
/// default (`o`).
pub(crate) fn open_folder_prompt(app: &mut App, mode: FolderPromptMode) {
    app.state.folder_prompt.open = true;
    app.state.folder_prompt.mode = mode;
    app.state.folder_prompt.edit_buffer = app.config.download_dir.display().to_string();
}

/// Closes the folder prompt without committing — the typed buffer is
/// discarded, exactly like Esc on a settings inline edit.
pub(crate) fn cancel_folder_prompt(app: &mut App) {
    app.state.folder_prompt.open = false;
    app.state.folder_prompt.edit_buffer.clear();
}

/// Enter on the folder prompt: validates the path, then either downloads the
/// selected row into it (shift+D) or persists it as the new default folder
/// (`o`). An empty path or a directory that cannot be created keeps the
/// prompt open with a loud banner — never a silent fallback.
pub(crate) async fn commit_folder_prompt(app: &mut App) {
    let dir = PathBuf::from(app.state.folder_prompt.edit_buffer.trim());
    if dir.as_os_str().is_empty() {
        app.warn("enter a folder path first");
        return;
    }
    if let Err(err) = std::fs::create_dir_all(&dir) {
        app.warn(format!("could not create {}: {err}", dir.display()));
        return;
    }
    match app.state.folder_prompt.mode {
        FolderPromptMode::SetDefault => {
            app.config.download_dir = dir;
            save_settings(app);
        }
        FolderPromptMode::DownloadTo => {
            // One-off: download the selected row into `dir` without touching
            // the configured default. `download_selected` copies
            // `config.download_dir` into the queue item synchronously, so a
            // temporary swap is safe — the original is restored right after.
            let previous = std::mem::replace(&mut app.config.download_dir, dir);
            download_selected_picking_files(app).await;
            app.config.download_dir = previous;
        }
    }
    app.state.folder_prompt.open = false;
    app.state.folder_prompt.edit_buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use crate::engine::fake::FakeEngine;
    use crate::persist::{Config, Store};
    use crate::queue::Queue;
    use crate::search::SearchEngine;
    use crate::theme::Theme;
    use crate::ui::AppState;
    use crate::ui::player::PlayerPicker;
    use crate::ui::settings::SettingsState;

    fn test_app(label: &str) -> App {
        let root = std::env::temp_dir().join(format!(
            "harbour-settings-picker-test-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        let config = Config {
            download_dir: root.join("dl"),
            ..Config::default()
        };
        App {
            state: AppState::default(),
            queue: Queue::new(Arc::new(FakeEngine::new()), 0),
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
            settings_open: true,
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
        }
    }

    #[test]
    fn enter_on_the_player_row_opens_the_picker_overlay() {
        let mut app = test_app("player-row");
        app.settings.selected = 0;
        assert_eq!(
            crate::ui::settings::row_label(0),
            Some("Video Player (click / enter)")
        );

        settings_activate(&mut app);

        assert!(
            app.picker.open,
            "settings player row must open the same shift+P overlay"
        );
        assert!(
            !app.settings.editing,
            "player is a picker, not an inline text field"
        );
        assert!(app.picker_pending.is_none(), "no watch is waiting");
    }
}
