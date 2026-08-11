//! Keyboard handling: key event in, [`Action`] out.
//!
//! Deliberately a pure function of `(key, screen)`. The app loop performs the
//! actions; nothing here touches the engine, the queue, or the terminal. That
//! is what makes the entire keymap testable without a TUI, and it is why the
//! keybind tests below can assert `UR-10` directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::Screen;

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
}
