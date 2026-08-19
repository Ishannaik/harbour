//! Now-playing view (FR-57..FR-59, FR-105, phase 6): the watch screen shown
//! while an external player (mpv/VLC) plays the item's stream.
//!
//! Pure paint: `draw` renders the item + stream URL + what harbour actually
//! knows about the playback; the app loop owns the player lifecycle (launch
//! on `w`, return on player exit or `q`/esc). harbour ships no render engine
//! — the external player is the renderer (design.md §2.4), and it is launched
//! with a bare URL, so it never reports position back. The view is therefore
//! deliberately honest about progress (FR-59/FR-105): it states that the
//! stream is live, that seeking is supported (every harbour stream is
//! Range-served), measured download progress/speed from the live `ItemView`,
//! and that player position is the player's — it never invents an
//! elapsed/total or a buffer percentage no one measured.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::types::ItemView;
use crate::theme::Theme;
use crate::ui::NowPlaying;

/// Title — same framing as the other views.
const TITLE: &str = " harbour — now playing ";
/// Bottom hint.
const HINT: &str = "q / esc back to the TUI";

/// Footer for sidecar subs. Never claims a language we cannot read from MKV.
fn subs_footer(state: &NowPlaying) -> String {
    match state.subtitle.as_deref() {
        Some(name) => format!("subs: {name}"),
        None => "subs: (none in torrent)".into(),
    }
}

/// Renders the now-playing screen: item name, the loopback stream URL the
/// player opened, and the playback state harbour knows (FR-59/FR-105) — the
/// stream is live, seeking works, download progress/speed are measured,
/// position lives in the player. Player transport (seek/volume) lives in the
/// player; this view is a status screen.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &NowPlaying,
    stats: Option<&ItemView>,
    theme: &Theme,
) {
    let colors = &theme.colors;
    let bg = colors.bg().to_ratatui();
    let accent = colors.accent().to_ratatui();

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
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(border)
        .border_style(Style::default().fg(colors.border().to_ratatui()))
        .title(Span::styled(TITLE, Style::default().fg(accent)))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let success = colors.success().to_ratatui();
    let text = colors.text().to_ratatui();
    let muted = colors.muted().to_ratatui();
    let mut lines = vec![
        Line::default(),
        // The one live fact: the stream answered the pre-launch probe and is
        // playing in the player.
        Line::from(vec![
            Span::styled(
                theme.symbols.dot_online.as_ref().to_string(),
                Style::default().fg(success),
            ),
            Span::styled(
                " streaming to your player".to_string(),
                Style::default().fg(success),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(state.name.clone(), Style::default().fg(text))),
        Line::from(Span::styled(
            state.stream_url.clone(),
            Style::default().fg(muted),
        )),
    ];
    if let Some(view) = stats {
        // FR-105: measured bytes-so-far and speed, never a fake buffer bar.
        lines.push(Line::from(Span::styled(
            format!(
                "{:.0}% · {:.1} MiB/s",
                view.progress() * 100.0,
                view.speed_mib()
            ),
            Style::default().fg(muted),
        )));
    }
    lines.extend([
        Line::default(),
        // Honest playback state (FR-59): harbour streams are Range-served, so
        // seeking works — but the external player owns position and does not
        // report it, so no elapsed/total bar is drawn.
        Line::from(Span::styled(
            "seeking supported — position is your player's".to_string(),
            Style::default().fg(muted),
        )),
        Line::default(),
        // Sidecar only: we never parse MKV tracks, so "none" means none in the
        // torrent's file list, not "Japanese missing".
        Line::from(Span::styled(subs_footer(state), Style::default().fg(muted))),
        Line::from(Span::styled(HINT.to_string(), Style::default().fg(muted))),
        Line::default(),
    ]);
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::core::types::{EngineStats, QueueItem};

    fn state() -> NowPlaying {
        NowPlaying {
            id: "abc".into(),
            name: "Frieren - 01 [1080p]".into(),
            stream_url: "http://127.0.0.1:4567/stream".into(),
            ephemeral: false,
            subtitle: None,
        }
    }

    fn render_text(state: &NowPlaying, stats: Option<&ItemView>, theme: &Theme) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|f| draw(f, f.area(), state, stats, theme))
            .expect("draw must succeed");
        let buf = terminal.backend().buffer();
        (0..24)
            .flat_map(|y| (0..80).map(move |x| buf[(x, y)].symbol().to_string()))
            .collect()
    }

    #[test]
    fn now_playing_renders_stream_and_honest_playback_state() {
        let theme = Theme::titanium();
        let text = render_text(&state(), None, &theme);
        assert!(text.contains("now playing"), "panel title shown");
        assert!(text.contains("Frieren"), "item name shown");
        assert!(text.contains("127.0.0.1:4567"), "stream URL shown");
        // FR-59: the view states the live stream and the seeking support it
        // can actually vouch for — and that position is the player's, never
        // an invented progress bar.
        assert!(text.contains("streaming"), "live stream stated");
        assert!(text.contains("seeking supported"), "Range seek stated");
        assert!(text.contains("position"), "player owns position");
        assert!(
            !text.contains("buffer"),
            "must never fabricate a buffer percentage"
        );
        assert!(
            text.contains("subs: (none in torrent)"),
            "no sidecar is said plainly, never a language we did not read"
        );
    }

    #[test]
    fn now_playing_shows_measured_progress_and_speed() {
        let mut item = QueueItem::new(
            "abc".into(),
            "Frieren".into(),
            None,
            None,
            PathBuf::from("dl"),
            0,
        );
        item.total_bytes = 1000;
        let view = ItemView::new(
            item,
            Some(EngineStats {
                progress: 0.4,
                downloaded_bytes: 400,
                total_bytes: 1000,
                speed_mib: 2.1,
                ..EngineStats::default()
            }),
        );
        let text = render_text(&state(), Some(&view), &Theme::titanium());
        assert!(text.contains("40%"), "measured progress, not a fake buffer");
        assert!(text.contains("2.1 MiB/s"), "measured speed from ItemView");
    }

    #[test]
    fn now_playing_footer_names_the_sidecar_file() {
        let theme = Theme::titanium();
        let mut state = state();
        state.subtitle = Some("Frieren.en.srt".into());
        let text = render_text(&state, None, &theme);
        assert!(
            text.contains("subs: Frieren.en.srt"),
            "sidecar filename shown, got: {text}"
        );
        assert!(
            !text.contains("(none in torrent)"),
            "a present sidecar must not show the none-in-torrent fallback"
        );
    }

    #[test]
    fn now_playing_stream_dot_uses_the_online_glyph() {
        let theme = Theme::titanium();
        let text = render_text(&state(), None, &theme);
        assert!(
            text.contains(theme.symbols.dot_online.as_ref()),
            "streaming status marked with the theme's online dot"
        );
    }
}
