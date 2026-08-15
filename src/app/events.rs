//! Input dispatch: turns terminal events into actions (`handle_event`) and
//! actions into state changes (`apply_action`).

use crossterm::event::{Event, MouseButton, MouseEventKind};

use crate::input::Action;
use crate::ui::FolderPromptMode;
use crate::ui::Screen;
use crate::ui::player::PickerMode;

use super::actions::{
    download_selected, move_selection, remove_selected, retry_selected, toggle_pause,
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

    // The settings overlay is modal: while it is up, a click must not
    // reach the screen underneath (the overlay is painted over it).
    if app.settings_open || app.state.folder_prompt.open {
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
    let action = crate::input::mouse_to_action(
        app.state.screen,
        mouse_view_area(app),
        mouse.column,
        mouse.row,
        app.help_open,
        app.state.downloads.show_seeding,
    );
    apply_action(app, action).await;
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
                    _ => app.state.search.selected = index,
                }
            }
        }
        Action::MoveUp => move_selection(app, -1),
        Action::MoveDown => move_selection(app, 1),
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
            if app.settings_open {
                // First Esc exits an inline edit, the second closes the
                // overlay — the hint's "esc back" is two steps, never a
                // lost edit.
                if app.settings.editing {
                    app.settings.editing = false;
                    app.settings.edit_buffer.clear();
                } else {
                    app.settings_open = false;
                }
            } else if app.picker.open {
                // Closing the picker also drops a watch waiting on it.
                app.picker.open = false;
                app.picker_pending = None;
            } else if app.help_open {
                app.help_open = false;
            } else if !app.state.search.query.is_empty() {
                app.state.search.query.clear();
            } else {
                app.state.error_banner = None;
            }
        }
        Action::Submit => {
            let query = app.state.search.query.clone();
            app.start_search(query);
            // Enter hands the keyboard to the results pane: plain keys act
            // on the selected row from here (d/w/s/?).
            app.state.search.focus = false;
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
    }
}
