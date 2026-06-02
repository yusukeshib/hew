//! TUI application state and render loop.

use crate::comments::model::CommentStore;
use crate::diff::model::{Changeset, LineKind, Side};
use crate::ui::highlight::Highlighter;
use crate::ui::render_rows::{
    build_rows, build_split_rows, Row, RowKind, SideCell, SplitRow, SplitRowKind,
};
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
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

/// Which pane keyboard navigation acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Diff,
}

/// A row in the file list: a directory header, a file entry (by file index),
/// or a comment thread (by index into the comment store) nested under its file.
enum SbRow {
    Dir(String),
    File(usize),
    Thread(usize),
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

fn base_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Truncate `s` from the right (keeping the head) to fit `w` columns.
fn elide_right(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(w - 1).collect();
    out.push('…');
    out
}

/// Group files by directory (keeping file order), inserting a header row when
/// the directory changes. Returns the rows plus a `file_idx -> row` map.
fn build_sidebar_rows(
    changeset: &Changeset,
    comments: &CommentStore,
) -> (Vec<SbRow>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut map = vec![0usize; changeset.files.len()];
    let mut last_dir: Option<String> = None;
    for (i, f) in changeset.files.iter().enumerate() {
        let dir = dir_of(f.display_path());
        if last_dir.as_deref() != Some(dir) {
            rows.push(SbRow::Dir(if dir.is_empty() {
                ".".into()
            } else {
                dir.into()
            }));
            last_dir = Some(dir.to_string());
        }
        map[i] = rows.len();
        rows.push(SbRow::File(i));
        // Comment threads anchored to this file, nested one level deeper.
        let path = PathBuf::from(f.display_path());
        for (ti, _) in comments
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.file == path)
        {
            rows.push(SbRow::Thread(ti));
        }
    }
    (rows, map)
}

const SIDEBAR_WIDTH: u16 = 30;
const MIN_SIDEBAR: u16 = 14;
const MIN_DIFF: u16 = 20;
/// Selection background when the pane is focused / unfocused.
const FOCUS_BG: Color = SEL_BG;
const UNFOCUS_BG: Color = Color::Rgb(40, 42, 48);

/// Per-file (additions, deletions) counts for the sidebar.
fn file_stats(changeset: &Changeset) -> Vec<(usize, usize)> {
    changeset
        .files
        .iter()
        .map(|f| {
            let mut adds = 0;
            let mut dels = 0;
            for h in &f.hunks {
                for l in &h.lines {
                    match l.kind {
                        LineKind::Addition => adds += 1,
                        LineKind::Deletion => dels += 1,
                        LineKind::Context => {}
                    }
                }
            }
            (adds, dels)
        })
        .collect()
}

/// Minimal standard base64 (no deps) for OSC 52 clipboard writes.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Is `(col, row)` inside `rect`?
fn hit(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Map a click/drag at terminal `row` on a vertical scrollbar track to a top
/// line index, placing the thumb's top under the cursor. Mirrors ratatui's
/// thumb geometry so the thumb actually follows the pointer.
fn sb_thumb_pos(track_y: u16, track_h: usize, total: usize, viewport: usize, row: u16) -> usize {
    let max_top = total.saturating_sub(viewport);
    if max_top == 0 || track_h == 0 {
        return 0;
    }
    // Thumb length matches the render model below (content_length = max_top + 1,
    // viewport_content_length = viewport): thumb = viewport * track / total.
    let thumb = ((viewport as f32) * (track_h as f32) / (total as f32))
        .round()
        .max(1.0) as usize;
    let span = track_h.saturating_sub(thumb).max(1);
    let off = (row.saturating_sub(track_y) as usize).min(span);
    ((off as f32 / span as f32) * max_top as f32).round() as usize
}

/// Truncate `s` from the left (keeping the tail) to fit `w` columns.
fn elide_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w <= 1 {
        return "…".chars().take(w).collect();
    }
    let tail: String = s.chars().skip(n - (w - 1)).collect();
    format!("…{tail}")
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
    show_sidebar: bool,
    sidebar_width: u16,
    sidebar_scroll: usize, // top file row of the sidebar (independent of selection)
    sidebar_sel: usize,    // cursor row in the sidebar (a File or Thread row)
    resizing: bool,        // dragging the sidebar/diff divider
    focus: Focus,
    current_file: usize,          // the diff pane shows only this file
    sidebar_rows: Vec<SbRow>,     // file list grouped by directory
    file_to_sbrow: Vec<usize>,    // file_idx -> index into sidebar_rows
    sel_anchor: Option<usize>,    // drag-selection anchor (cursor = `selected`)
    pending_copy: Option<String>, // text to push to the clipboard next frame
    file_stats: Vec<(usize, usize)>,
    diff_area: Rect,        // last-drawn diff pane rect (for mouse hit-testing)
    sidebar_area: Rect,     // last-drawn sidebar rect (zero-sized when hidden)
    diff_sb: Rect,          // diff scrollbar track (zero-sized when none)
    sidebar_sb: Rect,       // sidebar scrollbar track (zero-sized when none)
    sb_drag: Option<Focus>, // which scrollbar is being dragged
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
        let stats = file_stats(&changeset);
        let (sidebar_rows, file_to_sbrow) = build_sidebar_rows(&changeset, &comments);
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
            status: "q quit  j/k move  spc/b page  g/G top/bot  [/] file  n/N comment  tab split"
                .into(),
            watch: None,
            needs_clear: false,
            show_sidebar: true,
            sidebar_width: SIDEBAR_WIDTH,
            sidebar_scroll: 0,
            sidebar_sel: file_to_sbrow.first().copied().unwrap_or(0),
            resizing: false,
            focus: Focus::Diff,
            current_file: 0,
            sidebar_rows,
            file_to_sbrow,
            sel_anchor: None,
            pending_copy: None,
            file_stats: stats,
            diff_area: Rect::default(),
            sidebar_area: Rect::default(),
            diff_sb: Rect::default(),
            sidebar_sb: Rect::default(),
            sb_drag: None,
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
        let (s, e) = self.file_range();
        (s..e).find(|&i| self.is_selectable_at(i))
    }

    fn last_selectable(&self) -> Option<usize> {
        let (s, e) = self.file_range();
        (s..e).rev().find(|&i| self.is_selectable_at(i))
    }

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        while !self.quit {
            if self.needs_clear {
                terminal.clear()?;
                self.needs_clear = false;
            }
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.on_key(key.code, key.modifiers);
                    }
                    Event::Mouse(me) => self.on_mouse(me),
                    _ => {}
                }
            }
            if let Some(text) = self.pending_copy.take() {
                // OSC 52: write the selection to the terminal clipboard.
                use std::io::Write;
                let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
                let mut out = std::io::stdout();
                let _ = out.write_all(seq.as_bytes());
                let _ = out.flush();
            }
            self.poll_reload();
        }
        Ok(())
    }

    /// Column of the draggable sidebar/diff divider, if the sidebar is shown.
    fn divider_col(&self) -> Option<u16> {
        (self.sidebar_area.width > 0).then(|| self.sidebar_area.x + self.sidebar_area.width - 1)
    }

    /// Resize the sidebar so its divider sits at column `col`.
    fn resize_to(&mut self, col: u16) {
        let total = self.sidebar_area.width + self.diff_area.width;
        let max = total.saturating_sub(MIN_DIFF).max(MIN_SIDEBAR);
        self.sidebar_width = (col.saturating_sub(self.sidebar_area.x) + 1).clamp(MIN_SIDEBAR, max);
    }

    /// Mouse: wheel scrolls the pane under the pointer; left-click selects;
    /// dragging the divider resizes the sidebar.
    fn on_mouse(&mut self, me: MouseEvent) {
        let (col, row) = (me.column, me.row);
        let on_divider = self.divider_col() == Some(col);
        let over_sidebar = self.sidebar_area.width > 0 && hit(self.sidebar_area, col, row);
        match me.kind {
            MouseEventKind::Up(_) => {
                self.resizing = false;
                self.sb_drag = None;
            }
            // Scrollbar thumb drag (start + continue).
            MouseEventKind::Down(MouseButton::Left) if hit(self.diff_sb, col, row) => {
                self.sb_drag = Some(Focus::Diff);
                self.drag_diff_sb(row);
            }
            MouseEventKind::Down(MouseButton::Left) if hit(self.sidebar_sb, col, row) => {
                self.sb_drag = Some(Focus::Sidebar);
                self.drag_sidebar_sb(row);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Diff) => {
                self.drag_diff_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Sidebar) => {
                self.drag_sidebar_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing => self.resize_to(col),
            MouseEventKind::Down(MouseButton::Left) if on_divider => self.resizing = true,
            // Wheel scrolls the pane under the pointer. Over the sidebar it
            // moves the list only — selection/focus are left untouched.
            MouseEventKind::ScrollDown => {
                if over_sidebar {
                    self.scroll_sidebar(3);
                } else {
                    self.focus = Focus::Diff;
                    self.scroll_view(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if over_sidebar {
                    self.scroll_sidebar(-3);
                } else {
                    self.focus = Focus::Diff;
                    self.scroll_view(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if over_sidebar {
                    self.click_sidebar(row);
                } else if hit(self.diff_area, col, row) {
                    self.click_diff(row, true);
                }
            }
            // Drag in the diff extends the line selection.
            MouseEventKind::Drag(MouseButton::Left) if hit(self.diff_area, col, row) => {
                self.click_diff(row, false);
            }
            _ => {}
        }
    }

    /// Whether row `idx` falls within the current selection (cursor or drag).
    fn in_selection(&self, idx: usize) -> bool {
        let anchor = self.sel_anchor.unwrap_or(self.selected);
        let (lo, hi) = (anchor.min(self.selected), anchor.max(self.selected));
        idx >= lo && idx <= hi
    }

    /// The code text of row `idx` (sign stripped / new side), if it's a line.
    fn line_text(&self, idx: usize) -> Option<String> {
        match self.view {
            View::Unified => match self.rows.get(idx)?.kind {
                RowKind::Line { .. } => {
                    let t = &self.rows[idx].text;
                    Some(t.get(1..).unwrap_or("").to_string())
                }
                _ => None,
            },
            View::Split => match &self.split_rows.get(idx)?.kind {
                SplitRowKind::Pair { left, right } => {
                    right.as_ref().or(left.as_ref()).map(|c| c.text.clone())
                }
                _ => None,
            },
        }
    }

    /// Copy the selected lines to the system clipboard (via OSC 52 next frame).
    fn copy_selection(&mut self) {
        let anchor = self.sel_anchor.unwrap_or(self.selected);
        let (lo, hi) = (anchor.min(self.selected), anchor.max(self.selected));
        let lines: Vec<String> = (lo..=hi).filter_map(|i| self.line_text(i)).collect();
        if lines.is_empty() {
            return;
        }
        self.status = format!("copied {} line(s)", lines.len());
        self.pending_copy = Some(lines.join("\n"));
    }

    /// Map a scrollbar drag at terminal `row` to a scroll position.
    fn drag_diff_sb(&mut self, row: u16) {
        let (start, end) = self.file_range();
        let total = end - start;
        let pos = sb_thumb_pos(
            self.diff_sb.y,
            self.diff_sb.height as usize,
            total,
            self.height,
            row,
        );
        self.scroll = start + pos;
    }

    fn drag_sidebar_sb(&mut self, row: u16) {
        let h = self.sidebar_sb.height as usize;
        self.sidebar_scroll =
            sb_thumb_pos(self.sidebar_sb.y, h, self.sidebar_rows.len(), h, row);
    }

    /// Scroll the file list independently of the selection.
    fn scroll_sidebar(&mut self, delta: isize) {
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        self.sidebar_scroll =
            (self.sidebar_scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Scroll the list so the sidebar cursor row is visible.
    fn reveal_sidebar(&mut self) {
        let h = self.sidebar_area.height as usize;
        if h == 0 {
            return;
        }
        let r = self.sidebar_sel.min(self.sidebar_rows.len().saturating_sub(1));
        // Include the row just above (dir header / parent file) when present.
        let target = r.saturating_sub(1);
        if target < self.sidebar_scroll {
            self.sidebar_scroll = target;
        } else if r >= self.sidebar_scroll + h {
            self.sidebar_scroll = r + 1 - h;
        }
    }

    /// Is the sidebar row at `idx` a landing spot for the cursor (file/thread)?
    fn sb_selectable(&self, idx: usize) -> bool {
        matches!(
            self.sidebar_rows.get(idx),
            Some(SbRow::File(_) | SbRow::Thread(_))
        )
    }

    /// Move the sidebar cursor to the next/prev selectable row and act on it.
    fn move_sidebar(&mut self, dir: isize) {
        let n = self.sidebar_rows.len();
        let mut i = self.sidebar_sel as isize;
        loop {
            i += dir;
            if i < 0 || i as usize >= n {
                return;
            }
            if self.sb_selectable(i as usize) {
                self.sidebar_sel = i as usize;
                self.activate_sidebar();
                return;
            }
        }
    }

    /// Jump the sidebar cursor to the first/last selectable row.
    fn sidebar_edge(&mut self, last: bool) {
        let n = self.sidebar_rows.len();
        let found = if last {
            (0..n).rev().find(|&i| self.sb_selectable(i))
        } else {
            (0..n).find(|&i| self.sb_selectable(i))
        };
        if let Some(i) = found {
            self.sidebar_sel = i;
            self.activate_sidebar();
        }
    }

    /// Apply the row under the sidebar cursor: switch file or jump to thread.
    fn activate_sidebar(&mut self) {
        match self.sidebar_rows.get(self.sidebar_sel) {
            Some(SbRow::File(fi)) => {
                let fi = *fi;
                if fi != self.current_file {
                    self.set_current_file(fi);
                }
                self.reveal_sidebar();
            }
            Some(SbRow::Thread(ti)) => {
                let ti = *ti;
                self.goto_thread(ti, false);
            }
            _ => {}
        }
    }

    fn click_sidebar(&mut self, row: u16) {
        let off = row.saturating_sub(self.sidebar_area.y) as usize;
        let idx = self.sidebar_scroll + off;
        match self.sidebar_rows.get(idx) {
            Some(SbRow::File(fi)) => {
                let fi = *fi;
                self.focus = Focus::Sidebar;
                self.set_current_file(fi);
            }
            Some(SbRow::Thread(ti)) => {
                let ti = *ti;
                self.sidebar_sel = idx;
                self.goto_thread(ti, true);
            }
            _ => {}
        }
    }

    /// Jump the diff pane straight to comment thread `ti` and select its line.
    /// `focus_diff` moves keyboard focus to the diff (used for mouse clicks).
    fn goto_thread(&mut self, ti: usize, focus_diff: bool) {
        let Some(t) = self.comments.threads.get(ti) else {
            return;
        };
        let (file, side, range) = (t.file.clone(), t.side, t.range);
        let Some(fi) = self
            .changeset
            .files
            .iter()
            .position(|f| Path::new(f.display_path()) == file)
        else {
            return;
        };
        self.set_current_file(fi);
        let target = (0..self.active_len()).find(|&i| {
            self.is_selectable_at(i)
                && matches!(self.anchor_at(i), Some((f, s, l)) if f == fi && s == side && range.contains(l))
        });
        if let Some(i) = target {
            self.selected = i;
            self.scroll = i.saturating_sub(self.height / 2);
            self.ensure_visible();
        }
        // Point the sidebar cursor at this thread's row and keep it on screen.
        if let Some(r) = self
            .sidebar_rows
            .iter()
            .position(|row| matches!(row, SbRow::Thread(t) if *t == ti))
        {
            self.sidebar_sel = r;
            self.reveal_sidebar();
        }
        if focus_diff {
            self.focus = Focus::Diff;
        }
    }

    /// Place the cursor at the clicked diff row. `anchor` starts a new
    /// selection there; otherwise the existing anchor is kept (drag extend).
    fn click_diff(&mut self, row: u16, anchor: bool) {
        self.focus = Focus::Diff;
        let (start, end) = self.file_range();
        let top = self.scroll.max(start);
        let idx = (top + row.saturating_sub(self.diff_area.y) as usize)
            .clamp(start, end.saturating_sub(1).max(start));
        let target = if self.is_selectable_at(idx) {
            Some(idx)
        } else {
            self.nearest_selectable(idx, 1)
                .or_else(|| self.nearest_selectable(idx, -1))
        };
        if let Some(i) = target {
            self.selected = i;
            if anchor {
                self.sel_anchor = Some(i);
            }
            self.ensure_visible();
        }
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
                    self.file_stats = file_stats(&self.changeset);
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
        // Rebuild the sidebar once both the diff and comments are current.
        let (sr, map) = build_sidebar_rows(&self.changeset, &self.comments);
        self.sidebar_rows = sr;
        self.file_to_sbrow = map;
        // Files may have changed; re-point at a valid file and selectable row.
        self.set_current_file(self.current_file);
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

    /// The file index a row belongs to (header rows included).
    fn row_file_idx(&self, i: usize) -> Option<usize> {
        match self.view {
            View::Unified => self.rows.get(i).map(|r| r.file_idx),
            View::Split => self.split_rows.get(i).map(|r| r.file_idx),
        }
    }

    /// `[start, end)` row range of the current file in the active list. Files
    /// are contiguous, so this is a single slice. Empty `(len, len)` if absent.
    fn file_range(&self) -> (usize, usize) {
        let len = self.active_len();
        let (mut start, mut end) = (len, len);
        for i in 0..len {
            if self.row_file_idx(i) == Some(self.current_file) {
                if start == len {
                    start = i;
                }
                end = i + 1;
            }
        }
        (start, end)
    }

    /// Switch the diff pane to the next/prev file.
    fn jump_file(&mut self, dir: isize) {
        let n = self.changeset.files.len();
        if n == 0 {
            return;
        }
        let target = (self.current_file as isize + dir).clamp(0, n as isize - 1) as usize;
        if target == self.current_file {
            return;
        }
        self.set_current_file(target);
    }

    /// Point the diff pane at `file`, resetting the cursor to its top.
    fn set_current_file(&mut self, file: usize) {
        self.sel_anchor = None;
        self.current_file = file.min(self.changeset.files.len().saturating_sub(1));
        self.sidebar_sel = self
            .file_to_sbrow
            .get(self.current_file)
            .copied()
            .unwrap_or(0);
        self.reveal_sidebar();
        let (start, _) = self.file_range();
        self.scroll = start;
        self.selected = self.first_selectable().unwrap_or(start);
        self.ensure_visible();
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
        self.sel_anchor = None;
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
        // Stay on the same file across the layout switch.
        self.current_file = self
            .row_file_idx(self.selected)
            .unwrap_or(self.current_file);
        // Recenter so the cursor is roughly mid-viewport (clamped to the file).
        self.scroll = self.selected.saturating_sub(self.height / 2);
        self.ensure_visible();
        self.status = match self.view {
            View::Unified => "unified view".into(),
            View::Split => "split view".into(),
        };
    }

    /// Is the file sidebar an actual pane the user can focus?
    fn sidebar_available(&self) -> bool {
        self.show_sidebar && self.changeset.files.len() > 1
    }

    /// Focus clamped to reality (never Sidebar when there's no sidebar).
    fn effective_focus(&self) -> Focus {
        if self.sidebar_available() {
            self.focus
        } else {
            Focus::Diff
        }
    }

    /// Selection background for the diff pane (dim when it isn't focused).
    fn diff_cursor_bg(&self) -> Color {
        if self.effective_focus() == Focus::Diff {
            FOCUS_BG
        } else {
            UNFOCUS_BG
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        // Global keys, independent of the focused pane.
        match code {
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Left => {
                if self.sidebar_available() {
                    self.focus = Focus::Sidebar;
                }
                return;
            }
            KeyCode::Right => return self.focus = Focus::Diff,
            KeyCode::Tab | KeyCode::Char('s') => return self.toggle_view(),
            KeyCode::Char('b') if ctrl => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar {
                    self.focus = Focus::Diff;
                }
                return;
            }
            KeyCode::Char('l') if ctrl => return self.needs_clear = true,
            _ => {}
        }
        match self.effective_focus() {
            Focus::Sidebar => self.on_key_sidebar(code),
            Focus::Diff => self.on_key_diff(code, ctrl),
        }
    }

    /// Navigation when the file sidebar is focused: move by row (files and the
    /// comment threads nested under them).
    fn on_key_sidebar(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_sidebar(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sidebar(-1),
            KeyCode::Char('g') | KeyCode::Home => self.sidebar_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.sidebar_edge(true),
            // Enter the diff with the current row selected.
            KeyCode::Enter | KeyCode::Char('l') => self.focus = Focus::Diff,
            _ => {}
        }
    }

    /// Navigation when the diff pane is focused.
    fn on_key_diff(&mut self, code: KeyCode, ctrl: bool) {
        let page = self.height.max(1);
        let half = (self.height / 2).max(1);
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1, 1),

            // Half-page: Ctrl-D / Ctrl-U (vim/less).
            KeyCode::Char('d') if ctrl => self.move_by(1, half),
            KeyCode::Char('u') if ctrl => self.move_by(-1, half),

            // Full page: Space / Ctrl-F / PageDown forward, b / PageUp back.
            KeyCode::Char(' ') | KeyCode::Char('f') | KeyCode::PageDown => self.move_by(1, page),
            KeyCode::Char('b') | KeyCode::PageUp => self.move_by(-1, page),

            // One-line viewport scroll, cursor stays in view: Ctrl-E / Ctrl-Y (less/vim).
            KeyCode::Char('e') if ctrl => self.scroll_view(1),
            KeyCode::Char('y') if ctrl => self.scroll_view(-1),

            // Top / bottom.
            KeyCode::Char('g') | KeyCode::Home => {
                self.sel_anchor = None;
                self.selected = self.first_selectable().unwrap_or(0);
                self.ensure_visible();
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.sel_anchor = None;
                self.selected = self.last_selectable().unwrap_or(0);
                self.ensure_visible();
            }

            // Jump between comment threads.
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),

            // Jump between files.
            KeyCode::Char(']') => self.jump_file(1),
            KeyCode::Char('[') => self.jump_file(-1),

            // Copy the selected line(s); Esc clears a drag selection.
            KeyCode::Char('y') => self.copy_selection(),
            KeyCode::Esc => self.sel_anchor = None,
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.sel_anchor = None;
        let (start, end) = self.file_range();
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < start as isize || i as usize >= end {
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
        let (start, end) = self.file_range();
        let max_top = end.saturating_sub(1).max(start) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(start as isize, max_top) as usize;
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

    /// First selectable row at/beyond `from` scanning in `dir`, within the file.
    fn nearest_selectable(&self, from: usize, dir: isize) -> Option<usize> {
        let (start, end) = self.file_range();
        let mut i = from as isize;
        while i >= start as isize && (i as usize) < end {
            if self.is_selectable_at(i as usize) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    fn ensure_visible(&mut self) {
        let (start, end) = self.file_range();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.height {
            self.scroll = self.selected + 1 - self.height;
        }
        // Never scroll outside the current file's slice.
        self.scroll = self.scroll.clamp(start, end.saturating_sub(1).max(start));
    }

    fn selected_anchor(&self) -> Option<(PathBuf, Side, u32)> {
        let (file_idx, side, line) = self.anchor_at(self.selected)?;
        let file = self.changeset.files.get(file_idx)?;
        Some((PathBuf::from(file.display_path()), side, line))
    }

    fn jump_comment(&mut self, dir: isize) {
        self.sel_anchor = None;
        // Collect rows in the current file that carry a thread anchor.
        let (start, end) = self.file_range();
        let mut targets: Vec<usize> = Vec::new();
        for i in start..end {
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

        // Body: optional file sidebar on the left, diff on the right.
        let body = chunks[1];
        let sidebar = self.show_sidebar && self.changeset.files.len() > 1 && body.width >= 60;
        let (diff_area, sidebar_area) = if sidebar {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(self.sidebar_width), Constraint::Min(1)])
                .split(body);
            self.render_sidebar(f, cols[0]);
            (cols[1], cols[0])
        } else {
            (body, Rect::default())
        };
        self.sidebar_area = sidebar_area;
        self.sidebar_sb =
            if sidebar_area.width > 0 && self.sidebar_rows.len() > sidebar_area.height as usize {
                Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(2),
                    y: sidebar_area.y,
                    width: 1,
                    height: sidebar_area.height,
                }
            } else {
                Rect::default()
            };
        self.height = diff_area.height as usize;
        // Reserve the rightmost column for a scrollbar when the file overflows.
        let (fr_start, fr_end) = self.file_range();
        let overflow = fr_end - fr_start > self.height;
        let content = if overflow {
            Rect {
                width: diff_area.width.saturating_sub(1),
                ..diff_area
            }
        } else {
            diff_area
        };
        self.diff_area = content;
        self.diff_sb = if overflow {
            Rect {
                x: diff_area.x + diff_area.width.saturating_sub(1),
                y: diff_area.y,
                width: 1,
                height: diff_area.height,
            }
        } else {
            Rect::default()
        };
        self.render_diff(f, content);
        if overflow {
            self.render_diff_scrollbar(f, diff_area);
        }

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

    /// Left-hand file list: path + (+adds / -dels), current file highlighted.
    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let focused = self.effective_focus() == Focus::Sidebar;
        let border = if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(border));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let h = inner.height as usize;
        let n = self.sidebar_rows.len();
        // Reserve a column for the scrollbar when the list overflows.
        let need_sb = n > h;
        let w = (inner.width as usize).saturating_sub(if need_sb { 1 } else { 0 });
        let max = n.saturating_sub(h);
        let scroll = self.sidebar_scroll.min(max);

        // The thread the diff cursor currently sits on (for sidebar highlight).
        let cur_anchor = self.anchor_at(self.selected);
        let mut lines: Vec<Line> = Vec::new();
        for idx in scroll..n.min(scroll + h) {
            match &self.sidebar_rows[idx] {
                SbRow::Dir(dir) => {
                    let text = format!("{}/", elide_left(dir, w.saturating_sub(1)));
                    lines.push(Line::from(Span::styled(
                        text,
                        Style::default()
                            .fg(Color::Rgb(106, 115, 130))
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                SbRow::File(fi) => {
                    let fi = *fi;
                    let is_cur = fi == self.current_file;
                    let (adds, dels) = self.file_stats.get(fi).copied().unwrap_or((0, 0));
                    let counts = format!(" +{adds} -{dels}");
                    let prefix = "  ";
                    let avail = w.saturating_sub(prefix.chars().count() + counts.chars().count());
                    let base = self
                        .changeset
                        .files
                        .get(fi)
                        .map(|f| base_of(f.display_path()))
                        .unwrap_or_default();
                    let name = format!("{:<width$}", elide_left(base, avail), width = avail);
                    let is_cursor = focused && idx == self.sidebar_sel;
                    let bg = if is_cursor {
                        Some(FOCUS_BG)
                    } else if is_cur {
                        Some(UNFOCUS_BG)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    let name_style = if is_cur {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, wbg(Style::default().fg(Color::Cyan))),
                        Span::styled(name, wbg(name_style)),
                        Span::styled(format!(" +{adds}"), wbg(Style::default().fg(Color::Green))),
                        Span::styled(format!(" -{dels}"), wbg(Style::default().fg(Color::Red))),
                    ]));
                }
                SbRow::Thread(ti) => {
                    let Some(t) = self.comments.threads.get(*ti) else {
                        continue;
                    };
                    let glyph = if t.resolved { "○ " } else { "● " };
                    let snippet = t
                        .comments
                        .first()
                        .map(|c| c.body.as_str())
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim();
                    let indent = "    ";
                    let avail =
                        w.saturating_sub(indent.chars().count() + glyph.chars().count());
                    let text = format!("{:<width$}", elide_right(snippet, avail), width = avail);
                    // Highlight when the diff cursor sits on a line this thread anchors.
                    let active = cur_anchor
                        .map(|(f, s, l)| {
                            self.changeset
                                .files
                                .get(f)
                                .map(|cf| Path::new(cf.display_path()) == t.file)
                                .unwrap_or(false)
                                && s == t.side
                                && t.range.contains(l)
                        })
                        .unwrap_or(false);
                    let is_cursor = focused && idx == self.sidebar_sel;
                    let bg = if is_cursor {
                        Some(FOCUS_BG)
                    } else if active {
                        Some(UNFOCUS_BG)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    let glyph_color = if t.resolved { Color::DarkGray } else { Color::Yellow };
                    let text_color = if t.resolved { Color::DarkGray } else { Color::Gray };
                    lines.push(Line::from(vec![
                        Span::styled(indent, wbg(Style::default())),
                        Span::styled(glyph, wbg(Style::default().fg(glyph_color))),
                        Span::styled(text, wbg(Style::default().fg(text_color))),
                    ]));
                }
            }
        }
        f.render_widget(Paragraph::new(lines), inner);
        if need_sb {
            let mut sb = ScrollbarState::new(max + 1)
                .position(scroll)
                .viewport_content_length(h);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                inner,
                &mut sb,
            );
        }
    }

    /// A vertical scrollbar on the right edge of `area` for the diff pane.
    fn render_diff_scrollbar(&self, f: &mut Frame, area: Rect) {
        let (start, end) = self.file_range();
        let total = end - start;
        if total <= self.height {
            return;
        }
        let max_top = total - self.height;
        let pos = self.scroll.saturating_sub(start).min(max_top);
        let mut sb = ScrollbarState::new(max_top + 1)
            .position(pos)
            .viewport_content_length(self.height);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut sb,
        );
    }

    fn render_unified(&self, f: &mut Frame, area: Rect) {
        let width = area.width as usize;
        let (start, file_end) = self.file_range();
        let top = self.scroll.max(start);
        let end = (top + self.height).min(file_end);
        let mut lines: Vec<Line> = Vec::new();
        for idx in top..end {
            let row = &self.rows[idx];
            let selected = self.in_selection(idx);
            lines.push(self.row_to_line(row, selected, width));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn render_split(&self, f: &mut Frame, area: Rect) {
        let total = area.width as usize;
        let divider = " │ ";
        let side_w = total.saturating_sub(divider.len()) / 2;
        let (start, file_end) = self.file_range();
        let top = self.scroll.max(start);
        let end = (top + self.height).min(file_end);
        let mut lines: Vec<Line> = Vec::new();
        for idx in top..end {
            let row = &self.split_rows[idx];
            let selected = self.in_selection(idx);
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
                Style::default()
                    .fg(Color::Rgb(106, 115, 130))
                    .add_modifier(Modifier::ITALIC),
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
                    Some(self.diff_cursor_bg())
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
                Style::default()
                    .fg(Color::Rgb(106, 115, 130))
                    .add_modifier(Modifier::ITALIC),
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
                    Some(self.diff_cursor_bg())
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
