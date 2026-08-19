//! Input dispatch: turns terminal events into actions (`handle_event`) and
//! actions into state changes (`apply_action`).

use std::time::{Duration, Instant};

use crossterm::event::{Event, MouseButton, MouseEventKind};

use crate::input::Action;
use crate::ui::FolderPromptMode;
use crate::ui::Screen;
use crate::ui::player::PickerMode;

use super::actions::{
    download_selected, enqueue_magnet, enqueue_torrent, move_selection, move_selection_to,
    remove_selected, retry_selected, toggle_pause,
};
use super::settings::{
    cancel_folder_prompt, commit_folder_prompt, open_folder_prompt, settings_activate,
};
use super::watch::{
    choose_player, end_watch, open_player_picker, start_watch, start_watch_ephemeral,
};
use super::{App, mouse_view_area};

async fn handle_mouse_event(app: &mut App, mouse: crossterm::event::MouseEvent) {
    // Track mouse position on any mouse movement or interaction.
    app.state.mouse_pos = Some((mouse.column, mouse.row));

    // Handle mouse wheel scrolling (scrolls by a page of ~8 rows for fast browsing):
    if app.episode_picker.open {
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = ratatui::layout::Rect::new(0, 0, term_w, term_h);
        if mouse.kind == MouseEventKind::ScrollDown {
            app.episode_picker.select_next();
            return;
        }
        if mouse.kind == MouseEventKind::ScrollUp {
            app.episode_picker.select_prev();
            return;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(ep_idx) = crate::ui::episode_picker::episode_at_mouse(
                &app.episode_picker,
                area,
                mouse.column,
                mouse.row,
            ) {
                apply_action(app, Action::EpisodeChoose(Some(ep_idx))).await;
            } else {
                app.episode_picker.open = false;
            }
        }
        return;
    }

    if app.batch_picker.open {
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = ratatui::layout::Rect::new(0, 0, term_w, term_h);
        if mouse.kind == MouseEventKind::ScrollDown {
            app.batch_picker.select_next();
            return;
        }
        if mouse.kind == MouseEventKind::ScrollUp {
            app.batch_picker.select_prev();
            return;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(file_idx) = crate::ui::batch_picker::file_at_mouse(
                &app.batch_picker,
                area,
                mouse.column,
                mouse.row,
            ) {
                app.batch_picker.selected = file_idx;
                app.batch_picker.toggle_index(file_idx);
            } else {
                app.batch_picker.open = false;
            }
        }
        return;
    }

    if app.picker.open {
        // The picker is on top of settings when opened from the player row;
        // clicks belong to it (keys already do), not the overlay underneath.
        return;
    }

    if mouse.kind == MouseEventKind::ScrollDown {
        if app.settings_open {
            apply_action(app, Action::SettingsMoveDown).await;
        } else {
            apply_action(app, Action::PageDown).await;
        }
        return;
    }
    if mouse.kind == MouseEventKind::ScrollUp {
        if app.settings_open {
            apply_action(app, Action::SettingsMoveUp).await;
        } else {
            apply_action(app, Action::PageUp).await;
        }
        return;
    }

    if app.settings_open {
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = ratatui::layout::Rect::new(0, 0, term_w, term_h);
        let panel = crate::ui::settings::panel_rect(area, crate::ui::settings::row_count());
        let in_panel = mouse.column >= panel.x
            && mouse.column < panel.right()
            && mouse.row >= panel.y
            && mouse.row < panel.bottom();

        if !in_panel {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                app.settings_open = false;
            }
            return;
        }

        // Check if clicking [✕] button on top border:
        if mouse.row == panel.y && mouse.column >= panel.right().saturating_sub(6) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                app.settings_open = false;
            }
            return;
        }

        // Row hit test:
        let rel_row = mouse.row.saturating_sub(panel.y + 1);
        let row_idx = rel_row.saturating_sub(1) as usize;
        if rel_row > 0 && row_idx < crate::ui::settings::row_count() {
            app.settings.selected = row_idx;
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                settings_activate(app);
            }
        }
        return;
    }
    if app.state.folder_prompt.open {
        return;
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    // Dismiss error banner if clicked directly or on its [✕ dismiss] button
    if app.state.error_banner.is_some() {
        let (_, term_height) = crossterm::terminal::size().unwrap_or((0, 0));
        let banner_h = super::banner_height(app.state.error_banner.as_deref());
        let banner_top = term_height.saturating_sub(banner_h + 1);
        let banner_bottom = term_height.saturating_sub(1);
        if mouse.row >= banner_top && mouse.row < banner_bottom {
            app.state.error_banner = None;
            return;
        }
    }

    // Build the action first: `mouse_to_action` borrows `app.state`
    // only to read the screen, and the awaited apply takes `app`
    // mutably — the two cannot overlap.
    let view = mouse_view_area(app);
    let single = crate::input::mouse_to_action(
        app.state.screen,
        view,
        mouse.column,
        mouse.row,
        app.help_open,
        app.state.downloads.show_seeding,
    );
    let double = search_result_double_click(app, &single);
    if double {
        // Select the clicked row before Download so a double-click on row
        // N enqueues N, not whatever the keyboard cursor was on.
        apply_action(app, single).await;
    }
    let action = crate::input::mouse_to_action_click(
        app.state.screen,
        view,
        mouse.column,
        mouse.row,
        app.help_open,
        app.state.downloads.show_seeding,
        double,
    );
    apply_action(app, action).await;
}

/// Crossterm never reports a double-click kind; two left-downs on the same
/// in-range search result within this window count as download (#71).
const SEARCH_DOUBLE_CLICK: Duration = Duration::from_millis(500);

fn search_result_double_click(app: &mut App, action: &Action) -> bool {
    let Action::ClickRow(idx) = *action else {
        app.last_search_click = None;
        return false;
    };
    if app.state.screen != Screen::Search || idx >= app.state.search.results.len() {
        app.last_search_click = None;
        return false;
    }
    let is_double = app
        .last_search_click
        .is_some_and(|(at, prev)| prev == idx && at.elapsed() < SEARCH_DOUBLE_CLICK);
    app.last_search_click = if is_double {
        None
    } else {
        Some((Instant::now(), idx))
    };
    is_double
}

/// Turns one terminal event into state changes.
pub(crate) async fn handle_event(app: &mut App, event: Event) {
    // A left-button press is a click. Releases, drags, scrolls, and other
    // buttons carry no selection intent; ignoring them keeps a scroll wheel
    // from firing row selections while the user browses.
    if let Event::Mouse(mouse) = event {
        handle_mouse_event(app, mouse).await;
        return;
    }
    if let Event::Paste(text) = event {
        open_or_type_paste(app, &text).await;
        return;
    }
    let Event::Key(key) = event else {
        // Resize events need no handling: ratatui re-lays out from the frame
        // size on every draw.
        return;
    };
    // Ignore key release events (Windows reports both press and release);
    // keep Press and Repeat.
    if key.kind == crossterm::event::KeyEventKind::Release {
        return;
    }

    let action = crate::input::map_with_focus(
        key,
        app.state.screen,
        crate::input::FocusFlags {
            help_open: app.help_open,
            picker_open: app.picker.open,
            picker_custom: app.picker.mode == PickerMode::Custom,
            episode_picker_open: app.episode_picker.open,
            batch_picker_open: app.batch_picker.open,
            settings_open: app.settings_open,
            folder_open: app.state.folder_prompt.open,
            search_focus: app.state.search.focus,
        },
    );

    apply_action(app, action).await;
}

async fn apply_action(app: &mut App, action: Action) {
    match action {
        Action::None => {}
        Action::Quit => app.quitting = true,
        Action::Dismiss => {
            app.state.screen = Screen::Search;
            if app.state.search.results.is_empty()
                && !app.state.search.searching
                && app.state.search.query.is_empty()
            {
                app.start_search(String::new());
            }
        }
        Action::ToggleHelp => app.help_open = !app.help_open,
        Action::SwitchScreen => {
            app.state.screen = match app.state.screen {
                Screen::Downloads => Screen::Search,
                _ => Screen::Downloads,
            };
            app.state.error_banner = None;
        }
        Action::ToggleSeeding | Action::ClickSeedingTab => {
            app.state.downloads.show_seeding = !app.state.downloads.show_seeding;
            app.state.downloads.selected = 0;
        }
        Action::ClickRow(index) => {
            // The mouse mapping only knows the view geometry, so the list's
            // real length decides whether the click lands on a row. Unlike
            // the keyboard cursor there is no wraparound — a click past the
            // last row is a no-op, and clicking an empty list does nothing.
            let len = match app.state.screen {
                Screen::Downloads => app.visible_items().len(),
                _ => app.state.search.results.len(),
            };
            if index < len {
                match app.state.screen {
                    Screen::Downloads => app.state.downloads.selected = index,
                    _ => {
                        app.state.search.selected = index;
                        // A click on a result owns the keyboard, so `d`
                        // downloads and the footer shows the watch/download
                        // hint instead of the typing hint (#71).
                        app.state.search.focus = false;
                    }
                }
            }
        }
        Action::MoveUp => move_selection(app, -1),
        Action::MoveDown => move_selection(app, 1),
        Action::PageUp => {
            let page = super::actions::page_size() as isize;
            move_selection(app, -page);
        }
        Action::PageDown => {
            let page = super::actions::page_size() as isize;
            move_selection(app, page);
        }
        Action::MoveHome => move_selection_to(app, 0),
        Action::MoveEnd => move_selection_to(app, usize::MAX),
        Action::Backspace => {
            app.state.search.query.pop();
        }
        Action::Escape => {
            // A visible error banner goes away on the first Esc — the user
            // has seen it; it must never need a screen-switch to clear.
            if app.state.error_banner.is_some() {
                app.state.error_banner = None;
                return;
            }
            if app.picker.open {
                // Closing the picker also drops a watch waiting on it.
                app.picker.open = false;
                app.picker_pending = None;
            } else if app.settings_open {
                // First Esc exits an inline edit, the second closes the
                // overlay — the hint's "esc back" is two steps, never a
                // lost edit.
                if app.settings.editing {
                    app.settings.editing = false;
                    app.settings.edit_buffer.clear();
                } else {
                    app.settings_open = false;
                }
            } else if app.help_open {
                app.help_open = false;
            } else if !app.state.search.query.is_empty() {
                app.state.search.query.clear();
            } else {
                app.state.error_banner = None;
            }
        }
        Action::Submit => {
            if app.state.search.focus {
                let query = app.state.search.query.clone();
                if query.trim().is_empty() && !app.state.search.results.is_empty() {
                    match app.state.screen {
                        Screen::Search => start_watch_ephemeral(app).await,
                        _ => start_watch(app).await,
                    }
                } else if try_open_dropped(app, &query).await {
                    app.state.search.query.clear();
                    app.state.search.focus = false;
                } else {
                    app.start_search(query);
                    app.state.search.focus = false;
                }
            } else {
                match app.state.screen {
                    Screen::Search => start_watch_ephemeral(app).await,
                    _ => start_watch(app).await,
                }
            }
        }
        Action::FocusSearchInput => {
            app.state.search.focus = true;
            // Leaving the results pane means the banner has been seen —
            // one Esc from anywhere dismisses it.
            app.state.error_banner = None;
        }
        Action::Type(c) => {
            // Typing from the results pane returns focus to the input and
            // types there (fzf-style refinement).
            app.state.search.focus = true;
            app.state.search.query.push(c);
        }
        Action::Download => download_selected(app).await,
        Action::DownloadToFolder => open_folder_prompt(app, FolderPromptMode::DownloadTo),
        Action::ChangeDefaultFolder => open_folder_prompt(app, FolderPromptMode::SetDefault),
        Action::FolderType(c) => {
            if app.state.folder_prompt.open {
                app.state.folder_prompt.edit_buffer.push(c);
            }
        }
        Action::FolderBackspace => {
            if app.state.folder_prompt.open {
                app.state.folder_prompt.edit_buffer.pop();
            }
        }
        Action::FolderConfirm => commit_folder_prompt(app).await,
        Action::FolderCancel => cancel_folder_prompt(app),
        Action::TogglePause => toggle_pause(app).await,
        Action::Retry => retry_selected(app).await,
        Action::Remove => remove_selected(app).await,
        Action::Sort(col) => {
            app.state.search.toggle_sort(col);
        }
        Action::Watch => match app.state.screen {
            // Watch-now (2.3): stream the selected result without
            // downloading it to the library.
            Screen::Search => start_watch_ephemeral(app).await,
            _ => start_watch(app).await,
        },
        Action::EndWatch => end_watch(app).await,
        Action::OpenPlayerPicker => open_player_picker(app),
        Action::PlayerUp => {
            if app.picker.open && !app.picker.options.is_empty() {
                let len = app.picker.options.len();
                app.picker.selected = (app.picker.selected + len - 1) % len;
            }
        }
        Action::PlayerDown => {
            if app.picker.open && !app.picker.options.is_empty() {
                let len = app.picker.options.len();
                app.picker.selected = (app.picker.selected + 1) % len;
            }
        }
        Action::PlayerCustom => {
            if app.picker.open {
                app.picker.mode = PickerMode::Custom;
            }
        }
        Action::PlayerType(c) => {
            if app.picker.open && app.picker.mode == PickerMode::Custom {
                app.picker.custom.push(c);
            }
        }
        Action::PlayerBackspace => {
            if app.picker.open && app.picker.mode == PickerMode::Custom {
                app.picker.custom.pop();
            }
        }
        Action::PlayerChoose => choose_player(app).await,
        Action::ToggleSource(id) => {
            if app.disabled_sources.contains(&id) {
                app.disabled_sources.remove(&id);
            } else {
                app.disabled_sources.insert(id);
            }
            app.apply_source_filter();
        }
        Action::OpenSettings => {
            app.settings_open = !app.settings_open;
            if app.settings_open {
                // Refresh the theme list so the theme row cycles over what
                // is actually installed right now.
                app.settings.themes = crate::ui::settings::installed_themes();
            }
        }
        Action::SettingsMoveUp => {
            // Moving the selection discards an inline edit — the buffer
            // belongs to the row it was started on.
            app.settings.editing = false;
            app.settings.edit_buffer.clear();
            app.settings.selected = app.settings.selected.saturating_sub(1);
        }
        Action::SettingsMoveDown => {
            app.settings.editing = false;
            app.settings.edit_buffer.clear();
            let last = crate::ui::settings::row_count().saturating_sub(1);
            app.settings.selected = (app.settings.selected + 1).min(last);
        }
        Action::SettingsActivate => settings_activate(app),
        Action::SettingsType(c) => {
            if app.settings.editing {
                app.settings.edit_buffer.push(c);
            }
        }
        Action::SettingsBackspace => {
            if app.settings.editing {
                app.settings.edit_buffer.pop();
            }
        }
        Action::EpisodeUp => app.episode_picker.select_prev(),
        Action::EpisodeDown => app.episode_picker.select_next(),
        Action::EpisodePageUp => app.episode_picker.page_up(),
        Action::EpisodePageDown => app.episode_picker.page_down(),
        Action::EpisodeChoose(opt_idx) => super::watch::choose_episode(app, opt_idx).await,
        Action::EpisodeClose => {
            app.episode_picker.open = false;
        }
        Action::BatchUp => app.batch_picker.select_prev(),
        Action::BatchDown => app.batch_picker.select_next(),
        Action::BatchPageUp => app.batch_picker.page_up(),
        Action::BatchPageDown => app.batch_picker.page_down(),
        Action::BatchToggle(opt_idx) => {
            if let Some(idx) = opt_idx {
                app.batch_picker.toggle_index(idx);
            } else {
                app.batch_picker.toggle_selected();
            }
        }
        Action::BatchSelectAll => app.batch_picker.select_all(),
        Action::BatchUnselectAll => app.batch_picker.unselect_all(),
        Action::BatchInvert => app.batch_picker.invert_selection(),
        Action::BatchConfirm => super::actions::confirm_batch_download(app).await,
        Action::BatchClose => {
            app.batch_picker.open = false;
        }
        Action::ClearCompleted => super::actions::clear_completed(app).await,
        Action::OpenFolder => super::actions::open_selected_item(app),
        Action::CopyBugReport => {
            match crate::bugreport::share_bugreport(&crate::core::paths::state_dir()) {
                Ok((_, true)) => app.warn(crate::bugreport::COPIED_BANNER),
                Ok((path, false)) => {
                    app.warn(format!("bugreport written to {}", path.display()));
                }
                Err(err) => app.warn(format!("could not write bugreport: {err}")),
            }
        }
    }
}

/// Strip quotes / `file://` that Windows Terminal puts on a dropped path.
fn normalize_dropped(raw: &str) -> String {
    let s = raw.trim().trim_matches('"').trim_matches('\'');
    let s = s.strip_prefix("file://").unwrap_or(s);
    // Windows: file:///C:/foo → C:/foo
    if let Some(rest) = s.strip_prefix('/')
        && rest.len() >= 2
        && rest.as_bytes()[1] == b':'
    {
        return rest.to_string();
    }
    s.to_string()
}

/// True if `raw` was a magnet, infohash, or existing `.torrent` and we started it.
async fn try_open_dropped(app: &mut App, raw: &str) -> bool {
    let s = normalize_dropped(raw);
    if s.is_empty() {
        return false;
    }
    if s.to_ascii_lowercase().starts_with("magnet:?") {
        enqueue_magnet(app, &s).await;
        return true;
    }
    if let Some(hash) = crate::core::magnet::normalize_info_hash(&s) {
        let magnet = crate::core::magnet::build_magnet(&hash, &hash);
        enqueue_magnet(app, &magnet).await;
        return true;
    }
    let path = std::path::Path::new(&s);
    if s.to_ascii_lowercase().ends_with(".torrent") && path.is_file() {
        enqueue_torrent(app, path).await;
        return true;
    }
    false
}

async fn open_or_type_paste(app: &mut App, text: &str) {
    if try_open_dropped(app, text).await {
        return;
    }
    app.state.search.focus = true;
    app.state.search.query.push_str(text.trim());
}
