//! TUI application state and render loop.

use crate::comments::model::CommentStore;
use crate::diff::model::{Changeset, LineKind, Side};
use crate::ui::highlight::Highlighter;
use crate::ui::render_rows::{
    build_rows, build_split_rows, Row, RowKind, SideCell, SplitRow, SplitRowKind,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

const ADD_BG: Color = Color::Rgb(20, 42, 24);
const DEL_BG: Color = Color::Rgb(48, 24, 26);
const SEL_BG: Color = Color::Rgb(60, 66, 80);

/// Highlighted runs for one line: `(fg color, text)`.
type LineRuns = Rc<Vec<(Color, String)>>;
/// Cache key: which file + the exact line text.
type HlKey = (usize, String);

/// File inputs to reload from when `--watch` is set. `patch` is `None` when the
/// diff came from stdin (a stream can't be re-read).
pub struct WatchPaths {
    pub patch: Option<PathBuf>,
    pub comments: Option<PathBuf>,
}

/// Tracks watched files and their last-seen modification times.
struct Watch {
    patch: Option<PathBuf>,
    comments: Option<PathBuf>,
    patch_mtime: Option<SystemTime>,
    comments_mtime: Option<SystemTime>,
}

fn file_mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Diff layout: unified (stacked) or split (old | new, like `git delta`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Unified,
    Split,
}

/// Read-only diff/comment viewer state. Comments are loaded from a sidecar and
/// only displayed/navigated — hew never mutates them.
pub struct App {
    title: String,
    changeset: Changeset,
    rows: Vec<Row>,
    comments: CommentStore,
    split_rows: Vec<SplitRow>,
    view: View,
    selected: usize, // index into the active row list
    scroll: usize,   // top row of viewport
    height: usize,   // last known viewport height
    status: String,
    watch: Option<Watch>,
    needs_clear: bool,
    highlighter: Highlighter,
    /// (file_idx, line text) -> highlighted runs. Viewport-only, grows lazily.
    hl_cache: RefCell<HashMap<HlKey, LineRuns>>,
    quit: bool,
}

impl App {
    /// Construct with a pre-loaded comment store (e.g. from a sidecar JSON).
    pub fn with_comments(title: String, changeset: Changeset, comments: CommentStore) -> Self {
        let rows = build_rows(&changeset);
        let split_rows = build_split_rows(&changeset);
        let mut app = App {
            title,
            changeset,
            rows,
            split_rows,
            view: View::Unified,
            comments,
            selected: 0,
            scroll: 0,
            height: 1,
            status: "q quit  j/k move  ^d/^u half  spc/b page  g/G top/bot  n/N comment  tab split"
                .into(),
            watch: None,
            needs_clear: false,
            highlighter: Highlighter::new(),
            hl_cache: RefCell::new(HashMap::new()),
            quit: false,
        };
        app.selected = app.first_selectable().unwrap_or(0);
        app
    }

    /// Highlighted `(color, text)` runs for a line, cached per (file, text).
    fn highlight(&self, file_idx: usize, text: &str) -> LineRuns {
        let key = (file_idx, text.to_string());
        if let Some(v) = self.hl_cache.borrow().get(&key) {
            return v.clone();
        }
        let spans = match self.changeset.files.get(file_idx) {
            Some(f) => {
                let syntax = self.highlighter.syntax_for(f.display_path());
                self.highlighter.line(syntax, text)
            }
            None => vec![(Color::Gray, text.to_string())],
        };
        let rc = Rc::new(spans);
        self.hl_cache.borrow_mut().insert(key, rc.clone());
        rc
    }

    /// Highlighted spans for `text`, truncated/padded to exactly `width` chars,
    /// with an optional background applied to every run (and the padding).
    fn styled_fit(
        &self,
        file_idx: usize,
        text: &str,
        width: usize,
        bg: Option<Color>,
    ) -> Vec<Span<'static>> {
        let hl = self.highlight(file_idx, text);
        let mut out = Vec::new();
        let mut used = 0usize;
        for (c, s) in hl.iter() {
            if used >= width {
                break;
            }
            let take: String = s.chars().take(width - used).collect();
            if take.is_empty() {
                continue;
            }
            used += take.chars().count();
            let mut st = Style::default().fg(*c);
            if let Some(b) = bg {
                st = st.bg(b);
            }
            out.push(Span::styled(take, st));
        }
        if used < width {
            let mut st = Style::default();
            if let Some(b) = bg {
                st = st.bg(b);
            }
            out.push(Span::styled(" ".repeat(width - used), st));
        }
        out
    }

    /// Enable `--watch`: reload the patch and/or comments file when they change.
    pub fn watching(mut self, paths: WatchPaths) -> Self {
        self.watch = Some(Watch {
            patch_mtime: paths.patch.as_deref().and_then(file_mtime),
            comments_mtime: paths.comments.as_deref().and_then(file_mtime),
            patch: paths.patch,
            comments: paths.comments,
        });
        self
    }

    fn first_selectable(&self) -> Option<usize> {
        (0..self.active_len()).find(|&i| self.is_selectable_at(i))
    }

    fn last_selectable(&self) -> Option<usize> {
        (0..self.active_len())
            .rev()
            .find(|&i| self.is_selectable_at(i))
    }

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        while !self.quit {
            if self.needs_clear {
                terminal.clear()?;
                self.needs_clear = false;
            }
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key.code, key.modifiers);
                    }
                }
            }
            self.poll_reload();
        }
        Ok(())
    }

    /// On `--watch`: if any watched file's mtime changed, reload it.
    fn poll_reload(&mut self) {
        let changed = match self.watch.as_mut() {
            None => false,
            Some(w) => {
                let mut changed = false;
                if let Some(p) = &w.patch {
                    let m = file_mtime(p);
                    if m != w.patch_mtime {
                        w.patch_mtime = m;
                        changed = true;
                    }
                }
                if let Some(p) = &w.comments {
                    let m = file_mtime(p);
                    if m != w.comments_mtime {
                        w.comments_mtime = m;
                        changed = true;
                    }
                }
                changed
            }
        };
        if changed {
            self.reload();
        }
    }

    /// Re-read the watched patch/comments and rebuild, keeping the cursor sane.
    fn reload(&mut self) {
        let (patch, comments) = match &self.watch {
            Some(w) => (w.patch.clone(), w.comments.clone()),
            None => return,
        };
        if let Some(p) = patch {
            match crate::loader::load_patch(Some(&p)) {
                Ok(cs) => {
                    self.changeset = cs;
                    self.rows = build_rows(&self.changeset);
                    self.split_rows = build_split_rows(&self.changeset);
                    self.hl_cache.borrow_mut().clear();
                }
                Err(e) => {
                    self.status = format!("reload failed: {e}");
                    return;
                }
            }
        }
        if let Some(p) = comments {
            match crate::loader::load_comments(&p) {
                Ok(store) => self.comments = store,
                Err(e) => self.status = format!("comments reload failed: {e}"),
            }
        }
        // Keep the selection on a valid, selectable row after the rebuild.
        let max = self.active_len().saturating_sub(1);
        if self.selected > max || !self.is_selectable_at(self.selected) {
            self.selected = self
                .nearest_selectable(self.selected.min(max), 1)
                .or_else(|| self.first_selectable())
                .unwrap_or(0);
        }
        self.ensure_visible();
        self.status = "reloaded".into();
    }

    // ---- active-list abstraction (unified vs split) ----

    fn active_len(&self) -> usize {
        match self.view {
            View::Unified => self.rows.len(),
            View::Split => self.split_rows.len(),
        }
    }

    fn is_selectable_at(&self, i: usize) -> bool {
        match self.view {
            View::Unified => self.rows.get(i).is_some_and(|r| r.is_selectable()),
            View::Split => self.split_rows.get(i).is_some_and(|r| r.is_selectable()),
        }
    }

    /// `(file_idx, side, line)` anchor for the row at `i`, if it carries one.
    fn anchor_at(&self, i: usize) -> Option<(usize, Side, u32)> {
        match self.view {
            View::Unified => {
                let r = self.rows.get(i)?;
                let (s, l) = r.anchor()?;
                Some((r.file_idx, s, l))
            }
            View::Split => {
                let r = self.split_rows.get(i)?;
                let (s, l) = r.anchor()?;
                Some((r.file_idx, s, l))
            }
        }
    }

    /// Toggle between unified and split, keeping the cursor on the same line.
    fn toggle_view(&mut self) {
        let anchor = self.anchor_at(self.selected);
        self.view = match self.view {
            View::Unified => View::Split,
            View::Split => View::Unified,
        };
        // Re-find the same (file, side, line) in the other layout.
        let target = anchor.and_then(|a| {
            (0..self.active_len())
                .find(|&i| self.is_selectable_at(i) && self.anchor_at(i) == Some(a))
        });
        self.selected = target
            .or_else(|| self.first_selectable())
            .unwrap_or(0)
            .min(self.active_len().saturating_sub(1));
        // Recenter so the cursor is roughly mid-viewport.
        self.scroll = self.selected.saturating_sub(self.height / 2);
        self.ensure_visible();
        self.status = match self.view {
            View::Unified => "unified view".into(),
            View::Split => "split view".into(),
        };
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
                self.selected = self.last_selectable().unwrap_or(0);
                self.ensure_visible();
            }

            // Jump between comment threads.
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),

            // Toggle unified <-> split (side-by-side) layout.
            KeyCode::Tab | KeyCode::Char('s') => self.toggle_view(),

            // Force a full repaint (escape hatch if the terminal gets dirtied).
            KeyCode::Char('l') if ctrl => self.needs_clear = true,
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < 0 || i as usize >= self.active_len() {
                return;
            }
            if self.is_selectable_at(i as usize) {
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
        let max_top = self.active_len().saturating_sub(1) as isize;
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
        while i >= 0 && (i as usize) < self.active_len() {
            if self.is_selectable_at(i as usize) {
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
        let (file_idx, side, line) = self.anchor_at(self.selected)?;
        let file = self.changeset.files.get(file_idx)?;
        Some((PathBuf::from(file.display_path()), side, line))
    }

    fn jump_comment(&mut self, dir: isize) {
        // Collect rows that carry a thread anchor.
        let mut targets: Vec<usize> = Vec::new();
        for i in 0..self.active_len() {
            if let Some((file_idx, side, line)) = self.anchor_at(i) {
                if let Some(file) = self.changeset.files.get(file_idx) {
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
        match self.view {
            View::Unified => self.render_unified(f, area),
            View::Split => self.render_split(f, area),
        }
    }

    fn render_unified(&self, f: &mut Frame, area: Rect) {
        let width = area.width as usize;
        let mut lines: Vec<Line> = Vec::new();
        let end = (self.scroll + self.height).min(self.rows.len());
        for idx in self.scroll..end {
            let row = &self.rows[idx];
            let selected = idx == self.selected;
            lines.push(self.row_to_line(row, selected, width));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn render_split(&self, f: &mut Frame, area: Rect) {
        let total = area.width as usize;
        let divider = " │ ";
        let side_w = total.saturating_sub(divider.len()) / 2;
        let mut lines: Vec<Line> = Vec::new();
        let end = (self.scroll + self.height).min(self.split_rows.len());
        for idx in self.scroll..end {
            let row = &self.split_rows[idx];
            let selected = idx == self.selected;
            lines.push(self.split_row_to_line(row, selected, side_w, divider));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn split_row_to_line(
        &self,
        row: &SplitRow,
        selected: bool,
        side_w: usize,
        divider: &str,
    ) -> Line<'static> {
        match &row.kind {
            SplitRowKind::FileHeader => Line::from(Span::styled(
                format!("▌ {}", row.text),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(40, 44, 52))
                    .add_modifier(Modifier::BOLD),
            )),
            SplitRowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default().fg(Color::Magenta),
            )),
            SplitRowKind::Pair { left, right } => {
                let mut spans =
                    self.side_spans(left.as_ref(), Side::Old, row.file_idx, side_w, selected);
                spans.push(Span::styled(
                    divider.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
                spans.extend(self.side_spans(
                    right.as_ref(),
                    Side::New,
                    row.file_idx,
                    side_w,
                    selected,
                ));
                Line::from(spans)
            }
        }
    }

    /// Render one side (old/new) of a split pair into spans of width `width`.
    fn side_spans(
        &self,
        cell: Option<&SideCell>,
        side: Side,
        file_idx: usize,
        width: usize,
        selected: bool,
    ) -> Vec<Span<'static>> {
        const PREFIX: usize = 7; // marker(2) + line number(4) + space(1)
        match cell {
            None => vec![Span::styled(
                " ".repeat(width),
                Style::default().bg(Color::Rgb(28, 30, 34)),
            )],
            Some(c) => {
                let marker = self.marker(file_idx, side, c.line);
                let num = c
                    .line
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let bg = if selected {
                    Some(SEL_BG)
                } else {
                    match c.kind {
                        LineKind::Addition => Some(ADD_BG),
                        LineKind::Deletion => Some(DEL_BG),
                        LineKind::Context => None,
                    }
                };
                let mut spans = vec![
                    Span::styled(marker, Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{num} "), Style::default().fg(Color::DarkGray)),
                ];
                spans.extend(self.styled_fit(file_idx, &c.text, width.saturating_sub(PREFIX), bg));
                spans
            }
        }
    }

    fn row_to_line(&self, row: &Row, selected: bool, width: usize) -> Line<'static> {
        match &row.kind {
            RowKind::FileHeader => {
                let st = Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(40, 44, 52))
                    .add_modifier(Modifier::BOLD);
                let text = format!("▌ {}", row.text);
                let pad = width.saturating_sub(text.chars().count());
                Line::from(vec![
                    Span::styled(text, st),
                    Span::styled(" ".repeat(pad), st),
                ])
            }
            RowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default().fg(Color::Magenta),
            )),
            RowKind::Line {
                kind,
                old_line,
                new_line,
            } => {
                let marker = self.thread_marker(row);
                let num = format!(
                    "{:>5} {:>5} ",
                    old_line.map(|n| n.to_string()).unwrap_or_default(),
                    new_line.map(|n| n.to_string()).unwrap_or_default(),
                );
                let (sign, code) = row.text.split_at(1);
                let bg = if selected {
                    Some(SEL_BG)
                } else {
                    match kind {
                        LineKind::Addition => Some(ADD_BG),
                        LineKind::Deletion => Some(DEL_BG),
                        LineKind::Context => None,
                    }
                };
                let sign_color = match kind {
                    LineKind::Addition => Color::Green,
                    LineKind::Deletion => Color::Red,
                    LineKind::Context => Color::DarkGray,
                };
                let with_bg = |st: Style| match bg {
                    Some(b) => st.bg(b),
                    None => st,
                };
                let mut used = marker.chars().count() + num.chars().count() + 1;
                let mut spans = vec![
                    Span::styled(marker, with_bg(Style::default().fg(Color::Cyan))),
                    Span::styled(num, with_bg(Style::default().fg(Color::DarkGray))),
                    Span::styled(sign.to_string(), with_bg(Style::default().fg(sign_color))),
                ];
                // Highlighted code, with the diff background tint behind it.
                let hl = self.highlight(row.file_idx, code);
                for (c, s) in hl.iter() {
                    used += s.chars().count();
                    spans.push(Span::styled(s.clone(), with_bg(Style::default().fg(*c))));
                }
                // Fill the rest so the tint / selection spans the whole line.
                if bg.is_some() && used < width {
                    spans.push(Span::styled(
                        " ".repeat(width - used),
                        with_bg(Style::default()),
                    ));
                }
                Line::from(spans)
            }
        }
    }

    /// Gutter marker for the row's own anchor (unified view).
    fn thread_marker(&self, row: &Row) -> &'static str {
        match row.anchor() {
            Some((side, line)) => self.marker(row.file_idx, side, Some(line)),
            None => "  ",
        }
    }

    /// Gutter marker (● open / ○ resolved / blank) for a file+side+line.
    fn marker(&self, file_idx: usize, side: Side, line: Option<u32>) -> &'static str {
        let Some(line) = line else { return "  " };
        let Some(file) = self.changeset.files.get(file_idx) else {
            return "  ";
        };
        let path = PathBuf::from(file.display_path());
        let here: Vec<_> = self
            .comments
            .threads
            .iter()
            .filter(|t| t.file == path && t.side == side && t.range.contains(line))
            .collect();
        if here.iter().any(|t| !t.resolved) {
            "● "
        } else if !here.is_empty() {
            "○ "
        } else {
            "  "
        }
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
