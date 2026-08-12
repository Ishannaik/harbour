//! The player-picker overlay (`shift+P`): choose which external player watch
//! mode uses, or enter a custom path. The choice persists to `config.player`
//! (the config stays the fallback); the overlay itself mirrors `help.rs` — a
//! dimmed backdrop with a centred panel over whatever screen is underneath,
//! so opening it never loses the user's place.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// Picker interaction mode: browsing installed players, or typing a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerMode {
    #[default]
    List,
    Custom,
}

/// Player-picker state (2.1), owned by the app loop and drawn by [`draw`].
#[derive(Debug, Clone, Default)]
pub struct PlayerPicker {
    /// Whether the overlay is up. The app loop checks this before routing
    /// keys (`input::map`'s `picker_open` param).
    pub open: bool,
    pub mode: PickerMode,
    /// Index into `options` — the highlighted row in list mode.
    pub selected: usize,
    /// Installed players as (display label, command path), from
    /// `watch::find_players`, filled when the overlay opens.
    pub options: Vec<(String, String)>,
    /// Custom player path being typed in custom mode.
    pub custom: String,
    /// A validation error to show in the overlay (loud, never silent).
    pub message: Option<String>,
}

/// The hint line — single source of truth for the overlay's footer.
pub const HINT: &str = "↑/↓ select · enter use · c custom path · esc cancel";

/// Draws the overlay: a dim backdrop, then a centred panel listing every
/// installed player with `●` on the current `config.player` choice, the
/// custom-path input line, the hint, and any validation error.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    picker: &PlayerPicker,
    config_player: Option<&str>,
) {
    let colors = &theme.colors;

    // Dim the whole frame first; the panel repaints its own rectangle over
    // it, so the screen behind stays visible but clearly de-emphasized.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(colors.dim().to_ratatui())),
        area,
    );

    let hint_width = HINT.chars().count() + 4;
    let option_width = picker
        .options
        .iter()
        .map(|(label, _)| label.chars().count() + 8)
        .max()
        .unwrap_or(0);
    let custom_width = "path: ".len() + picker.custom.chars().count().min(36) + 4;
    let width = hint_width
        .max(option_width)
        .max(custom_width)
        .clamp(30, area.width.saturating_sub(4).max(30) as usize) as u16;

    let option_rows = picker.options.len() as u16;
    let error_rows = u16::from(picker.message.is_some());
    let height = (5 + option_rows + error_rows).min(area.height);
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

    let mut lines = vec![Line::from("")];
    for (i, (label, command)) in picker.options.iter().enumerate() {
        let row_style = if i == picker.selected {
            Style::default()
                .fg(colors.accent().to_ratatui())
                .bg(colors.selected_bg().to_ratatui())
        } else {
            Style::default().fg(colors.text().to_ratatui())
        };
        // `▸` follows the selection; `●` marks the persisted config choice.
        let cursor = if i == picker.selected { "▸" } else { " " };
        let choice = if config_player == Some(command.as_str()) {
            "●"
        } else {
            "·"
        };
        lines.push(Line::from(vec![
            Span::raw(format!(" {cursor} {choice} ")),
            Span::styled(label.clone(), row_style),
        ]));
    }

    // The custom-path line is the active input in custom mode, a hint
    // otherwise; the text is capped so a long path cannot widen the panel
    // past the terminal.
    let custom_display: String = picker.custom.chars().take(36).collect::<String>();
    let path_style = if picker.mode == PickerMode::Custom {
        colors.accent().to_ratatui()
    } else {
        colors.dim().to_ratatui()
    };
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(
            format!("path: {custom_display}"),
            Style::default().fg(path_style),
        ),
    ]));

    lines.push(Line::from(vec![Span::styled(
        format!("  {HINT}"),
        Style::default().fg(colors.muted().to_ratatui()),
    )]));

    if let Some(message) = &picker.message {
        lines.push(Line::from(vec![Span::styled(
            format!("  {message}"),
            Style::default().fg(colors.error().to_ratatui()),
        )]));
    }

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_set(border)
                .border_style(Style::default().fg(colors.border().to_ratatui()))
                .title(Span::styled(
                    " player ",
                    Style::default().fg(colors.accent().to_ratatui()),
                ))
                .style(Style::default().bg(colors.bg().to_ratatui())),
        ),
        panel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picker with two players, one matching the persisted config choice.
    fn picker() -> PlayerPicker {
        PlayerPicker {
            open: true,
            mode: PickerMode::List,
            selected: 0,
            options: vec![
                ("mpv".to_string(), "mpv".to_string()),
                ("VLC".to_string(), "vlc".to_string()),
            ],
            custom: String::new(),
            message: None,
        }
    }

    #[test]
    fn draw_marks_the_config_choice_and_the_selection() {
        let theme = Theme::titanium();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), &theme, &picker(), Some("mpv")))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let text: String = (0..24)
            .flat_map(|y| (0..80).map(move |x| buf[(x, y)].symbol()))
            .collect();
        // The config choice (mpv) carries ●, the non-choice (VLC) a dot.
        assert!(text.contains('●'), "config choice marked");
        assert!(text.contains('·'), "non-choice marked");
        assert!(text.contains("mpv"));
        assert!(text.contains("VLC"));
        assert!(text.contains("custom path"));
    }

    #[test]
    fn draw_shows_the_error_message_and_typed_path() {
        let theme = Theme::titanium();
        let mut p = picker();
        p.mode = PickerMode::Custom;
        p.custom = "C:\\tools\\mpv.exe".into();
        p.message = Some("'C:\\tools\\mpv.exe' is not an existing absolute path".into());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), &theme, &p, Some("mpv")))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        let text: String = (0..24)
            .flat_map(|y| (0..80).map(move |x| buf[(x, y)].symbol()))
            .collect();
        assert!(text.contains("not an existing absolute path"));
        assert!(text.contains("C:\\tools\\mpv.exe"));
    }

    #[test]
    fn default_picker_is_closed_in_list_mode() {
        let p = PlayerPicker::default();
        assert!(!p.open);
        assert_eq!(p.mode, PickerMode::List);
        assert!(p.options.is_empty());
        assert!(p.custom.is_empty());
        assert!(p.message.is_none());
    }
}
