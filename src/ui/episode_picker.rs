//! Episode / Multi-File picker overlay: when a torrent contains multiple
//! video files (such as season packs or batch anime releases), this modal
//! lets the user interactively browse and choose an episode to stream.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::core::types::TorrentFileView;
use crate::theme::Theme;

/// Episode-picker state, owned by the app loop and drawn by [`draw`].
#[derive(Debug, Clone, Default)]
pub struct EpisodePicker {
    pub open: bool,
    pub torrent_id: String,
    pub torrent_name: String,
    pub player: String,
    pub ephemeral: bool,
    pub episodes: Vec<TorrentFileView>,
    pub selected: usize,
}

impl EpisodePicker {
    pub fn select_next(&mut self) {
        if !self.episodes.is_empty() {
            self.selected = (self.selected + 1).min(self.episodes.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn page_down(&mut self) {
        if !self.episodes.is_empty() {
            self.selected = (self.selected + 6).min(self.episodes.len() - 1);
        }
    }

    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(6);
    }
}

/// The hint line at the footer of the episode picker modal.
pub const HINT: &str = "↑/↓ select · enter watch episode · esc cancel";

/// Draws the episode picker overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    picker: &EpisodePicker,
    mouse_pos: Option<(u16, u16)>,
) {
    if !picker.open || picker.episodes.is_empty() {
        return;
    }
    let colors = &theme.colors;

    // Dim the whole frame backdrop
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(colors.dim().to_ratatui())),
        area,
    );

    let max_w = (area.width as usize).saturating_sub(6).max(40);
    let max_name_len = picker
        .episodes
        .iter()
        .map(|ep| ep.name.chars().count() + 18)
        .max()
        .unwrap_or(40);
    let width = max_name_len.clamp(50, max_w) as u16;

    let max_h = (area.height as usize).saturating_sub(4).max(8);
    let needed_h = (picker.episodes.len() + 4).min(max_h);
    let height = needed_h as u16;

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

    let content_h = height.saturating_sub(3) as usize;
    let list_len = picker.episodes.len();
    let scroll_start = if picker.selected < content_h / 2 {
        0
    } else if picker.selected + content_h / 2 >= list_len {
        list_len.saturating_sub(content_h)
    } else {
        picker.selected - content_h / 2
    };

    let list_area = Rect {
        x: panel.x + 2,
        y: panel.y + 2,
        width: panel.width.saturating_sub(4),
        height: content_h as u16,
    };

    let mut lines = Vec::new();
    for (rel_idx, (idx, ep)) in picker
        .episodes
        .iter()
        .enumerate()
        .skip(scroll_start)
        .take(content_h)
        .enumerate()
    {
        let is_sel = idx == picker.selected;
        let row_y = list_area.y + rel_idx as u16;
        let hovered = mouse_pos
            .is_some_and(|(mx, my)| mx >= list_area.x && mx < list_area.right() && my == row_y);

        let prefix = if is_sel {
            " ► "
        } else if hovered {
            " · "
        } else {
            "   "
        };
        let size_str = format_bytes(ep.size_bytes);
        let avail_w = (list_area.width as usize).saturating_sub(prefix.len() + size_str.len() + 3);
        let name_str = truncate_str(&ep.name, avail_w);
        let pad = avail_w.saturating_sub(name_str.chars().count());

        let (fg, bg) = if is_sel {
            (colors.accent().to_ratatui(), colors.dim().to_ratatui())
        } else if hovered {
            (colors.text().to_ratatui(), colors.dim().to_ratatui())
        } else {
            (colors.text().to_ratatui(), colors.bg().to_ratatui())
        };

        let style = if is_sel {
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(fg).bg(bg)
        };

        lines.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(name_str, style),
            Span::styled(" ".repeat(pad), style),
            Span::styled(
                format!(" {size_str} "),
                Style::default().fg(colors.muted().to_ratatui()).bg(bg),
            ),
        ]));
    }

    // Outer panel frame with title and footer
    frame.render_widget(Clear, panel);
    let title = format!(
        " Select Episode to Watch ({} files) ",
        picker.episodes.len()
    );
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_set(border)
        .border_style(Style::default().fg(colors.accent().to_ratatui()))
        .title(Span::styled(
            title,
            Style::default()
                .fg(colors.accent().to_ratatui())
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            format!(" {HINT} "),
            Style::default().fg(colors.muted().to_ratatui()),
        ))
        .style(Style::default().bg(colors.bg().to_ratatui()));

    frame.render_widget(block, panel);
    frame.render_widget(Paragraph::new(lines), list_area);

    if list_len > content_h {
        let max_scroll = list_len.saturating_sub(content_h);
        let mut scrollbar_state = ScrollbarState::new(max_scroll)
            .position(scroll_start)
            .viewport_content_length(content_h);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .thumb_style(Style::default().fg(colors.accent().to_ratatui()))
            .track_style(Style::default().fg(colors.dim().to_ratatui()));
        frame.render_stateful_widget(scrollbar, list_area, &mut scrollbar_state);
    }
}

/// Helper to get clicked episode index from mouse coordinate.
pub fn episode_at_mouse(
    picker: &EpisodePicker,
    area: Rect,
    mouse_x: u16,
    mouse_y: u16,
) -> Option<usize> {
    if !picker.open || picker.episodes.is_empty() {
        return None;
    }
    let max_w = (area.width as usize).saturating_sub(6).max(40);
    let max_name_len = picker
        .episodes
        .iter()
        .map(|ep| ep.name.chars().count() + 18)
        .max()
        .unwrap_or(40);
    let width = max_name_len.clamp(50, max_w) as u16;

    let max_h = (area.height as usize).saturating_sub(4).max(8);
    let needed_h = (picker.episodes.len() + 4).min(max_h);
    let height = needed_h as u16;

    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height,
    };

    let content_h = height.saturating_sub(3) as usize;
    let list_area = Rect {
        x: panel.x + 2,
        y: panel.y + 2,
        width: panel.width.saturating_sub(4),
        height: content_h as u16,
    };

    if mouse_x < list_area.x
        || mouse_x >= list_area.right()
        || mouse_y < list_area.y
        || mouse_y >= list_area.bottom()
    {
        return None;
    }

    let list_len = picker.episodes.len();
    let scroll_start = if picker.selected < content_h / 2 {
        0
    } else if picker.selected + content_h / 2 >= list_len {
        list_len.saturating_sub(content_h)
    } else {
        picker.selected - content_h / 2
    };

    let rel_row = (mouse_y - list_area.y) as usize;
    let ep_idx = scroll_start + rel_row;
    if ep_idx < picker.episodes.len() {
        Some(ep_idx)
    } else {
        None
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
