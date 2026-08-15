//! Input handling: key and mouse events in, [`Action`] out.
//!
//! Deliberately pure functions of `(screen, …)`. The app loop performs the
//! actions; nothing here touches the engine, the queue, or the terminal. That
//! is what makes the keymap and the click mapping testable without a TUI, and
//! it is why the keybind tests below can assert `UR-10` directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Margin, Rect};

use crate::core::types::SourceId;
use crate::ui::Screen;
use crate::ui::search::{SEARCH_BAR_H, SIDEBAR_WIDTH, sidebar_source_at};

/// Everything the user can ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Leave the splash for the search screen.
    Dismiss,
    ToggleHelp,
    /// Move between the search and downloads screens.
    SwitchScreen,
    ToggleSeeding,
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    MoveHome,
    MoveEnd,
    /// Run the current query.
    Submit,
    /// Append to the query.
    Type(char),
    Backspace,
    /// Clear the query, or close an overlay.
    Escape,
    /// Return focus from the search results pane to the input pane.
    FocusSearchInput,
    /// Download the highlighted result.
    Download,
    /// Download the highlighted result into a folder you pick (shift+D,
    /// FR-29) — opens the folder prompt.
    DownloadToFolder,
    /// Change + persist the default download folder (`o`, FR-40) — opens the
    /// folder prompt in set-default mode.
    ChangeDefaultFolder,
    /// Append a character to the folder-prompt path.
    FolderType(char),
    /// Delete the last character from the folder-prompt path.
    FolderBackspace,
    /// Commit the folder prompt: download to the entered folder (shift+D)
    /// or persist it as the new default (`o`).
    FolderConfirm,
    /// Close the folder prompt without committing.
    FolderCancel,
    /// Pause or resume the highlighted item.
    TogglePause,
    /// Retry the highlighted failed item.
    Retry,
    /// Forget the highlighted item, keeping its files.
    Remove,
    /// Watch the highlighted item: stream it to an external player (FR-57).
    Watch,
    /// Leave the now-playing screen back to the TUI (FR-59).
    EndWatch,
    // --- Player picker (2.1): choose/override the watch player in the TUI ---
    /// Open the player picker overlay.
    OpenPlayerPicker,
    /// Move the picker selection up.
    PlayerUp,
    /// Move the picker selection down.
    PlayerDown,
    /// Use the selected (list mode) or entered (custom mode) player.
    PlayerChoose,
    /// Switch the picker to custom-path entry.
    PlayerCustom,
    /// Append a character to the custom player path.
    PlayerType(char),
    /// Backspace the custom player path.
    PlayerBackspace,
    /// Select the visible row under a left click — a search result or a
    /// downloads item (UR-13). The index is clamped by the app loop, which
    /// knows the real list length; a click past the last row is a no-op.
    ClickRow(usize),
    /// Toggle the downloads seeding tab from a click (same effect as
    /// `ToggleSeeding`).
    ClickSeedingTab,
    /// Enable/disable a source from the search sidebar (2.2): disabled
    /// sources are never queried or merged, and the choice persists.
    ToggleSource(SourceId),
    // --- Settings (2.5): everything in Config editable from the TUI ---
    /// Open or close the settings overlay.
    OpenSettings,
    /// Move the settings selection up.
    SettingsMoveUp,
    /// Move the settings selection down.
    SettingsMoveDown,
    /// Activate the selected row: enter/commit a text edit, cycle the
    /// theme, or flip a toggle.
    SettingsActivate,
    /// Append a character to the settings text-edit buffer.
    SettingsType(char),
    /// Delete the last character from the settings text-edit buffer.
    SettingsBackspace,
}

/// Ctrl-C always quits, everywhere — a terminal convention users rely on more
/// than any binding we invent.
fn is_hard_quit(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

/// Maps one keypress to an action.
///
/// `help_open` is separate from `screen` because the overlay floats above
/// whatever is underneath: closing it must return the user exactly where they
/// were rather than to a default screen. The player picker (`picker_open`)
/// floats the same way; `picker_custom` tells the picker branch whether the
/// user is entering a custom player path (where `c` and other letters are
/// input) or browsing the installed-player list (where `c` switches modes).
/// The settings overlay (`settings_open`) is the same modal shape again:
/// `esc`/arrows/enter/typing all belong to it while it is up.
/// The overlay/input flags that drive focus-aware key mapping (FR-29/40).
///
/// Folded into one struct so [`map_with_focus`] stays under the FR-67
/// parameter ceiling — eight bare booleans is a review-sheet smell.
#[derive(Debug, Clone, Copy, Default)]
pub struct FocusFlags {
    /// The `?` help overlay owns every key while up.
    pub help_open: bool,
    /// The player picker owns every key while up.
    pub picker_open: bool,
    /// The picker is in custom-path entry mode (typing edits the path).
    pub picker_custom: bool,
    /// The settings overlay owns every key while up.
    pub settings_open: bool,
    /// The folder prompt (shift+d / o) owns every key while up.
    pub folder_open: bool,
    /// The search screen's input pane is focused (true: every key types).
    pub search_focus: bool,
}

/// Maps one keypress to an action with the search input pane focused (the
/// default). Test-only convenience: shipped code calls [`map_with_focus`].
#[cfg(test)]
pub fn map(
    key: KeyEvent,
    screen: Screen,
    help_open: bool,
    picker_open: bool,
    picker_custom: bool,
    settings_open: bool,
) -> Action {
    map_with_focus(
        key,
        screen,
        FocusFlags {
            help_open,
            picker_open,
            picker_custom,
            settings_open,
            search_focus: true,
            ..FocusFlags::default()
        },
    )
}

/// The focus-aware keymap. `search_focus` selects the search screen's input
/// pane (true: every key types) or results pane (false: plain keys act).
/// `folder_open` mirrors `settings_open`: while the folder prompt (FR-29/40)
/// is up it owns every key — typing edits the path, Enter commits, Esc
/// cancels — exactly like the settings overlay's inline edit.
pub fn map_with_focus(key: KeyEvent, screen: Screen, flags: FocusFlags) -> Action {
    let FocusFlags {
        help_open,
        picker_open,
        picker_custom,
        settings_open,
        folder_open,
        search_focus,
    } = flags;
    if is_hard_quit(&key) {
        return Action::Quit;
    }

    if help_open {
        // Any key dismisses the overlay. Someone who opened it by accident
        // should not have to work out which key closes it.
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            _ => Action::ToggleHelp,
        };
    }

    if picker_open {
        // The picker owns every key while it is up, exactly like help: Esc
        // closes it (leaving the user where they were), arrows/Enter drive
        // the list, `c` switches to custom-path entry, and in custom mode
        // typing and Backspace edit the path. `q` still quits.
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => Action::Escape,
            KeyCode::Up | KeyCode::Char('k') => Action::PlayerUp,
            KeyCode::Down | KeyCode::Char('j') => Action::PlayerDown,
            KeyCode::Enter => Action::PlayerChoose,
            KeyCode::Backspace if picker_custom => Action::PlayerBackspace,
            KeyCode::Char('c') if !picker_custom => Action::PlayerCustom,
            KeyCode::Char(c) if picker_custom => Action::PlayerType(c),
            _ => Action::None,
        };
    }

    if settings_open {
        // The settings overlay owns every key while it is up, exactly like
        // help and the picker: Esc closes it (returning the user where they
        // were), arrows/Enter drive the rows, and typing edits the inline
        // buffer of a text row (the app gates it on edit mode). `q` quits.
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => Action::Escape,
            KeyCode::Up | KeyCode::Char('k') => Action::SettingsMoveUp,
            KeyCode::Down | KeyCode::Char('j') => Action::SettingsMoveDown,
            KeyCode::Enter => Action::SettingsActivate,
            KeyCode::Backspace => Action::SettingsBackspace,
            KeyCode::Char(c) => Action::SettingsType(c),
            _ => Action::None,
        };
    }

    if folder_open {
        // The folder prompt owns every key while it is up, exactly like the
        // overlays above: typing edits the path, Backspace deletes, Enter
        // commits, Esc cancels. `q` still quits (the overlays' convention).
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => Action::FolderCancel,
            KeyCode::Enter => Action::FolderConfirm,
            KeyCode::Backspace => Action::FolderBackspace,
            KeyCode::Char(c) => Action::FolderType(c),
            _ => Action::None,
        };
    }

    match screen {
        // The splash is an intro, not a state: anything moves past it.
        Screen::Splash => match key.code {
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Dismiss,
        },

        // The search screen is a two-pane focus model (the fzf convention):
        // the input pane types EVERY key — D, W, S, P and `?` are plain
        // characters, so no modifier ever hijacks typing "Dune". Enter runs
        // the search and moves focus to the results pane, where plain keys
        // act on the selected row: d download, w watch-now, s settings,
        // ? help. Esc returns focus to the input, and typing any printable
        // from the results jumps back to the input and types it (instant
        // refinement, fzf-style).
        Screen::Search if search_focus => match key.code {
            KeyCode::Enter => Action::Submit,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Down => Action::MoveDown,
            KeyCode::PageUp => Action::PageUp,
            KeyCode::PageDown => Action::PageDown,
            KeyCode::Tab => Action::SwitchScreen,
            KeyCode::Esc => Action::Escape,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Char(c) => Action::Type(c),
            _ => Action::None,
        },

        Screen::Search => match key.code {
            KeyCode::Char('d') => Action::Download,
            KeyCode::Char('D') => Action::DownloadToFolder,
            KeyCode::Char('o') => Action::ChangeDefaultFolder,
            KeyCode::Char('w') | KeyCode::Enter => Action::Watch,
            KeyCode::Char('s') => Action::OpenSettings,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Down => Action::MoveDown,
            KeyCode::PageUp => Action::PageUp,
            KeyCode::PageDown => Action::PageDown,
            KeyCode::Home => Action::MoveHome,
            KeyCode::End => Action::MoveEnd,
            KeyCode::Tab => Action::SwitchScreen,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('/') => Action::FocusSearchInput,
            KeyCode::Char(c) => Action::Type(c),
            _ => Action::None,
        },

        Screen::Downloads => match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Tab | KeyCode::Esc => Action::SwitchScreen,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Down => Action::MoveDown,
            KeyCode::PageUp => Action::PageUp,
            KeyCode::PageDown => Action::PageDown,
            KeyCode::Home => Action::MoveHome,
            KeyCode::End => Action::MoveEnd,
            KeyCode::Left | KeyCode::Right => Action::ToggleSeeding,
            KeyCode::Char('s') => Action::ToggleSeeding,
            KeyCode::Char('p') => Action::TogglePause,
            KeyCode::Char('P') => Action::OpenPlayerPicker,
            KeyCode::Char('S') => Action::OpenSettings,
            KeyCode::Char('o') => Action::ChangeDefaultFolder,
            KeyCode::Char('r') => Action::Retry,
            KeyCode::Char('x') => Action::Remove,
            KeyCode::Char('w') => Action::Watch,
            _ => Action::None,
        },

        // Watch mode: q/esc end the session and return to the TUI (FR-59);
        // they never quit the app. Ctrl+C still quits, handled above.
        Screen::NowPlaying => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::EndWatch,
            _ => Action::None,
        },

        // Reachable only as a state; the overlay branches above handle its
        // keys.
        Screen::Help => Action::ToggleHelp,
        Screen::Settings => Action::None,
    }
}

/// Actions available from the search screen when the query box is empty.
///
/// Maps a left click at terminal coordinates `(col, row)` inside `view_area`
/// to an action, or `Action::None` when the click lands on nothing clickable.
///
/// `view_area` is the area the current screen draws into — the full terminal
/// width minus the status rows (`app.rs`'s `draw` layout). `col`/`row` are
/// 0-based terminal coordinates, as crossterm reports them, so they line up
/// with ratatui `Rect` positions directly.
///
/// `help_open` mirrors `map`'s overlay flag: with the help modal open, any
/// click dismisses it no matter where it lands (see the `map` help branch).
pub fn mouse_to_action(
    screen: Screen,
    view_area: Rect,
    col: u16,
    row: u16,
    help_open: bool,
    show_seeding: bool,
) -> Action {
    if help_open {
        // The modal is dismissible and position carries no meaning: a click
        // anywhere — on the modal or on the screen behind it — closes help
        // and returns the user exactly where they were. This wins over row
        // selection, mirroring `map`: with help open, a click never selects
        // the row underneath the overlay.
        return Action::ToggleHelp;
    }

    // Status bar row clicks on the tab buttons (exactly 1 line below view_area):
    let is_status_row = row == view_area.y + view_area.height;
    if is_status_row && col >= view_area.x && col < view_area.right() {
        if let Some(tab) = crate::ui::status::status_button_at(col, view_area.width) {
            return match tab {
                crate::ui::status::StatusTab::Search => {
                    if screen == Screen::Search {
                        Action::FocusSearchInput
                    } else {
                        Action::Dismiss
                    }
                }
                crate::ui::status::StatusTab::Downloads => Action::SwitchScreen,
                crate::ui::status::StatusTab::Settings => Action::OpenSettings,
                crate::ui::status::StatusTab::Help => Action::ToggleHelp,
            };
        }
    }

    match screen {
        Screen::Search => {
            // Mirrors `ui::search::draw`'s layout: a 1-cell panel border,
            // then a 22-wide sidebar; the main column starts after both.
            // Inside it, the search bar is SEARCH_BAR_H rows, the results
            // header 1 row, then one result row per line. The hint line
            // owns the last inner row, and the panel's bottom border sits
            // one row below that.
            let inner_x = view_area.x + 1;
            let main_col = inner_x + SIDEBAR_WIDTH;
            let results_top = view_area.y + 1 + SEARCH_BAR_H + 1;
            let results_bottom = view_area.y + view_area.height.saturating_sub(2);
            if col < inner_x {
                // The panel's left border is not clickable.
                Action::None
            } else if col < main_col {
                // A click on a sidebar source row toggles that source (2.2).
                // The "Sources" title, group dividers, and rows past the last
                // source map to None — nothing to toggle there.
                sidebar_source_at(row.saturating_sub(view_area.y + 1))
                    .map_or(Action::None, Action::ToggleSource)
            } else if row < results_top {
                // The panel top border and the search bar: a click here means
                // "I want to type" — focus the input pane.
                Action::FocusSearchInput
            } else if row >= results_bottom {
                Action::None
            } else {
                Action::ClickRow((row - results_top) as usize)
            }
        }
        Screen::Downloads => {
            // Mirrors `ui::downloads::draw`'s layout: a 1-cell panel
            // border, then a 2-row tab band (label row + underline row)
            // at the top of the inner area, then the body, with the hint
            // line on the last inner row. A click anywhere on the tab
            // band flips the seeding tab.
            //
            // Row height depends on the active tab: the Downloads tab
            // renders two rows per item (name + progress bar), the Seeding
            // tab one (seed_row). `show_seeding` picks the divisor.
            let inner = view_area.inner(Margin::new(1, 1));
            if col < inner.x || col >= inner.right() {
                Action::None
            } else {
                let items_top = inner.y + 2;
                let rows_per_item: u16 = if show_seeding { 1 } else { 2 };
                match row {
                    r if r < inner.y => Action::None,
                    r if r < items_top => Action::ClickSeedingTab,
                    r if r >= inner.bottom().saturating_sub(1) => Action::None,
                    r => Action::ClickRow(((r - items_top) / rows_per_item) as usize),
                }
            }
        }
        // The splash is an intro, not a state: anything moves past it,
        // clicks included.
        Screen::Splash => Action::Dismiss,
        // Watch mode: a click ends the session and returns to the TUI,
        // the same action q/esc map to (FR-59).
        Screen::NowPlaying => Action::EndWatch,
        // Reachable only as a state; the `help_open` branch above handles
        // the real overlay. Mirror the keymap: a click closes it.
        Screen::Help => Action::ToggleHelp,
        // Reachable only as a state; the settings overlay is driven by
        // `settings_open` (app.rs guards clicks while it is up).
        Screen::Settings => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_c_quits_from_everywhere_including_the_help_overlay() {
        for screen in [Screen::Splash, Screen::Search, Screen::Downloads] {
            assert_eq!(
                map(ctrl('c'), screen, false, false, false, false),
                Action::Quit,
                "{screen:?}"
            );
            assert_eq!(
                map(ctrl('c'), screen, true, false, false, false),
                Action::Quit,
                "{screen:?} + help"
            );
        }
    }

    #[test]
    fn typing_on_the_search_screen_never_triggers_an_action() {
        // Input pane: EVERY printable key types — including `?`, capitals,
        // and letters that act elsewhere. No modifier is ever needed to type
        // "Dune" or "warcraft".
        for c in "dunepqrsx?DWS".chars() {
            assert_eq!(
                map(
                    key(KeyCode::Char(c)),
                    Screen::Search,
                    false,
                    false,
                    false,
                    false
                ),
                Action::Type(c),
                "`{c}` must reach the text field in the input pane"
            );
        }
    }

    #[test]
    fn the_results_pane_binds_plain_letters() {
        // After Enter, the results pane maps plain keys to actions on the
        // selected row — no shift-anything needed.
        let r = |code| map_with_focus(key(code), Screen::Search, FocusFlags::default());
        assert_eq!(r(KeyCode::Char('d')), Action::Download);
        assert_eq!(r(KeyCode::Char('w')), Action::Watch);
        assert_eq!(r(KeyCode::Char('s')), Action::OpenSettings);
        assert_eq!(r(KeyCode::Char('?')), Action::ToggleHelp);
        assert_eq!(r(KeyCode::Char('q')), Action::Quit);
        assert_eq!(
            map_with_focus(key(KeyCode::Esc), Screen::Search, FocusFlags::default(),),
            Action::FocusSearchInput
        );
        assert_eq!(
            map_with_focus(
                key(KeyCode::Backspace),
                Screen::Search,
                FocusFlags::default(),
            ),
            Action::FocusSearchInput,
            "backspace is the reflexive way out of the results pane"
        );
        // Any other printable returns to the input and types there.
        assert_eq!(r(KeyCode::Char('x')), Action::Type('x'));
        assert_eq!(
            r(KeyCode::Char('Z')),
            Action::Type('Z'),
            "capitals still type"
        );
        // Enter watches the selected row (the Stremio flow); arrows navigate,
        // Tab switches — same as input pane.
        assert_eq!(r(KeyCode::Enter), Action::Watch);
        assert_eq!(r(KeyCode::Up), Action::MoveUp);
        assert_eq!(r(KeyCode::Down), Action::MoveDown);
        assert_eq!(r(KeyCode::Tab), Action::SwitchScreen);
    }

    #[test]
    fn shift_s_opens_settings_from_downloads_only() {
        assert_eq!(
            map(
                key(KeyCode::Char('S')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::OpenSettings
        );
        // Lowercase s stays the seeding-tab toggle.
        assert_eq!(
            map(
                key(KeyCode::Char('s')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::ToggleSeeding
        );
        // On Search, shift+S is just a capital S — the input pane types it.
        assert_eq!(
            map(
                key(KeyCode::Char('S')),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::Type('S')
        );
    }

    #[test]
    fn the_settings_overlay_binds_its_keys() {
        for (code, expected) in [
            (KeyCode::Up, Action::SettingsMoveUp),
            (KeyCode::Char('k'), Action::SettingsMoveUp),
            (KeyCode::Down, Action::SettingsMoveDown),
            (KeyCode::Char('j'), Action::SettingsMoveDown),
            (KeyCode::Enter, Action::SettingsActivate),
            (KeyCode::Backspace, Action::SettingsBackspace),
            (KeyCode::Esc, Action::Escape),
            (KeyCode::Char('q'), Action::Quit),
        ] {
            assert_eq!(
                map(key(code), Screen::Downloads, false, false, false, true),
                expected,
                "{code:?} with settings open"
            );
        }
        // Typing edits the buffer — every printable key, on any screen,
        // including keys that would otherwise be screen bindings.
        assert_eq!(
            map(
                key(KeyCode::Char('x')),
                Screen::Downloads,
                false,
                false,
                false,
                true
            ),
            Action::SettingsType('x')
        );
        assert_eq!(
            map(
                key(KeyCode::Char('?')),
                Screen::Search,
                false,
                false,
                false,
                true
            ),
            Action::SettingsType('?')
        );
        // The overlay never leaks a key to the screen underneath.
        assert_eq!(
            map(key(KeyCode::Tab), Screen::Search, false, false, false, true),
            Action::None
        );
        // Help and the picker are checked first: if either is open the key
        // belongs to it, mirroring the overlay ordering in `map`.
        assert_eq!(
            map(
                key(KeyCode::Char('x')),
                Screen::Downloads,
                true,
                false,
                false,
                true
            ),
            Action::ToggleHelp
        );
    }

    #[test]
    fn shift_p_opens_the_player_picker_from_downloads() {
        assert_eq!(
            map(
                key(KeyCode::Char('P')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::OpenPlayerPicker
        );
        // Lowercase p stays the pause binding.
        assert_eq!(
            map(
                key(KeyCode::Char('p')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::TogglePause
        );
    }

    #[test]
    fn the_player_picker_binds_list_keys() {
        // List mode: arrows/Enter drive the list, `c` switches to custom
        // entry, Esc closes, q quits, and any other letter does nothing
        // rather than typing into the screen behind the overlay.
        for (code, expected) in [
            (KeyCode::Up, Action::PlayerUp),
            (KeyCode::Char('k'), Action::PlayerUp),
            (KeyCode::Down, Action::PlayerDown),
            (KeyCode::Char('j'), Action::PlayerDown),
            (KeyCode::Enter, Action::PlayerChoose),
            (KeyCode::Char('c'), Action::PlayerCustom),
            (KeyCode::Esc, Action::Escape),
            (KeyCode::Char('q'), Action::Quit),
        ] {
            assert_eq!(
                map(key(code), Screen::Downloads, false, true, false, false),
                expected,
                "{code:?} in list mode"
            );
        }
        assert_eq!(
            map(
                key(KeyCode::Char('x')),
                Screen::Downloads,
                false,
                true,
                false,
                false
            ),
            Action::None,
            "a letter other than c does nothing in list mode"
        );
    }

    #[test]
    fn the_player_picker_binds_custom_mode_keys() {
        // Custom mode: typing edits the path (`c` included — the path can
        // legitimately contain the letter), Backspace deletes, Enter
        // validates and uses it, Esc closes.
        assert_eq!(
            map(
                key(KeyCode::Char('c')),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::PlayerType('c')
        );
        assert_eq!(
            map(
                key(KeyCode::Char('C')),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::PlayerType('C')
        );
        assert_eq!(
            map(
                key(KeyCode::Backspace),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::PlayerBackspace
        );
        assert_eq!(
            map(
                key(KeyCode::Enter),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::PlayerChoose
        );
        assert_eq!(
            map(
                key(KeyCode::Esc),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::Escape
        );
        // `c` is typed, never a mode switch, in custom mode.
        assert_ne!(
            map(
                key(KeyCode::Char('c')),
                Screen::Downloads,
                false,
                true,
                true,
                false
            ),
            Action::PlayerCustom
        );
    }

    #[test]
    fn w_watches_and_q_ends_watch() {
        assert_eq!(
            map(
                key(KeyCode::Char('w')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::Watch
        );
        assert_eq!(
            map(
                key(KeyCode::Char('q')),
                Screen::NowPlaying,
                false,
                false,
                false,
                false
            ),
            Action::EndWatch
        );
        assert_eq!(
            map(
                key(KeyCode::Esc),
                Screen::NowPlaying,
                false,
                false,
                false,
                false
            ),
            Action::EndWatch
        );
        // Ctrl+C still quits from the watch screen.
        assert_eq!(
            map(ctrl('c'), Screen::NowPlaying, false, false, false, false),
            Action::Quit
        );
        // q must not quit from now-playing — it ends the session.
        assert_ne!(
            map(
                key(KeyCode::Char('q')),
                Screen::NowPlaying,
                false,
                false,
                false,
                false
            ),
            Action::Quit
        );
    }

    #[test]
    fn shift_d_downloads_to_a_folder_and_o_changes_the_default() {
        // FR-29/40: on the results pane, D downloads the selected row into a
        // folder you pick; o changes (and persists) the default download
        // folder. On Downloads, only o is bound — there is no search row to
        // download from there.
        let r = |code| map_with_focus(key(code), Screen::Search, FocusFlags::default());
        assert_eq!(r(KeyCode::Char('D')), Action::DownloadToFolder);
        assert_eq!(r(KeyCode::Char('o')), Action::ChangeDefaultFolder);
        assert_eq!(
            map(
                key(KeyCode::Char('o')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::ChangeDefaultFolder
        );
        assert_eq!(
            map(
                key(KeyCode::Char('D')),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::None
        );
        // The input pane still types capitals and o — "Dune" and "ocean"
        // must never trigger an action (the `typing_a_capital_d` regression).
        assert_eq!(
            map(
                key(KeyCode::Char('D')),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::Type('D')
        );
        assert_eq!(
            map(
                key(KeyCode::Char('o')),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::Type('o')
        );
    }

    #[test]
    fn the_folder_prompt_owns_its_keys_while_open() {
        // FR-29/40: while the folder prompt is up, every key belongs to it —
        // typing edits the path (o and s included — they are path letters,
        // never re-open or tab toggles), Backspace deletes, Enter commits,
        // Esc cancels, and q still quits. Screen bindings never leak through.
        let p = |code| {
            map_with_focus(
                key(code),
                Screen::Downloads,
                FocusFlags {
                    folder_open: true,
                    ..FocusFlags::default()
                },
            )
        };
        assert_eq!(p(KeyCode::Char('x')), Action::FolderType('x'));
        assert_eq!(
            p(KeyCode::Char('o')),
            Action::FolderType('o'),
            "o types a path letter while the prompt is open"
        );
        assert_eq!(
            p(KeyCode::Char('s')),
            Action::FolderType('s'),
            "s types too, never the seeding toggle"
        );
        assert_eq!(p(KeyCode::Backspace), Action::FolderBackspace);
        assert_eq!(p(KeyCode::Enter), Action::FolderConfirm);
        assert_eq!(p(KeyCode::Esc), Action::FolderCancel);
        assert_eq!(p(KeyCode::Char('q')), Action::Quit);
        assert_eq!(p(KeyCode::Up), Action::None, "arrows never leak through");
        assert_eq!(
            p(KeyCode::Tab),
            Action::None,
            "tab never switches screens under the prompt"
        );
    }

    #[test]
    fn left_and_right_arrows_switch_the_downloads_tabs() {
        // UR-10: arrows flip the Downloads/Seeding tab, exactly like s.
        for code in [KeyCode::Left, KeyCode::Right] {
            assert_eq!(
                map(key(code), Screen::Downloads, false, false, false, false),
                Action::ToggleSeeding,
                "{code:?} toggles the tab"
            );
        }
        // The search input pane is untouched: Right is unbound there, so it
        // neither toggles a tab nor hijacks typing.
        assert_eq!(
            map(
                key(KeyCode::Right),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::None
        );
    }

    #[test]
    fn typing_a_capital_d_never_downloads() {
        // The regression this guards: "Dune" starts with a capital D. In the
        // input pane it must type; the results pane's shift+D downloads to a
        // folder (FR-29), and only the plain `d` downloads to the default.
        assert_eq!(
            map(
                key(KeyCode::Char('D')),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::Type('D')
        );
        assert_eq!(
            map_with_focus(
                key(KeyCode::Char('d')),
                Screen::Search,
                FocusFlags::default(),
            ),
            Action::Download
        );
    }

    #[test]
    fn any_key_closes_the_help_overlay_and_returns_you_where_you_were() {
        for code in [
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char('x'),
            KeyCode::Up,
        ] {
            assert_eq!(
                map(key(code), Screen::Downloads, true, false, false, false),
                Action::ToggleHelp,
                "{code:?} should dismiss the overlay"
            );
        }
    }

    #[test]
    fn the_splash_is_dismissed_by_anything() {
        assert_eq!(
            map(
                key(KeyCode::Enter),
                Screen::Splash,
                false,
                false,
                false,
                false
            ),
            Action::Dismiss
        );
        assert_eq!(
            map(
                key(KeyCode::Char('x')),
                Screen::Splash,
                false,
                false,
                false,
                false
            ),
            Action::Dismiss
        );
        // Except `q`, which should still quit rather than making someone wait.
        assert_eq!(
            map(
                key(KeyCode::Char('q')),
                Screen::Splash,
                false,
                false,
                false,
                false
            ),
            Action::Quit
        );
    }

    #[test]
    fn the_downloads_screen_binds_its_own_letters() {
        let cases = [
            (KeyCode::Char('p'), Action::TogglePause),
            (KeyCode::Char('r'), Action::Retry),
            (KeyCode::Char('x'), Action::Remove),
            (KeyCode::Char('s'), Action::ToggleSeeding),
            (KeyCode::Char('q'), Action::Quit),
            (KeyCode::Char('?'), Action::ToggleHelp),
            (KeyCode::Tab, Action::SwitchScreen),
            (KeyCode::Up, Action::MoveUp),
            (KeyCode::Down, Action::MoveDown),
        ];
        for (code, expected) in cases {
            assert_eq!(
                map(key(code), Screen::Downloads, false, false, false, false),
                expected,
                "{code:?}"
            );
        }
    }

    #[test]
    fn unbound_keys_do_nothing_rather_than_something_surprising() {
        assert_eq!(
            map(
                key(KeyCode::F(5)),
                Screen::Downloads,
                false,
                false,
                false,
                false
            ),
            Action::None
        );
        assert_eq!(
            map(
                key(KeyCode::Insert),
                Screen::Search,
                false,
                false,
                false,
                false
            ),
            Action::None
        );
    }

    #[test]
    fn every_action_the_keymap_can_produce_is_documented_in_the_help() {
        // UR-10: the overlay must show exactly what the app implements.
        let documented: Vec<&str> = crate::ui::help::BINDINGS.iter().map(|(k, _)| *k).collect();
        for key in [
            "enter", "↑ ↓", "← →", "d", "shift+D", "o", "tab", "s", "p", "r", "x", "esc", "?", "q",
        ] {
            assert!(
                documented.contains(&key),
                "`{key}` is missing from the help overlay"
            );
        }
    }

    /// A search-view area with no status bar: 30 rows of 100 columns.
    fn search_view() -> Rect {
        Rect::new(0, 0, 100, 30)
    }

    /// The terminal row where the first search result renders: inner top
    /// border (1) + search bar (3) + results header (1).
    fn results_top() -> u16 {
        1 + SEARCH_BAR_H + 1
    }

    #[test]
    fn a_click_on_a_search_result_selects_that_row() {
        let view = search_view();
        // First result row sits at `results_top`; row 2 is two lines down.
        assert_eq!(
            mouse_to_action(Screen::Search, view, 30, results_top() + 2, false, false),
            Action::ClickRow(2)
        );
        // The first row maps to index 0, and any column past the sidebar
        // counts — clicks do not need to hit the name text.
        assert_eq!(
            mouse_to_action(Screen::Search, view, 99, results_top(), false, false),
            Action::ClickRow(0)
        );
    }

    #[test]
    fn clicking_the_search_bar_focuses_the_input() {
        let view = search_view();
        let main_col = 1 + SIDEBAR_WIDTH;
        // The panel's left border is not clickable (sidebar rows now toggle
        // sources — covered by `a_click_on_a_sidebar_source_toggles_it`).
        assert_eq!(
            mouse_to_action(Screen::Search, view, 0, results_top(), false, false),
            Action::None
        );
        // The panel top border and the search bar: "I want to type" — every
        // row above the results header focuses the input pane.
        for row in 0..results_top() {
            assert_eq!(
                mouse_to_action(Screen::Search, view, main_col + 10, row, false, false),
                Action::FocusSearchInput,
                "row {row} is the bar — clicking it focuses the input"
            );
        }
        // The results header row selects row 0, like any result row.
        assert_eq!(
            mouse_to_action(
                Screen::Search,
                view,
                main_col + 10,
                results_top(),
                false,
                false
            ),
            Action::ClickRow(0)
        );
        // Hint line (the last inner row) and the panel's bottom border.
        assert_eq!(
            mouse_to_action(
                Screen::Search,
                view,
                main_col + 10,
                view.height - 2,
                false,
                false
            ),
            Action::None
        );
        assert_eq!(
            mouse_to_action(
                Screen::Search,
                view,
                main_col + 10,
                view.height - 1,
                false,
                false
            ),
            Action::None
        );
        // Completely outside the view area.
        assert_eq!(
            mouse_to_action(Screen::Search, view, 200, 200, false, false),
            Action::None
        );
    }

    #[test]
    fn a_click_on_a_sidebar_source_toggles_it() {
        let view = search_view();
        // Sidebar rows (offset 0 = "Sources" title, matching the painter):
        // title, Games divider, FitGirl, Movies divider, YTS, TPB, 1337x,
        // BitTorrented, TV divider, EZTV, TPB-TV, 1337x-TV, Anime divider,
        // Nyaa, SubsPlease.
        let cases = [
            (1, None),
            (2, None),
            (3, Some(SourceId::FitGirl)),
            (4, None),
            (5, Some(SourceId::Yts)),
            (6, Some(SourceId::TpbMovies)),
            (7, Some(SourceId::X1337Movies)),
            (8, Some(SourceId::Bittorrented)),
            (9, None),
            (10, Some(SourceId::Eztv)),
            (11, Some(SourceId::TpbTv)),
            (12, Some(SourceId::X1337Tv)),
            (13, None),
            (14, Some(SourceId::Nyaa)),
            (15, Some(SourceId::SubsPlease)),
            (16, None),
            (29, None),
        ];
        for (row, expected) in cases {
            let action = mouse_to_action(Screen::Search, view, 1, row, false, false);
            assert_eq!(
                action,
                expected.map_or(Action::None, Action::ToggleSource),
                "sidebar row {row}"
            );
        }
        // Any column inside the sidebar toggles, not just the label column.
        assert_eq!(
            mouse_to_action(Screen::Search, view, SIDEBAR_WIDTH, 5, false, false),
            Action::ToggleSource(SourceId::Yts)
        );
        // The panel's top border is not a sidebar row.
        assert_eq!(
            mouse_to_action(Screen::Search, view, 1, 0, false, false),
            Action::None
        );
    }

    /// A downloads-view area with no status bar: 20 rows of 60 columns.
    fn downloads_view() -> Rect {
        Rect::new(0, 0, 60, 20)
    }

    /// The terminal row where the first downloads item renders: inner top
    /// border (1) + the 2-row tab band.
    fn downloads_items_top() -> u16 {
        1 + 2
    }

    #[test]
    fn a_click_on_the_downloads_tab_band_toggles_seeding() {
        let view = downloads_view();
        // The whole 2-row tab band responds, in any column inside the panel.
        for row in [1, 2] {
            assert_eq!(
                mouse_to_action(Screen::Downloads, view, 30, row, false, false),
                Action::ClickSeedingTab,
                "tab band row {row}"
            );
        }
        // The panel's inner corners still count as the tab band.
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 1, 2, false, false),
            Action::ClickSeedingTab
        );
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 58, 2, false, false),
            Action::ClickSeedingTab
        );
        // The panel border and everything above it are not clickable.
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 0, 2, false, false),
            Action::None
        );
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 30, 0, false, false),
            Action::None
        );
    }

    #[test]
    fn a_click_on_a_downloads_item_row_selects_that_item() {
        let view = downloads_view();
        // Each item spans two rows (name line + progress bar) and the
        // first starts two rows below the inner top — below the tab band.
        assert_eq!(
            mouse_to_action(
                Screen::Downloads,
                view,
                30,
                downloads_items_top(),
                false,
                false
            ),
            Action::ClickRow(0)
        );
        assert_eq!(
            mouse_to_action(
                Screen::Downloads,
                view,
                30,
                downloads_items_top() + 1,
                false,
                false
            ),
            Action::ClickRow(0)
        );
        assert_eq!(
            mouse_to_action(
                Screen::Downloads,
                view,
                30,
                downloads_items_top() + 2,
                false,
                false
            ),
            Action::ClickRow(1)
        );
        // Deep in the list: row 17 maps to the 8th visible item.
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 30, 17, false, false),
            Action::ClickRow(7)
        );
    }

    #[test]
    fn clicks_below_the_downloads_list_or_outside_the_panel_do_nothing() {
        let view = downloads_view();
        // The hint line owns the last inner row; the status bar and
        // off-screen rows below it have no click target.
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 30, 18, false, false),
            Action::None
        );
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 30, 19, false, false),
            Action::None
        );
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 30, 25, false, false),
            Action::None
        );
        // Outside the panel horizontally.
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 0, 10, false, false),
            Action::None
        );
        assert_eq!(
            mouse_to_action(Screen::Downloads, view, 61, 10, false, false),
            Action::None
        );
    }

    #[test]
    fn a_click_anywhere_on_the_splash_dismisses_it() {
        let view = search_view();
        // The splash is an intro, not a state: any left click moves past
        // it, exactly like any key. Position carries no meaning — there is
        // nothing on it to select.
        for (col, row) in [(0, 0), (99, 0), (0, 29), (99, 29), (200, 200)] {
            assert_eq!(
                mouse_to_action(Screen::Splash, view, col, row, false, false),
                Action::Dismiss,
                "click at ({col}, {row})"
            );
        }
    }

    #[test]
    fn a_click_anywhere_on_now_playing_ends_the_watch_session() {
        let view = search_view();
        // Mirror of the q/esc binding: a click ends the session and
        // returns to the TUI rather than quitting.
        for (col, row) in [(0, 0), (99, 0), (0, 29), (99, 29), (200, 200)] {
            assert_eq!(
                mouse_to_action(Screen::NowPlaying, view, col, row, false, false),
                Action::EndWatch,
                "click at ({col}, {row})"
            );
        }
    }

    #[test]
    fn a_click_while_help_is_open_closes_it_from_any_position() {
        let view = search_view();
        // The overlay is dismissible and position carries no meaning: a
        // click anywhere — on the modal or on the screen behind it — closes
        // help and returns the user exactly where they were. This wins over
        // row selection, mirroring `map`: with help open, a click never
        // selects the row underneath the overlay.
        for screen in [
            Screen::Search,
            Screen::Downloads,
            Screen::Splash,
            Screen::NowPlaying,
        ] {
            for (col, row) in [(0, 0), (30, results_top()), (99, 29)] {
                assert_eq!(
                    mouse_to_action(screen, view, col, row, true, false),
                    Action::ToggleHelp,
                    "{screen:?} + click at ({col}, {row})"
                );
            }
        }
    }

    #[test]
    fn the_help_screen_state_closes_on_a_click() {
        // Mirror of the keymap's `Screen::Help` arm; the real overlay is
        // handled by `help_open` above.
        let view = search_view();
        assert_eq!(
            mouse_to_action(Screen::Help, view, 30, results_top(), false, false),
            Action::ToggleHelp
        );
    }
}
