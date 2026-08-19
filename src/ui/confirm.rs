//! Forget / delete / clear-cache confirm overlay (FR-76, FR-80).
//!
//! `x`, `shift+X`, and `shift+C` all open this panel before anything is
//! destroyed. The highlighted choice defaults to **No** so Enter without
//! thinking is a no-op; `y` always proceeds. The pending work is an enum, not
//! a closure, so the overlay is snapshot-testable and the app loop can match it.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// What Enter/`y` will do if the user confirms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Forget the item; files stay on disk.
    Forget { id: String },
    /// Forget the item and delete its payload files (`shift+X`).
    ForgetAndDelete { id: String },
    /// Wipe search cache and unused torrent/watch cache (FR-80).
    ClearCache,
}

/// Confirm-overlay state, owned by the app loop and drawn by [`draw`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfirmPrompt {
    /// The overlay is up and owns the keyboard.
    pub open: bool,
    pub title: String,
    pub body: String,
    /// True for `shift+X` / clear-cache — Yes is painted in the error colour.
    pub destructive: bool,
    pub on_confirm: Option<ConfirmAction>,
    /// `false` = No (the default), `true` = Yes.
    pub yes_selected: bool,
}

/// Footer — single source of truth for the overlay hint.
pub const HINT: &str = "← → choose · y yes · n/esc no";

impl ConfirmPrompt {
    /// Forget confirm: names the item, never the directory (FR-76).
    pub fn forget(name: &str, id: String) -> Self {
        Self {
            open: true,
            title: " forget download ".into(),
            body: format!("Remove {name} from the list? Files stay on disk."),
            destructive: false,
            on_confirm: Some(ConfirmAction::Forget { id }),
            yes_selected: false,
        }
    }

    /// Delete-files confirm: names the item and the exact directory (FR-76).
    pub fn delete_files(name: &str, dir: &Path, id: String) -> Self {
        Self {
            open: true,
            title: " delete from device ".into(),
            body: format!("Delete {name} and its files in {}?", dir.display()),
            destructive: true,
            on_confirm: Some(ConfirmAction::ForgetAndDelete { id }),
            yes_selected: false,
        }
    }

    /// Clear-cache confirm. Defaults to No — wiping cache is loud, not a
    /// one-keystroke accident (FR-80).
    pub fn clear_cache() -> Self {
        Self {
            open: true,
            title: " clear cache ".into(),
            body: "Deletes search cache and unused .torrent files. \
                   Your downloads folder and queue stay."
                .into(),
            destructive: true,
            on_confirm: Some(ConfirmAction::ClearCache),
            yes_selected: false,
        }
    }
}

/// Draws the overlay: a dim backdrop, then a centred Yes/No panel.
pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme, prompt: &ConfirmPrompt) {
    let colors = &theme.colors;

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(colors.dim().to_ratatui())),
        area,
    );

    let width = HINT
        .chars()
        .count()
        .max(prompt.body.chars().count() + 4)
        .max(prompt.title.chars().count() + 8)
        .clamp(36, area.width.saturating_sub(4).max(36) as usize) as u16;
    let height = 8.min(area.height);
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height,
    };

    let border = BorderSet {
        top_left: theme.symbols.border_tl.as_ref(),
        top_right: theme.symbols.border_tr.as_ref(),
        bottom_left: theme.symbols.border_bl.as_ref(),
        bottom_right: theme.symbols.border_br.as_ref(),
        vertical_left: theme.symbols.border_v.as_ref(),
        vertical_right: theme.symbols.border_v.as_ref(),
        horizontal_top: theme.symbols.border_h.as_ref(),
        horizontal_bottom: theme.symbols.border_h.as_ref(),
    };

    let no_style = choice_style(theme, !prompt.yes_selected, false);
    let yes_style = choice_style(theme, prompt.yes_selected, prompt.destructive);
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", prompt.body),
            Style::default().fg(colors.text().to_ratatui()),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("    "),
            Span::styled(" No ", no_style),
            Span::raw("   "),
            Span::styled(" Yes ", yes_style),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {HINT}"),
            Style::default().fg(colors.muted().to_ratatui()),
        )),
    ];

    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_set(border)
                .border_style(Style::default().fg(colors.border().to_ratatui()))
                .title(Span::styled(
                    prompt.title.clone(),
                    Style::default().fg(if prompt.destructive {
                        colors.error().to_ratatui()
                    } else {
                        colors.accent().to_ratatui()
                    }),
                ))
                .style(Style::default().bg(colors.bg().to_ratatui())),
        ),
        panel,
    );
}

fn choice_style(theme: &Theme, selected: bool, destructive: bool) -> Style {
    let colors = &theme.colors;
    let fg = if destructive {
        colors.error().to_ratatui()
    } else {
        colors.accent().to_ratatui()
    };
    if selected {
        Style::default()
            .fg(fg)
            .bg(colors.selected_bg().to_ratatui())
    } else {
        Style::default().fg(colors.text().to_ratatui())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::theme::Theme;

    fn render(prompt: &ConfirmPrompt) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let theme = Theme::titanium();
        terminal
            .draw(|frame| draw(frame, frame.area(), &theme, prompt))
            .expect("draw");
        let buf = terminal.backend().buffer();
        (0..24)
            .map(|y| {
                let mut line: String = (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect();
                while line.ends_with(' ') {
                    line.pop();
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn forget_and_delete_prompts_default_to_no() {
        let forget = ConfirmPrompt::forget("Dune", "abc".into());
        assert!(forget.open);
        assert!(!forget.yes_selected, "FR-76: default highlight is No");
        assert!(!forget.destructive);
        assert!(forget.body.contains("Dune"));
        assert!(
            !forget.body.contains('/'),
            "forget does not name a directory"
        );

        let del = ConfirmPrompt::delete_files("Dune", Path::new("/media/dune"), "abc".into());
        assert!(del.open);
        assert!(!del.yes_selected, "FR-76: default highlight is No");
        assert!(del.destructive);
        assert!(del.body.contains("Dune"));
        assert!(
            del.body.contains("/media/dune"),
            "shift+X names the exact directory"
        );
    }

    #[test]
    fn clear_cache_confirm_defaults_to_no() {
        let prompt = ConfirmPrompt::clear_cache();
        assert!(prompt.open);
        assert!(!prompt.yes_selected, "FR-80: default highlight is No");
        assert!(prompt.destructive);
        assert_eq!(prompt.on_confirm, Some(ConfirmAction::ClearCache));
    }

    #[test]
    fn overlay_renders_the_item_choices_and_hint() {
        let prompt = ConfirmPrompt::delete_files(
            "Dune.mkv",
            &PathBuf::from("/tmp/harbour/dune"),
            "abc".into(),
        );
        let out = render(&prompt);
        assert!(out.contains("delete from device"), "title:\n{out}");
        assert!(out.contains("Dune.mkv"), "item name:\n{out}");
        assert!(out.contains("/tmp/harbour/dune"), "directory:\n{out}");
        assert!(out.contains("No"), "No choice:\n{out}");
        assert!(out.contains("Yes"), "Yes choice:\n{out}");
        assert!(out.contains("y yes"), "hint:\n{out}");
    }
}
