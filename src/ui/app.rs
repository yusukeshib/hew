//! TUI application state and render loop.

use crate::comments::model::{CommentStore, LineRange};
use crate::diff::model::{Changeset, LineKind, Side};
use crate::ui::render_rows::{build_rows, Row, RowKind};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::path::PathBuf;
use std::time::Duration;

/// Editing modes for the bottom prompt.
enum Mode {
    Normal,
    /// Typing a new comment on the selected line/range.
    Comment { range_start: usize },
    /// Typing a reply to the thread at the selected line.
    Reply { thread_idx: usize },
}

pub struct App {
    title: String,
    changeset: Changeset,
    rows: Vec<Row>,
    comments: CommentStore,
    selected: usize, // index into rows
    scroll: usize,   // top row of viewport
    height: usize,   // last known viewport height
    mode: Mode,
    input: String,
    status: String,
    range_anchor: Option<usize>, // for multi-line selection start
    quit: bool,
}

impl App {
    pub fn new(title: String, changeset: Changeset) -> Self {
        let rows = build_rows(&changeset);
        let mut app = App {
            title,
            changeset,
            rows,
            comments: CommentStore::new(),
            selected: 0,
            scroll: 0,
            height: 1,
            mode: Mode::Normal,
            input: String::new(),
            status: "q quit  j/k move  c comment  V range  r reply  R resolve  d delete  n/N next/prev".into(),
            range_anchor: None,
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
        match &self.mode {
            Mode::Normal => self.on_key_normal(code, mods),
            Mode::Comment { .. } | Mode::Reply { .. } => self.on_key_input(code),
        }
    }

    fn on_key_normal(&mut self, code: KeyCode, _mods: KeyModifiers) {
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') => {
                self.selected = self.first_selectable().unwrap_or(0);
                self.range_anchor = None;
            }
            KeyCode::Char('G') => {
                self.selected = self.rows.iter().rposition(|r| r.is_selectable()).unwrap_or(0);
                self.range_anchor = None;
            }
            KeyCode::Char('V') => {
                // Toggle range-selection anchor.
                self.range_anchor = match self.range_anchor {
                    Some(_) => None,
                    None => Some(self.selected),
                };
            }
            KeyCode::Char('c') => self.start_comment(),
            KeyCode::Char('r') => self.start_reply(),
            KeyCode::Char('R') => self.toggle_resolve(),
            KeyCode::Char('d') => self.delete_thread(),
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),
            _ => {}
        }
    }

    fn on_key_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.input.clear();
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
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

    fn start_comment(&mut self) {
        if self.selected_anchor().is_none() {
            self.status = "cannot comment here".into();
            return;
        }
        let start = self.range_anchor.unwrap_or(self.selected);
        self.mode = Mode::Comment { range_start: start };
        self.input.clear();
    }

    fn start_reply(&mut self) {
        if let Some((file, side, line)) = self.selected_anchor() {
            if let Some(idx) = self
                .comments
                .threads
                .iter()
                .position(|t| t.file == file && t.side == side && t.range.contains(line))
            {
                self.mode = Mode::Reply { thread_idx: idx };
                self.input.clear();
                return;
            }
        }
        self.status = "no thread here to reply to".into();
    }

    fn submit_input(&mut self) {
        let body = self.input.trim().to_string();
        if body.is_empty() {
            self.mode = Mode::Normal;
            return;
        }
        match &self.mode {
            Mode::Comment { range_start } => {
                let start_row = *range_start;
                if let Some((file, side, line)) = self.selected_anchor() {
                    // Compute the line range across the selection.
                    let other = self
                        .rows
                        .get(start_row)
                        .and_then(|r| r.anchor())
                        .map(|(_, l)| l)
                        .unwrap_or(line);
                    let range = LineRange { start: other.min(line), end: other.max(line) };
                    self.comments.add_thread(file, side, range, Some("you".into()), body);
                    self.status = "comment added".into();
                    self.range_anchor = None;
                }
            }
            Mode::Reply { thread_idx } => {
                if let Some(t) = self.comments.threads.get(*thread_idx) {
                    let id = t.id;
                    self.comments.reply(id, Some("you".into()), body);
                    self.status = "reply added".into();
                }
            }
            Mode::Normal => {}
        }
        self.mode = Mode::Normal;
        self.input.clear();
    }

    fn toggle_resolve(&mut self) {
        if let Some((file, side, line)) = self.selected_anchor() {
            if let Some(t) = self
                .comments
                .threads
                .iter_mut()
                .find(|t| t.file == file && t.side == side && t.range.contains(line))
            {
                t.resolved = !t.resolved;
                self.status = if t.resolved { "resolved" } else { "unresolved" }.into();
                return;
            }
        }
        self.status = "no thread here".into();
    }

    fn delete_thread(&mut self) {
        if let Some((file, side, line)) = self.selected_anchor() {
            if let Some(id) = self
                .comments
                .threads
                .iter()
                .find(|t| t.file == file && t.side == side && t.range.contains(line))
                .map(|t| t.id)
            {
                self.comments.remove_thread(id);
                self.status = "thread deleted".into();
                return;
            }
        }
        self.status = "no thread here".into();
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
            targets.iter().find(|&&i| i > self.selected).copied().or_else(|| targets.first().copied())
        } else {
            targets.iter().rev().find(|&&i| i < self.selected).copied().or_else(|| targets.last().copied())
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
            .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        // Title bar.
        let n = self.comments.count();
        let title = format!(" hew — {}  ({} comment{}) ", self.title, n, if n == 1 { "" } else { "s" });
        f.render_widget(
            Paragraph::new(title).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
            chunks[0],
        );

        self.height = chunks[1].height as usize;
        self.render_diff(f, chunks[1]);

        // Status / prompt.
        match &self.mode {
            Mode::Comment { .. } => {
                f.render_widget(
                    Paragraph::new(format!("comment> {}", self.input))
                        .style(Style::default().fg(Color::Yellow)),
                    chunks[2],
                );
            }
            Mode::Reply { .. } => {
                f.render_widget(
                    Paragraph::new(format!("reply> {}", self.input))
                        .style(Style::default().fg(Color::Yellow)),
                    chunks[2],
                );
            }
            Mode::Normal => {
                f.render_widget(
                    Paragraph::new(self.status.clone()).style(Style::default().fg(Color::DarkGray)),
                    chunks[2],
                );
            }
        }

        self.render_comment_popup(f, area);
    }

    fn render_diff(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let end = (self.scroll + self.height).min(self.rows.len());
        for idx in self.scroll..end {
            let row = &self.rows[idx];
            let selected = idx == self.selected;
            let in_range = match self.range_anchor {
                Some(a) => {
                    let (lo, hi) = (a.min(self.selected), a.max(self.selected));
                    idx >= lo && idx <= hi
                }
                None => false,
            };
            lines.push(self.row_to_line(row, selected, in_range));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn row_to_line(&self, row: &Row, selected: bool, in_range: bool) -> Line {
        let marker = self.thread_marker(row);
        let (content, base) = match &row.kind {
            RowKind::FileHeader => (
                format!("▌ {}", row.text),
                Style::default().fg(Color::White).bg(Color::Rgb(40, 44, 52)).add_modifier(Modifier::BOLD),
            ),
            RowKind::HunkHeader => (
                row.text.clone(),
                Style::default().fg(Color::Magenta),
            ),
            RowKind::Line { kind, old_line, new_line } => {
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
            style = style.bg(Color::Rgb(60, 66, 80)).add_modifier(Modifier::BOLD);
        } else if in_range {
            style = style.bg(Color::Rgb(50, 50, 70));
        }
        Line::from(vec![Span::styled(marker, Style::default().fg(Color::Cyan)), Span::styled(content, style)])
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
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let Some((file, side, line)) = self.selected_anchor() else { return };
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
            text.push(Line::from(Span::styled(head, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
            for c in &t.comments {
                let who = c.author.clone().unwrap_or_else(|| "?".into());
                text.push(Line::from(format!("  @{who}: {}", c.body)));
            }
            text.push(Line::from(""));
        }

        let w = (area.width as f32 * 0.6) as u16;
        let h = (text.len() as u16 + 2).min(area.height.saturating_sub(2)).max(3);
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
