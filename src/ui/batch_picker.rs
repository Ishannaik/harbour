//! Batch download file selector overlay: allows users to interactively
//! select specific files / episodes to download from a multi-file batch torrent.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border::Set as BorderSet;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::core::types::TorrentFileView;
use crate::theme::Theme;

/// Batch-picker state, owned by the app loop and drawn by [`draw`].
#[derive(Debug, Clone, Default)]
pub struct BatchPicker {
    pub open: bool,
    pub torrent_id: String,
    pub torrent_name: String,
    pub magnet: Option<String>,
    pub dir: PathBuf,
    pub files: Vec<TorrentFileView>,
    pub selected: usize,
    pub checked: HashSet<usize>,
}

impl BatchPicker {
    pub fn open_for(
        &mut self,
        id: String,
        name: String,
        magnet: Option<String>,
        dir: PathBuf,
        files: Vec<TorrentFileView>,
    ) {
        self.torrent_id = id;
        self.torrent_name = name;
        self.magnet = magnet;
        self.dir = dir;
        self.checked = files.iter().map(|f| f.id).collect(); // Default: check all
        self.files = files;
        self.selected = 0;
        self.open = true;
    }

    pub fn select_next(&mut self) {
        if !self.files.is_empty() {
            self.selected = (self.selected + 1).min(self.files.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn page_down(&mut self) {
        if !self.files.is_empty() {
            self.selected = (self.selected + 6).min(self.files.len() - 1);
        }
    }

    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(6);
    }

    pub fn toggle_selected(&mut self) {
        if let Some(file) = self.files.get(self.selected) {
            if self.checked.contains(&file.id) {
                self.checked.remove(&file.id);
            } else {
                self.checked.insert(file.id);
            }
        }
    }

    pub fn toggle_index(&mut self, idx: usize) {
        if let Some(file) = self.files.get(idx) {
            if self.checked.contains(&file.id) {
                self.checked.remove(&file.id);
            } else {
                self.checked.insert(file.id);
            }
        }
    }

    pub fn select_all(&mut self) {
        self.checked = self.files.iter().map(|f| f.id).collect();
    }

    pub fn unselect_all(&mut self) {
        self.checked.clear();
    }

    pub fn invert_selection(&mut self) {
        let mut new_checked = HashSet::new();
        for f in &self.files {
            if !self.checked.contains(&f.id) {
                new_checked.insert(f.id);
            }
        }
        self.checked = new_checked;
    }

    pub fn selected_size_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| self.checked.contains(&f.id))
            .map(|f| f.size_bytes)
            .sum()
    }
}

/// The hint line at the footer of the batch file picker modal.
pub const HINT: &str = "↑/↓ move · space toggle · a all · u none · enter download · esc cancel";

/// Draws the batch file picker overlay.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    picker: &BatchPicker,
    mouse_pos: Option<(u16, u16)>,
) {
    if !picker.open || picker.files.is_empty() {
        return;
    }
    let colors = &theme.colors;

    // Dim the backdrop
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(colors.dim().to_ratatui())),
        area,
    );

    let max_w = (area.width as usize).saturating_sub(6).max(40);
    let max_name_len = picker
        .files
        .iter()
        .map(|f| f.name.chars().count() + 22)
        .max()
        .unwrap_or(40);
    let width = max_name_len.clamp(54, max_w) as u16;

    let total_rows = picker.files.len() + 4;
    let height = (total_rows as u16).clamp(8, area.height.saturating_sub(4).max(8));

    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

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

    let selected_size = format_bytes(picker.selected_size_bytes());
    let title = format!(
        " Select Files to Download ({}/{} · {}) ",
        picker.checked.len(),
        picker.files.len(),
        selected_size
    );

    let panel_bg = colors.bg().to_ratatui();
    let border_fg = colors.border().to_ratatui();
    let accent_fg = colors.accent().to_ratatui();

    let block = ratatui::widgets::Block::new()
        .borders(ratatui::widgets::Borders::ALL)
        .border_set(border)
        .border_style(Style::default().fg(border_fg))
        .title(Span::styled(
            title,
            Style::default().fg(accent_fg).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(panel_bg));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    if inner.height < 2 {
        return;
    }

    let list_height = (inner.height.saturating_sub(1)) as usize;
    let scroll_offset = if picker.selected >= list_height {
        picker.selected.saturating_sub(list_height - 1)
    } else {
        0
    };

    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);

    let mut lines = Vec::new();
    for (i, file) in picker
        .files
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(list_height)
    {
        let is_sel = i == picker.selected;
        let is_checked = picker.checked.contains(&file.id);
        let row_y = list_area.y + (i - scroll_offset) as u16;
        let hovered = mouse_pos
            .is_some_and(|(mx, my)| mx >= list_area.x && mx < list_area.right() && my == row_y);

        let check_glyph = if is_checked { "[✓]" } else { "[ ]" };
        let pointer = if is_sel { " ► " } else { "   " };
        let size_str = format_bytes(file.size_bytes);
        let prefix_len = pointer.len() + check_glyph.len() + 1;
        let avail_w = (list_area.width as usize).saturating_sub(prefix_len + size_str.len() + 3);
        let name_str = truncate_str(&file.name, avail_w);
        let pad = avail_w.saturating_sub(name_str.chars().count());

        let (fg, bg) = if is_sel {
            (colors.accent().to_ratatui(), colors.dim().to_ratatui())
        } else if hovered {
            (colors.text().to_ratatui(), colors.dim().to_ratatui())
        } else {
            (colors.text().to_ratatui(), colors.bg().to_ratatui())
        };

        let check_fg = if is_checked {
            colors.success().to_ratatui()
        } else {
            colors.muted().to_ratatui()
        };

        let style = if is_sel {
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(fg).bg(bg)
        };

        lines.push(Line::from(vec![
            Span::styled(pointer, style),
            Span::styled(check_glyph, Style::default().fg(check_fg).bg(bg)),
            Span::styled(format!(" {name_str}"), style),
            Span::styled(" ".repeat(pad), style),
            Span::styled(
                format!(" {size_str} "),
                Style::default().fg(colors.muted().to_ratatui()).bg(bg),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), list_area);

    if picker.files.len() > list_height {
        let max_scroll = picker.files.len().saturating_sub(list_height);
        let mut scrollbar_state = ScrollbarState::new(max_scroll)
            .position(scroll_offset)
            .viewport_content_length(list_height);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .thumb_style(Style::default().fg(accent_fg))
            .track_style(Style::default().fg(colors.dim().to_ratatui()));
        frame.render_stateful_widget(scrollbar, list_area, &mut scrollbar_state);
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            HINT,
            Style::default().fg(colors.muted().to_ratatui()),
        )])),
        hint_area,
    );
}

/// Computes mouse click row in batch picker.
pub fn file_at_mouse(picker: &BatchPicker, area: Rect, col: u16, row: u16) -> Option<usize> {
    if !picker.open || picker.files.is_empty() {
        return None;
    }
    let max_w = (area.width as usize).saturating_sub(6).max(40);
    let max_name_len = picker
        .files
        .iter()
        .map(|f| f.name.chars().count() + 22)
        .max()
        .unwrap_or(40);
    let width = max_name_len.clamp(54, max_w) as u16;

    let total_rows = picker.files.len() + 4;
    let height = (total_rows as u16).clamp(8, area.height.saturating_sub(4).max(8));

    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    if col < modal_area.x
        || col >= modal_area.right()
        || row <= modal_area.y
        || row >= modal_area.bottom() - 1
    {
        return None;
    }

    let list_height = (modal_area.height.saturating_sub(3)) as usize;
    let scroll_offset = if picker.selected >= list_height {
        picker.selected.saturating_sub(list_height - 1)
    } else {
        0
    };

    let rel_row = (row - modal_area.y - 1) as usize;
    let file_idx = scroll_offset + rel_row;
    if file_idx < picker.files.len() {
        Some(file_idx)
    } else {
        None
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
