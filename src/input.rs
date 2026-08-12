//! Input handling: key and mouse events in, [`Action`] out.
//!
//! Deliberately pure functions of `(screen, …)`. The app loop performs the
//! actions; nothing here touches the engine, the queue, or the terminal. That
//! is what makes the keymap and the click mapping testable without a TUI, and
//! it is why the keybind tests below can assert `UR-10` directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Margin, Rect};

use crate::ui::Screen;
use crate::ui::search::{SEARCH_BAR_H, SIDEBAR_WIDTH};

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
    /// Run the current query.
    Submit,
    /// Append to the query.
    Type(char),
    Backspace,
    /// Clear the query, or close an overlay.
    Escape,
    /// Download the highlighted result.
    Download,
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
    /// Select the visible row under a left click — a search result or a
    /// downloads item (UR-13). The index is clamped by the app loop, which
    /// knows the real list length; a click past the last row is a no-op.
    ClickRow(usize),
    /// Toggle the downloads seeding tab from a click (same effect as
    /// `ToggleSeeding`).
    ClickSeedingTab,
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
/// were rather than to a default screen.
pub fn map(key: KeyEvent, screen: Screen, help_open: bool) -> Action {
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

    match screen {
        // The splash is an intro, not a state: anything moves past it.
        Screen::Splash => match key.code {
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Dismiss,
        },

        Screen::Search => match key.code {
            KeyCode::Enter => Action::Submit,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Down => Action::MoveDown,
            KeyCode::Tab => Action::SwitchScreen,
            KeyCode::Esc => Action::Escape,
            KeyCode::Backspace => Action::Backspace,
            // `?` opens help only when the query is empty, so it stays typable
            // in a search term. The same reasoning applies to `d` and `q`: on
            // the search screen the text field wins, because a user typing
            // "dune" must not trigger a download on the `d`.
            KeyCode::Char(c) => Action::Type(c),
            _ => Action::None,
        },

        Screen::Downloads => match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Tab | KeyCode::Esc => Action::SwitchScreen,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Down => Action::MoveDown,
            KeyCode::Char('s') => Action::ToggleSeeding,
            KeyCode::Char('p') => Action::TogglePause,
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

        // Reachable only as a state; the overlay branch above handles its keys.
        Screen::Help => Action::ToggleHelp,
    }
}

/// Actions available from the search screen when the query box is empty.
///
/// The search screen gives every printable key to the text field, which would
/// otherwise make `d`, `?` and `q` unreachable there. With an empty query there
/// is nothing to type into, so the bindings take over.
pub fn map_empty_query(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('d') => Some(Action::Download),
        _ => None,
    }
}

/// Downloading from the search screen while a query is present.
///
/// `shift+D` is the escape hatch: it cannot be confused with typing, because a
/// capital D in a search term is rare and the user can still type one with the
/// query box focused via any other capital.
pub fn is_download_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('D'))
}

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

    match screen {
        Screen::Search => {
            // Mirrors `ui::search::draw`'s layout: a 1-cell panel border,
            // then a 22-wide sidebar; the main column starts after both.
            // Inside it, the search bar is SEARCH_BAR_H rows, the results
            // header 1 row, then one result row per line. The hint line
            // owns the last inner row, and the panel's bottom border sits
            // one row below that.
            let main_col = view_area.x + 1 + SIDEBAR_WIDTH;
            let results_top = view_area.y + 1 + SEARCH_BAR_H + 1;
            let results_bottom = view_area.y + view_area.height.saturating_sub(2);
            if col < main_col || row < results_top || row >= results_bottom {
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
            assert_eq!(map(ctrl('c'), screen, false), Action::Quit, "{screen:?}");
            assert_eq!(
                map(ctrl('c'), screen, true),
                Action::Quit,
                "{screen:?} + help"
            );
        }
    }

    #[test]
    fn typing_on_the_search_screen_never_triggers_an_action() {
        // The bug this prevents: typing "dune" firing a download on the `d`.
        for c in "dunepqrsx?".chars() {
            assert_eq!(
                map(key(KeyCode::Char(c)), Screen::Search, false),
                Action::Type(c),
                "`{c}` must reach the text field"
            );
        }
    }

    #[test]
    fn an_empty_query_frees_the_letter_bindings() {
        // With nothing to type into, the keys become useful again.
        assert_eq!(map_empty_query(key(KeyCode::Char('q'))), Some(Action::Quit));
        assert_eq!(
            map_empty_query(key(KeyCode::Char('?'))),
            Some(Action::ToggleHelp)
        );
        assert_eq!(
            map_empty_query(key(KeyCode::Char('d'))),
            Some(Action::Download)
        );
        assert_eq!(map_empty_query(key(KeyCode::Char('z'))), None);
    }

    #[test]
    fn w_watches_and_q_ends_watch() {
        assert_eq!(
            map(key(KeyCode::Char('w')), Screen::Downloads, false),
            Action::Watch
        );
        assert_eq!(
            map(key(KeyCode::Char('q')), Screen::NowPlaying, false),
            Action::EndWatch
        );
        assert_eq!(
            map(key(KeyCode::Esc), Screen::NowPlaying, false),
            Action::EndWatch
        );
        // Ctrl+C still quits from the watch screen.
        assert_eq!(map(ctrl('c'), Screen::NowPlaying, false), Action::Quit);
        // q must not quit from now-playing — it ends the session.
        assert_ne!(
            map(key(KeyCode::Char('q')), Screen::NowPlaying, false),
            Action::Quit
        );
    }

    #[test]
    fn shift_d_downloads_even_mid_query() {
        assert!(is_download_key(key(KeyCode::Char('D'))));
        assert!(!is_download_key(key(KeyCode::Char('d'))));
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
                map(key(code), Screen::Downloads, true),
                Action::ToggleHelp,
                "{code:?} should dismiss the overlay"
            );
        }
    }

    #[test]
    fn the_splash_is_dismissed_by_anything() {
        assert_eq!(
            map(key(KeyCode::Enter), Screen::Splash, false),
            Action::Dismiss
        );
        assert_eq!(
            map(key(KeyCode::Char('x')), Screen::Splash, false),
            Action::Dismiss
        );
        // Except `q`, which should still quit rather than making someone wait.
        assert_eq!(
            map(key(KeyCode::Char('q')), Screen::Splash, false),
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
                map(key(code), Screen::Downloads, false),
                expected,
                "{code:?}"
            );
        }
    }

    #[test]
    fn unbound_keys_do_nothing_rather_than_something_surprising() {
        assert_eq!(
            map(key(KeyCode::F(5)), Screen::Downloads, false),
            Action::None
        );
        assert_eq!(
            map(key(KeyCode::Insert), Screen::Search, false),
            Action::None
        );
    }

    #[test]
    fn every_action_the_keymap_can_produce_is_documented_in_the_help() {
        // UR-10: the overlay must show exactly what the app implements.
        let documented: Vec<&str> = crate::ui::help::BINDINGS.iter().map(|(k, _)| *k).collect();
        for key in [
            "enter", "↑ ↓", "d", "tab", "s", "p", "r", "x", "esc", "?", "q",
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
    fn a_click_outside_the_search_results_does_nothing() {
        let view = search_view();
        let main_col = 1 + SIDEBAR_WIDTH;
        // Sidebar and the panel's left border.
        assert_eq!(
            mouse_to_action(Screen::Search, view, 0, results_top(), false, false),
            Action::None
        );
        assert_eq!(
            mouse_to_action(
                Screen::Search,
                view,
                SIDEBAR_WIDTH,
                results_top(),
                false,
                false
            ),
            Action::None
        );
        // Search bar rows and the results header.
        for row in 0..results_top() {
            assert_eq!(
                mouse_to_action(Screen::Search, view, main_col + 10, row, false, false),
                Action::None,
                "row {row} is above the results"
            );
        }
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
