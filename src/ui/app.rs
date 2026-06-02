//! TUI application state and render loop.

use crate::comments::model::CommentStore;
use crate::diff::model::{Changeset, LineKind, Side};
use crate::ui::render_rows::{build_rows, Row, RowKind};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::path::PathBuf;
use std::time::Duration;

/// Read-only diff/comment viewer state. Comments are loaded from a sidecar and
/// only displayed/navigated — hew never mutates them.
pub struct App {
    title: String,
    changeset: Changeset,
    rows: Vec<Row>,
    comments: CommentStore,
    selected: usize, // index into rows
    scroll: usize,   // top row of viewport
    height: usize,   // last known viewport height
    status: String,
    quit: bool,
}

impl App {
    #[allow(dead_code)] // convenience constructor; ui::run uses with_comments
    pub fn new(title: String, changeset: Changeset) -> Self {
        Self::with_comments(title, changeset, CommentStore::new())
    }

    /// Construct with a pre-loaded comment store (e.g. from a sidecar JSON).
    pub fn with_comments(title: String, changeset: Changeset, comments: CommentStore) -> Self {
        let rows = build_rows(&changeset);
        let mut app = App {
            title,
            changeset,
            rows,
            comments,
            selected: 0,
            scroll: 0,
            height: 1,
            status:
                "q quit  j/k move  ^d/^u half  spc/b page  ^e/^y scroll  g/G top/bot  n/N comment"
                    .into(),
            quit: false,
        };
        app.selected = app.first_selectable().unwrap_or(0);
        app
    }

    fn first_selectable(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.is_selectable())
    }

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key.code, key.modifiers);
                    }
                }
            }
        }
        Ok(())
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let page = self.height.max(1);
        let half = (self.height / 2).max(1);
        match code {
            KeyCode::Char('q') => self.quit = true,

            // Line movement.
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1, 1),

            // Half-page: Ctrl-D / Ctrl-U (vim/less).
            KeyCode::Char('d') if ctrl => self.move_by(1, half),
            KeyCode::Char('u') if ctrl => self.move_by(-1, half),

            // Full page: Space / Ctrl-F / PageDown forward, b / Ctrl-B / PageUp back.
            KeyCode::Char(' ') | KeyCode::Char('f') | KeyCode::PageDown => self.move_by(1, page),
            KeyCode::Char('b') | KeyCode::PageUp => self.move_by(-1, page),

            // One-line viewport scroll, cursor stays in view: Ctrl-E / Ctrl-Y (less/vim).
            KeyCode::Char('e') if ctrl => self.scroll_view(1),
            KeyCode::Char('y') if ctrl => self.scroll_view(-1),

            // Top / bottom.
            KeyCode::Char('g') | KeyCode::Home => {
                self.selected = self.first_selectable().unwrap_or(0);
                self.ensure_visible();
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self
                    .rows
                    .iter()
                    .rposition(|r| r.is_selectable())
                    .unwrap_or(0);
                self.ensure_visible();
            }

            // Jump between comment threads.
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < 0 || i as usize >= self.rows.len() {
                return;
            }
            if self.rows[i as usize].is_selectable() {
                self.selected = i as usize;
                self.ensure_visible();
                return;
            }
        }
    }

    /// Move the selection `count` selectable rows in `dir` (+1 down / -1 up).
    fn move_by(&mut self, dir: isize, count: usize) {
        for _ in 0..count {
            let before = self.selected;
            self.move_selection(dir);
            if self.selected == before {
                break; // hit top/bottom
            }
        }
    }

    /// Scroll the viewport by `delta` rows, dragging the selection back into
    /// view if it would fall outside (less/vim Ctrl-E / Ctrl-Y behavior).
    fn scroll_view(&mut self, delta: isize) {
        let max_top = self.rows.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max_top) as usize;
        if self.selected < self.scroll {
            if let Some(i) = self.nearest_selectable(self.scroll, 1) {
                self.selected = i;
            }
        } else if self.selected >= self.scroll + self.height {
            let last = self.scroll + self.height.saturating_sub(1);
            if let Some(i) = self.nearest_selectable(last, -1) {
                self.selected = i;
            }
        }
    }

    /// First selectable row at or beyond `from` scanning in `dir`.
    fn nearest_selectable(&self, from: usize, dir: isize) -> Option<usize> {
        let mut i = from as isize;
        while i >= 0 && (i as usize) < self.rows.len() {
            if self.rows[i as usize].is_selectable() {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.height {
            self.scroll = self.selected + 1 - self.height;
        }
    }

    fn selected_anchor(&self) -> Option<(PathBuf, Side, u32)> {
        let row = self.rows.get(self.selected)?;
        let (side, line) = row.anchor()?;
        let file = self.changeset.files.get(row.file_idx)?;
        Some((PathBuf::from(file.display_path()), side, line))
    }

    fn jump_comment(&mut self, dir: isize) {
        // Collect rows that carry a thread anchor.
        let mut targets: Vec<usize> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            if let (Some((side, line)), Some(file)) =
                (row.anchor(), self.changeset.files.get(row.file_idx))
            {
                let path = PathBuf::from(file.display_path());
                if self
                    .comments
                    .threads
                    .iter()
                    .any(|t| t.file == path && t.side == side && t.range.contains(line))
                {
                    targets.push(i);
                }
            }
        }
        if targets.is_empty() {
            self.status = "no comments".into();
            return;
        }
        let next = if dir > 0 {
            targets
                .iter()
                .find(|&&i| i > self.selected)
                .copied()
                .or_else(|| targets.first().copied())
        } else {
            targets
                .iter()
                .rev()
                .find(|&&i| i < self.selected)
                .copied()
                .or_else(|| targets.last().copied())
        };
        if let Some(i) = next {
            self.selected = i;
            self.ensure_visible();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        // Title bar.
        let n = self.comments.count();
        let title = format!(
            " hew — {}  ({} comment{}) ",
            self.title,
            n,
            if n == 1 { "" } else { "s" }
        );
        f.render_widget(
            Paragraph::new(title).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
            chunks[0],
        );

        self.height = chunks[1].height as usize;
        self.render_diff(f, chunks[1]);

        // Status line.
        f.render_widget(
            Paragraph::new(self.status.clone()).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );

        self.render_comment_popup(f, area);
    }

    fn render_diff(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let end = (self.scroll + self.height).min(self.rows.len());
        for idx in self.scroll..end {
            let row = &self.rows[idx];
            let selected = idx == self.selected;
            lines.push(self.row_to_line(row, selected));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn row_to_line(&self, row: &Row, selected: bool) -> Line<'static> {
        let marker = self.thread_marker(row);
        let (content, base) = match &row.kind {
            RowKind::FileHeader => (
                format!("▌ {}", row.text),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(40, 44, 52))
                    .add_modifier(Modifier::BOLD),
            ),
            RowKind::HunkHeader => (row.text.clone(), Style::default().fg(Color::Magenta)),
            RowKind::Line {
                kind,
                old_line,
                new_line,
            } => {
                let num = format!(
                    "{:>5} {:>5} ",
                    old_line.map(|n| n.to_string()).unwrap_or_default(),
                    new_line.map(|n| n.to_string()).unwrap_or_default(),
                );
                let style = match kind {
                    LineKind::Addition => Style::default().fg(Color::Green),
                    LineKind::Deletion => Style::default().fg(Color::Red),
                    LineKind::Context => Style::default().fg(Color::Gray),
                };
                (format!("{num}{}", row.text), style)
            }
        };
        let mut style = base;
        if selected {
            style = style
                .bg(Color::Rgb(60, 66, 80))
                .add_modifier(Modifier::BOLD);
        }
        Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(content, style),
        ])
    }

    /// A gutter marker showing whether a thread (resolved/open) sits here.
    fn thread_marker(&self, row: &Row) -> String {
        if let (Some((side, line)), Some(file)) =
            (row.anchor(), self.changeset.files.get(row.file_idx))
        {
            let path = PathBuf::from(file.display_path());
            let here: Vec<_> = self
                .comments
                .threads
                .iter()
                .filter(|t| t.file == path && t.side == side && t.range.contains(line))
                .collect();
            if here.iter().any(|t| !t.resolved) {
                return "● ".into();
            } else if !here.is_empty() {
                return "○ ".into();
            }
        }
        "  ".into()
    }

    /// Show threads anchored at the current line in a popup.
    fn render_comment_popup(&self, f: &mut Frame, area: Rect) {
        let Some((file, side, line)) = self.selected_anchor() else {
            return;
        };
        let threads: Vec<_> = self
            .comments
            .threads
            .iter()
            .filter(|t| t.file == file && t.side == side && t.range.contains(line))
            .collect();
        if threads.is_empty() {
            return;
        }
        let mut text: Vec<Line> = Vec::new();
        for t in threads {
            let head = format!(
                "{} {}:{}-{} {}",
                if t.resolved { "[resolved]" } else { "[open]" },
                t.file.display(),
                t.range.start,
                t.range.end,
                ""
            );
            text.push(Line::from(Span::styled(
                head,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for c in &t.comments {
                let who = c.author.clone().unwrap_or_else(|| "?".into());
                text.push(Line::from(format!("  @{who}: {}", c.body)));
            }
            text.push(Line::from(""));
        }

        let w = (area.width as f32 * 0.6) as u16;
        let h = (text.len() as u16 + 2)
            .min(area.height.saturating_sub(2))
            .max(3);
        let popup = Rect {
            x: area.width.saturating_sub(w).saturating_sub(1),
            y: area.height.saturating_sub(h).saturating_sub(1),
            width: w,
            height: h,
        };
        f.render_widget(Clear, popup);
        f.render_widget(
            Paragraph::new(text)
                .block(Block::default().borders(Borders::ALL).title(" thread "))
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}
