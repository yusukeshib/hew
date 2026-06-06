//! TUI application state and render loop.

use crate::comments::model::{CommentStore, LineRange};
use crate::diff::model::{Changeset, LineKind, Side};
use crate::ui::highlight_cache::HighlightCache;
use crate::ui::render_rows::{
    build_rows, build_split_rows, char_width, str_width, take_width, CommentKind, CommentLine,
    ComposerAnchor, ComposerKind, ComposerLine, ComposerSpec, Row, RowKind, SideCell, SplitRow,
    SplitRowKind,
};
use crate::ui::sidebar::{
    base_of, build_sidebar_rows, dir_of, file_comment_state, file_status, SbRow,
};
use crate::ui::theme::theme;
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;
use uuid::Uuid;

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

const SIDEBAR_WIDTH: u16 = 38;
const MIN_SIDEBAR: u16 = 14;
const MIN_DIFF: u16 = 20;
/// The column separator drawn between the two halves of the split view. Shared
/// by `render_split` and `sync_comment_wrap` so the comment-wrap math can't
/// desync from the rendered layout if the literal ever changes.
const SPLIT_DIVIDER: &str = " │ ";

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

/// Right-pad `s` with spaces so its display width is exactly `w` (no-op when it
/// already meets/exceeds `w`). Use after `elide_left`/`take_width` so a wide
/// glyph can't push the column past `w`.
fn pad_width(s: &str, w: usize) -> String {
    let sw = str_width(s);
    if sw >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - sw))
    }
}

/// Truncate `s` from the left (keeping the tail) to fit `w` display columns,
/// prefixing an ellipsis when it doesn't fit.
fn elide_left(s: &str, w: usize) -> String {
    if str_width(s) <= w {
        return s.to_string();
    }
    if w <= 1 {
        return "…".repeat(w);
    }
    // Keep the widest tail that fits in `w - 1` cells (room for the ellipsis).
    let budget = w - 1;
    let mut tail: std::collections::VecDeque<char> = std::collections::VecDeque::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let cw = char_width(c);
        if used + cw > budget {
            break;
        }
        tail.push_front(c);
        used += cw;
    }
    let tail: String = tail.into_iter().collect();
    format!("…{tail}")
}

/// A view-independent handle to the selected row, stable across row rebuilds
/// and unified/split switches (raw indices are not).
enum SelKey {
    /// A diff line, keyed by its comment anchor `(file, side, line)`.
    Line(usize, Side, u32),
    /// A comment message, keyed by its stable id.
    Comment(Uuid),
}

/// What an open composer will write on submit.
enum ComposeTarget {
    /// A brand-new thread anchored to a diff line range (`start == end` for a
    /// single line; a wider span comes from visual line-select mode).
    NewThread {
        file_idx: usize,
        side: Side,
        start: u32,
        end: u32,
    },
    /// A reply appended to an existing thread.
    Reply { thread_id: Uuid },
}

/// In-progress comment text and where it lands. The text + cursor live in a
/// [`TextArea`], used purely as an edit model (readline/emacs keybindings,
/// multi-line cursor, undo) — the box is drawn inline in the diff row stream,
/// not via tui-textarea's own widget.
struct Composer {
    target: ComposeTarget,
    textarea: TextArea<'static>,
}

/// Caret glyph drawn at the composer's cursor (a printable block, so it
/// survives `sanitize_line`).
const COMPOSER_CARET: char = '\u{2588}';

/// The composer body as a single string with the caret glyph spliced in at the
/// cursor position, ready for the row builder to wrap. A `TextArea` always has
/// at least one line, so the cursor row is always valid.
///
/// Built in one pass over `ta.lines()` (pushing line slices, not cloning each
/// line into a `Vec<String>`): this runs on every row rebuild — i.e. per
/// keystroke — so the per-line clone + join is worth avoiding.
fn body_with_caret(ta: &TextArea<'static>) -> String {
    let (row, col) = ta.cursor();
    let lines = ta.lines();
    let cap =
        lines.iter().map(|l| l.len()).sum::<usize>() + lines.len() + COMPOSER_CARET.len_utf8();
    let mut out = String::with_capacity(cap);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if i == row {
            // `col` is a char index; translate to a byte offset to splice.
            let byte = line
                .char_indices()
                .nth(col)
                .map(|(b, _)| b)
                .unwrap_or(line.len());
            out.push_str(&line[..byte]);
            out.push(COMPOSER_CARET);
            out.push_str(&line[byte..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Diff/review TUI state. Comments are loaded from a sidecar (immutable),
/// displayed and navigated inline, and mutated in place
/// (compose/reply/resolve/delete); on exit the final store is diffed against
/// the base into an action log.
pub struct App {
    changeset: Arc<Changeset>,
    rows: Vec<Row>,
    comments: CommentStore,
    /// Comment ids present at load (from the input sidecar). These are
    /// immutable: `D` deletes a single comment, and only ones added in-session,
    /// so an input comment is never removed and the action log never emits a
    /// delete (an in-session add+delete cancels out of the diff).
    base_comment_ids: HashSet<Uuid>,
    split_rows: Vec<SplitRow>,
    view: View,
    selected: usize, // index into the active row list
    scroll: usize,   // top row of viewport
    height: usize,   // last known viewport height
    status: String,
    needs_clear: bool,
    show_sidebar: bool,
    sidebar_width: u16,
    sidebar_scroll: usize, // top file row of the sidebar (independent of selection)
    sidebar_sel: usize,    // cursor row in the sidebar (a Dir or File row)
    collapsed: HashSet<String>, // directory paths collapsed in the sidebar tree
    comment_wrap: usize,   // width used to wrap inline comment bodies
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
    /// Syntax-highlight cache + background warm worker (see [`HighlightCache`]).
    hl: HighlightCache,
    /// Cached `[start, end)` row span of `current_file` (see `file_range`).
    file_span: (usize, usize),
    /// `[start, end)` row span of every file in the *active* row list, indexed
    /// by file_idx. Rebuilt in one O(rows) pass whenever the row list or view
    /// changes, so a file switch is an O(1) lookup instead of a full scan.
    file_spans: Vec<(usize, usize)>,
    composer: Option<Composer>,
    /// Visual line-select mode: movement keeps `sel_anchor` so the user can
    /// grow a multi-line selection (then `i` anchors a comment to the range).
    visual: bool,
    quit: bool,
}

impl App {
    /// Consume the app and return the final in-memory review store, so the
    /// caller can diff it against the immutable base to produce the action log.
    pub fn into_comments(self) -> CommentStore {
        self.comments
    }

    /// Construct with a pre-loaded comment store (e.g. from a sidecar JSON).
    pub fn with_comments(changeset: Changeset, comments: CommentStore) -> Self {
        // Comment threads always render expanded inline.
        let rows = build_rows(&changeset, &comments, 0, None);
        let split_rows = build_split_rows(&changeset, &comments, 0, None);
        // Snapshot every loaded comment id: these came from the input sidecar
        // and are protected from deletion (only in-session comments can be
        // removed).
        let base_comment_ids: HashSet<Uuid> = comments
            .threads
            .iter()
            .flat_map(|t| t.comments.iter().map(|c| c.id))
            .collect();
        let stats = file_stats(&changeset);
        let collapsed = HashSet::new();
        let (sidebar_rows, file_to_sbrow) = build_sidebar_rows(&changeset, &collapsed);
        let changeset = Arc::new(changeset);
        let hl = HighlightCache::new(changeset.clone());
        let mut app = App {
            changeset,
            rows,
            split_rows,
            view: View::Split,
            comments,
            base_comment_ids,
            selected: 0,
            scroll: 0,
            height: 1,
            status: "q quit  j/k move  i comment  r reply  R resolve  D delete".into(),
            needs_clear: false,
            show_sidebar: true,
            sidebar_width: SIDEBAR_WIDTH,
            sidebar_scroll: 0,
            sidebar_sel: file_to_sbrow
                .iter()
                .copied()
                .find(|&r| r != usize::MAX)
                .unwrap_or(0),
            collapsed,
            comment_wrap: 0,
            resizing: false,
            focus: Focus::Sidebar,
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
            hl,
            file_span: (0, 0),
            file_spans: Vec::new(),
            composer: None,
            visual: false,
            quit: false,
        };
        app.rebuild_file_spans();
        app.recompute_file_span();
        app.selected = app.first_selectable().unwrap_or(0);
        app
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
        let hl = self.hl.runs(file_idx, text);
        let mut out = Vec::new();
        let mut used = 0usize;
        for (c, s) in hl.iter() {
            if used >= width {
                break;
            }
            let (take, tw) = take_width(s, width - used);
            if take.is_empty() {
                continue;
            }
            used += tw;
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

    fn first_selectable(&self) -> Option<usize> {
        let (s, e) = self.file_range();
        (s..e).find(|&i| self.is_stop_at(i))
    }

    fn last_selectable(&self) -> Option<usize> {
        let (s, e) = self.file_range();
        (s..e).rev().find(|&i| self.is_stop_at(i))
    }

    pub fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        while !self.quit {
            if self.needs_clear {
                terminal.clear()?;
                self.needs_clear = false;
            }
            terminal.draw(|f| self.draw(f))?;
            // Block for the first event, then drain every event already queued
            // before redrawing. A burst of mouse-drag events thus collapses
            // into a single frame instead of one render per event, which is
            // what made divider drags lag.
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    self.on_key(key.code, key.modifiers);
                }
                Event::Mouse(me) => self.on_mouse(me),
                Event::Paste(text) => self.on_paste(text),
                _ => {}
            }
            while event::poll(Duration::from_millis(0))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.on_key(key.code, key.modifiers);
                    }
                    Event::Mouse(me) => self.on_mouse(me),
                    Event::Paste(text) => self.on_paste(text),
                    _ => {}
                }
            }
            if let Some(text) = self.pending_copy.take() {
                // OSC 52: write the selection to the terminal clipboard.
                // Target stderr, where the TUI renders; stdout is the JSON
                // result channel and may be redirected to a file.
                use std::io::Write;
                let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
                let mut out = std::io::stderr();
                let _ = out.write_all(seq.as_bytes());
                let _ = out.flush();
            }
        }
        Ok(())
    }

    /// Column of the draggable sidebar/diff divider, if the sidebar is shown.
    fn divider_col(&self) -> Option<u16> {
        // The diff panel's left border (just past the sidebar) is the divider.
        (self.sidebar_area.width > 0).then(|| self.sidebar_area.x + self.sidebar_area.width)
    }

    /// Resize the sidebar so its divider sits at column `col`.
    fn resize_to(&mut self, col: u16) {
        let total = self.sidebar_area.width + self.diff_area.width;
        let max = total.saturating_sub(MIN_DIFF).max(MIN_SIDEBAR);
        self.sidebar_width = col
            .saturating_sub(self.sidebar_area.x)
            .clamp(MIN_SIDEBAR, max);
    }

    /// Mouse: wheel scrolls the pane under the pointer; left-click selects;
    /// dragging the divider resizes the sidebar.
    fn on_mouse(&mut self, me: MouseEvent) {
        // The composer modal swallows mouse input too.
        if self.composer.is_some() {
            return;
        }
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
            // The pane border (rightmost column) is the resize divider.
            MouseEventKind::Down(MouseButton::Left) if on_divider => self.resizing = true,
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Diff) => {
                self.drag_diff_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Sidebar) => {
                self.drag_sidebar_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing => self.resize_to(col),
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

    /// Inclusive `[lo, hi]` row span of the current selection: the cursor line
    /// alone, or the cursor-to-anchor range when a drag/visual selection is
    /// active. The single source of truth for selection extent.
    fn selection_bounds(&self) -> (usize, usize) {
        let anchor = self.sel_anchor.unwrap_or(self.selected);
        (anchor.min(self.selected), anchor.max(self.selected))
    }

    /// Whether row `idx` falls within the current selection. When the cursor is
    /// on a comment, the whole message (its contiguous rows) is the selection;
    /// otherwise it's the diff-line cursor/drag range.
    fn in_selection(&self, idx: usize) -> bool {
        if let Some((lo, hi)) = self.comment_unit_span(self.selected) {
            return idx >= lo && idx <= hi;
        }
        let (lo, hi) = self.selection_bounds();
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

    /// Copy the selection to the system clipboard (via OSC 52 next frame): the
    /// focused comment's body when a comment is selected, else the diff lines.
    fn copy_selection(&mut self) {
        if let Some((thread_id, comment_id)) = self.focused_comment() {
            if let Some(body) = self
                .comments
                .threads
                .iter()
                .find(|t| t.id == thread_id)
                .and_then(|t| {
                    t.comments
                        .iter()
                        .find(|c| c.id == comment_id)
                        .map(|c| c.body.clone())
                })
            {
                self.status = "copied comment".into();
                self.pending_copy = Some(body);
            }
            return;
        }
        let (lo, hi) = self.selection_bounds();
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
        self.sidebar_scroll = sb_thumb_pos(self.sidebar_sb.y, h, self.sidebar_rows.len(), h, row);
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
        let r = self
            .sidebar_sel
            .min(self.sidebar_rows.len().saturating_sub(1));
        // Include the row just above (dir header / parent file) when present.
        let target = r.saturating_sub(1);
        if target < self.sidebar_scroll {
            self.sidebar_scroll = target;
        } else if r >= self.sidebar_scroll + h {
            self.sidebar_scroll = r + 1 - h;
        }
    }

    /// Rebuild the sidebar tree from the current collapse set.
    fn rebuild_sidebar(&mut self) {
        let (sr, map) = build_sidebar_rows(&self.changeset, &self.collapsed);
        self.sidebar_rows = sr;
        self.file_to_sbrow = map;
        // The row count shrank/grew; keep the scroll within bounds so clicks and
        // rendering agree.
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        self.sidebar_scroll = self.sidebar_scroll.min(max);
    }

    /// Expand every ancestor directory of `fi` so its row is visible.
    fn reveal_file_in_tree(&mut self, fi: usize) {
        let Some(f) = self.changeset.files.get(fi) else {
            return;
        };
        let dir = dir_of(f.display_path());
        if dir.is_empty() {
            return;
        }
        let segs: Vec<&str> = dir.split('/').collect();
        let mut changed = false;
        for d in 0..segs.len() {
            if self.collapsed.remove(&segs[..=d].join("/")) {
                changed = true;
            }
        }
        if changed {
            self.rebuild_sidebar();
        }
    }

    /// Open or close directory `path`, keeping the cursor on its row.
    fn set_dir_collapsed(&mut self, path: String, collapsed: bool) {
        let changed = if collapsed {
            self.collapsed.insert(path.clone())
        } else {
            self.collapsed.remove(&path)
        };
        if !changed {
            return;
        }
        self.rebuild_sidebar();
        if let Some(r) = self
            .sidebar_rows
            .iter()
            .position(|row| matches!(row, SbRow::Dir { path: p, .. } if *p == path))
        {
            self.sidebar_sel = r;
        }
        self.sidebar_sel = self
            .sidebar_sel
            .min(self.sidebar_rows.len().saturating_sub(1));
        self.reveal_sidebar();
    }

    /// Toggle the directory under the cursor (no-op on file rows).
    fn toggle_dir(&mut self, path: String) {
        let collapsed = self.collapsed.contains(&path);
        self.set_dir_collapsed(path, !collapsed);
    }

    /// Collapse (`collapse = true`) or expand the directory under the cursor.
    /// Collapsing while on a file row closes its containing folder and
    /// moves the cursor onto that folder.
    fn fold_dir(&mut self, collapse: bool) {
        match self.sidebar_rows.get(self.sidebar_sel) {
            Some(SbRow::Dir { path, .. }) => {
                let path = path.clone();
                // ← on an already-closed folder closes its container instead.
                if collapse && self.collapsed.contains(&path) {
                    let parent = dir_of(&path);
                    if !parent.is_empty() {
                        self.set_dir_collapsed(parent.to_string(), true);
                    }
                } else {
                    self.set_dir_collapsed(path, collapse);
                }
            }
            Some(SbRow::File { idx, .. }) if collapse => {
                let fi = *idx;
                if let Some(parent) = self.parent_dir_of_file(fi) {
                    self.set_dir_collapsed(parent, true);
                }
            }
            _ => {}
        }
    }

    /// The immediate containing directory of file `fi`, if it lives in one.
    fn parent_dir_of_file(&self, fi: usize) -> Option<String> {
        let f = self.changeset.files.get(fi)?;
        let dir = dir_of(f.display_path());
        (!dir.is_empty()).then(|| dir.to_string())
    }

    /// Toggle the directory under the cursor open/closed.
    fn fold_dir_toggle(&mut self) {
        if let Some(SbRow::Dir { path, .. }) = self.sidebar_rows.get(self.sidebar_sel) {
            let path = path.clone();
            self.toggle_dir(path);
        }
    }

    /// Move the sidebar cursor to the next/prev row and act on it. Every tree
    /// row (dir, file) is a valid landing spot, so this is a simple clamped step.
    fn move_sidebar(&mut self, dir: isize) {
        let n = self.sidebar_rows.len();
        let next = self.sidebar_sel as isize + dir;
        if next < 0 || next as usize >= n {
            return;
        }
        self.sidebar_sel = next as usize;
        self.activate_sidebar();
    }

    /// Jump the sidebar cursor to the first/last row.
    fn sidebar_edge(&mut self, last: bool) {
        let n = self.sidebar_rows.len();
        if n == 0 {
            return;
        }
        self.sidebar_sel = if last { n - 1 } else { 0 };
        self.activate_sidebar();
    }

    /// Apply the row under the sidebar cursor: switch to the file it names.
    fn activate_sidebar(&mut self) {
        // Directory rows are just a cursor resting spot during navigation;
        // they toggle only on explicit activation.
        if let Some(SbRow::File { idx, .. }) = self.sidebar_rows.get(self.sidebar_sel) {
            let fi = *idx;
            if fi != self.current_file {
                self.set_current_file(fi);
            }
            self.reveal_sidebar();
        }
    }

    fn click_sidebar(&mut self, row: u16) {
        let off = row.saturating_sub(self.sidebar_area.y) as usize;
        // Mirror render's clamp so clicks map to the row actually drawn.
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        let scroll = self.sidebar_scroll.min(max);
        let idx = scroll + off;
        match self.sidebar_rows.get(idx) {
            Some(SbRow::Dir { path, .. }) => {
                let path = path.clone();
                self.focus = Focus::Sidebar;
                self.sidebar_sel = idx;
                self.toggle_dir(path);
            }
            Some(SbRow::File { idx: fi, .. }) => {
                let fi = *fi;
                self.focus = Focus::Sidebar;
                self.set_current_file(fi);
            }
            None => {}
        }
    }

    /// Rebuild the diff row lists from the changeset + inline comment threads,
    /// keeping the cursor on the same (file, side, line) anchor.
    fn rebuild_rows(&mut self) {
        let key = self.sel_key();
        let cur_file = self.current_file;
        let composer = self.composer_spec();
        self.rows = build_rows(
            &self.changeset,
            &self.comments,
            self.comment_wrap,
            composer.as_ref(),
        );
        self.split_rows = build_split_rows(
            &self.changeset,
            &self.comments,
            self.comment_wrap,
            composer.as_ref(),
        );
        // Rows changed; recompute every file's span, then the current one,
        // before first_selectable/ensure_visible read it.
        self.rebuild_file_spans();
        self.recompute_file_span();
        let target = key.as_ref().and_then(|k| self.find_sel_key(k));
        self.selected = target
            .or_else(|| self.first_selectable())
            .unwrap_or(0)
            .min(self.active_len().saturating_sub(1));
        self.current_file = self.row_file_idx(self.selected).unwrap_or(cur_file);
        self.recompute_file_span();
        self.ensure_visible();
    }

    /// A stable handle to the current selection that survives a row rebuild or
    /// a view switch: the focused comment's message id, else the diff-line
    /// anchor `(file, side, line)`.
    fn sel_key(&self) -> Option<SelKey> {
        if let Some((_, cid)) = self.focused_comment() {
            return Some(SelKey::Comment(cid));
        }
        self.anchor_at(self.selected)
            .map(|(f, s, l)| SelKey::Line(f, s, l))
    }

    /// Re-find the row matching `key` in the (freshly rebuilt) active list.
    fn find_sel_key(&self, key: &SelKey) -> Option<usize> {
        (0..self.active_len()).find(|&i| match key {
            SelKey::Line(f, s, l) => {
                self.is_selectable_at(i) && self.anchor_at(i) == Some((*f, *s, *l))
            }
            SelKey::Comment(cid) => {
                self.is_stop_at(i) && self.comment_unit_at(i).map(|(_, c)| c) == Some(*cid)
            }
        })
    }

    /// Translate the live composer into a row-stream injection spec (where the
    /// box renders inline + its title), or `None` when no composer is open.
    fn composer_spec(&self) -> Option<ComposerSpec> {
        let c = self.composer.as_ref()?;
        let (anchor, title) = match &c.target {
            ComposeTarget::NewThread {
                file_idx,
                side,
                start,
                end,
            } => {
                let path = self
                    .changeset
                    .files
                    .get(*file_idx)
                    .map(|f| f.display_path())
                    .unwrap_or("?");
                let title = if start == end {
                    format!(" new comment — {path}:{start} ")
                } else {
                    format!(" new comment — {path}:{start}-{end} ")
                };
                (
                    ComposerAnchor::NewThread {
                        file_idx: *file_idx,
                        side: *side,
                        line: *start,
                    },
                    title,
                )
            }
            ComposeTarget::Reply { thread_id } => (
                ComposerAnchor::Reply {
                    thread_id: *thread_id,
                },
                " reply ".into(),
            ),
        };
        Some(ComposerSpec {
            anchor,
            title,
            body: body_with_caret(&c.textarea),
        })
    }

    /// Whether the active row at `i` is a line of the inline composer box.
    fn is_composer_at(&self, i: usize) -> bool {
        match self.view {
            View::Unified => matches!(
                self.rows.get(i).map(|r| &r.kind),
                Some(RowKind::Composer(_))
            ),
            View::Split => matches!(
                self.split_rows.get(i).map(|r| &r.kind),
                Some(SplitRowKind::Composer { .. })
            ),
        }
    }

    /// Whether the active row at `i` is the composer body row that carries the
    /// caret glyph. With cursor movement the caret can sit on any wrapped body
    /// line (not just the last), so scrolling keys off the glyph itself.
    fn is_composer_caret_at(&self, i: usize) -> bool {
        let body = match self.view {
            View::Unified => match self.rows.get(i).map(|r| &r.kind) {
                Some(RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Body(s),
                })) => s,
                _ => return false,
            },
            View::Split => match self.split_rows.get(i).map(|r| &r.kind) {
                Some(SplitRowKind::Composer {
                    line:
                        ComposerLine {
                            kind: ComposerKind::Body(s),
                        },
                    ..
                }) => s,
                _ => return false,
            },
        };
        body.contains(COMPOSER_CARET)
    }

    /// Scroll so the (contiguous) inline composer box is in view, anchored to
    /// the body row carrying the caret. The cursor can be on any line now, so
    /// when the box is taller than the viewport we keep the caret row on screen
    /// rather than pinning the top or the bottom.
    fn ensure_composer_visible(&mut self) {
        let (s, e) = self.file_range();
        let Some(first) = (s..e).find(|&i| self.is_composer_at(i)) else {
            return;
        };
        let last = (first..e)
            .take_while(|&i| self.is_composer_at(i))
            .last()
            .unwrap_or(first);
        // Anchor scroll to the row carrying the caret glyph (the cursor line),
        // wherever it is in the box — falling back to the last row if the glyph
        // somehow isn't found, so we never leave the box fully off-screen.
        let caret = (first..=last)
            .find(|&i| self.is_composer_caret_at(i))
            .unwrap_or(last);
        let height = self.height.max(1);
        // Caret below the fold: scroll down so it's the last visible row.
        if caret >= self.scroll + height {
            self.scroll = (caret + 1).saturating_sub(height).max(s);
        }
        // Caret above the viewport (cursor moved up in a tall box): scroll up to
        // it.
        if caret < self.scroll {
            self.scroll = caret.max(s);
        }
        // When the whole box fits, prefer showing its top.
        let fits = last - first < height;
        if first < self.scroll && fits {
            self.scroll = first.max(s);
        }
    }

    /// Open the composer for a new thread anchored to the current selection —
    /// the cursor line, or a multi-line range from visual mode (`v`) or a mouse
    /// drag (see [`Self::selection_range`]).
    fn open_new_thread(&mut self) {
        let Some((file_idx, side, start, end)) = self.selection_range() else {
            self.status = "put the cursor on a diff line first".into();
            return;
        };
        // A drag could be in flight when the composer opens via the keyboard;
        // clear it so the swallowed mouse-up can't leave us stuck mid-drag.
        self.resizing = false;
        self.sb_drag = None;
        self.composer = Some(Composer {
            target: ComposeTarget::NewThread {
                file_idx,
                side,
                start,
                end,
            },
            textarea: TextArea::default(),
        });
        self.status = "new comment — enter for newline, ctrl+s to submit, esc to cancel".into();
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Open the composer to reply to the focused thread (the comment the cursor
    /// is on, or the thread anchored to the focused diff line).
    fn open_reply(&mut self) {
        let Some(id) = self.focused_thread_id() else {
            self.status = "no comment thread here".into();
            return;
        };
        self.resizing = false;
        self.sb_drag = None;
        self.composer = Some(Composer {
            target: ComposeTarget::Reply { thread_id: id },
            textarea: TextArea::default(),
        });
        self.status = "reply — enter for newline, ctrl+s to submit, esc to cancel".into();
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Insert pasted text in one shot (bracketed paste). Only meaningful while
    /// the composer is open; elsewhere a paste is ignored rather than being
    /// replayed as commands. A single rebuild keeps a multi-paragraph paste from
    /// triggering one full row rebuild per character.
    fn on_paste(&mut self, text: String) {
        let Some(c) = self.composer.as_mut() else {
            return;
        };
        // Normalize newlines; `insert_str` splits on `\n` into the buffer at the
        // cursor (which then sits after the inserted text).
        c.textarea
            .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Keystrokes while the composer modal is open. hew's own chords (submit /
    /// cancel) are handled here; everything else is forwarded to the `TextArea`
    /// model, which provides readline/emacs editing (Ctrl+A/E/B/F/K/U/W,
    /// Alt+B/F, arrows, ↑/↓ line moves, Ctrl+D delete-forward, undo, …).
    fn on_key_compose(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            // Esc or Ctrl-C cancels without saving. (Ctrl+D is left to the
            // editor as delete-forward, per readline.)
            KeyCode::Esc => {
                self.composer = None;
                self.visual = false;
                self.sel_anchor = None;
                self.status = "cancelled".into();
            }
            KeyCode::Char('c') if ctrl => {
                self.composer = None;
                self.visual = false;
                self.sel_anchor = None;
                self.status = "cancelled".into();
            }
            // Ctrl+S is the primary submit: it's a C0 control byte, so it
            // survives tmux/SSH without any keyboard-protocol negotiation
            // (raw mode clears IXON, so there's no XOFF freeze). Ctrl+Enter is
            // kept as a GitHub-style alias for terminals that forward the kitty
            // keyboard-enhancement protocol (DISAMBIGUATE_ESCAPE_CODES, enabled
            // in `run`); under tmux that protocol is usually swallowed, which is
            // why a protocol-free fallback exists at all. A bare Enter inserts a
            // newline (handled by the editor). Shift+Enter is intentionally not
            // used: the protocol reports ctrl+key but not plain shift+key, so it
            // would be indistinguishable from a bare Enter.
            KeyCode::Char('s') if ctrl => self.submit_compose(),
            KeyCode::Enter if ctrl => self.submit_compose(),
            // Everything else: hand the key to the edit model. We always rebuild
            // afterward (below) rather than keying off `input()`'s return value:
            // it reports whether the *text* changed, so cursor-only moves (←/→,
            // Ctrl+A/E, ↑/↓, …) return false even though the caret moved and the
            // row stream needs to redraw it.
            _ => {
                let Some(c) = self.composer.as_mut() else {
                    return;
                };
                c.textarea.input(KeyEvent::new(code, mods));
            }
        }
        // The composer is part of the row stream now, so any state change
        // (typed text, cancel) must rebuild rows to reflect it. `submit_compose`
        // already rebuilds; a redundant rebuild here is cheap and harmless.
        self.rebuild_rows();
        if self.composer.is_some() {
            self.ensure_composer_visible();
        }
    }

    /// Commit the composer's text as a new thread or a reply.
    fn submit_compose(&mut self) {
        let Some(c) = self.composer.take() else {
            return;
        };
        let body = c.textarea.lines().join("\n").trim().to_string();
        if body.is_empty() {
            self.status = "empty comment discarded".into();
            return;
        }
        match c.target {
            ComposeTarget::NewThread {
                file_idx,
                side,
                start,
                end,
            } => {
                let Some(file) = self.changeset.files.get(file_idx) else {
                    // Defensive: the anchor's file index should always be valid
                    // (the changeset is fixed for the session).
                    self.status = "comment discarded — unknown file".into();
                    return;
                };
                let path = PathBuf::from(file.display_path());
                self.comments.add_thread(
                    path,
                    side,
                    LineRange { start, end },
                    Some("you".into()),
                    body,
                );
                // Leaving the composer also leaves visual mode.
                self.visual = false;
                self.sel_anchor = None;
                self.status = "added comment".into();
            }
            ComposeTarget::Reply { thread_id } => {
                if self.comments.reply(thread_id, Some("you".into()), body) {
                    self.status = "added reply".into();
                } else {
                    self.status = "thread no longer exists".into();
                }
            }
        }
        self.rebuild_rows();
    }

    /// The id of the first comment thread anchored to the selected line, if any.
    fn current_thread_id(&self) -> Option<Uuid> {
        let (fi, side, line) = self.anchor_at(self.selected)?;
        let file = self.changeset.files.get(fi)?;
        let path = Path::new(file.display_path());
        self.comments
            .threads
            .iter()
            .find(|t| t.file.as_path() == path && t.side == side && t.range.contains(line))
            .map(|t| t.id)
    }

    /// Toggle the resolved state of the focused thread.
    fn resolve_current_thread(&mut self) {
        let Some(id) = self.focused_thread_id() else {
            self.status = "no comment thread here".into();
            return;
        };
        match self.comments.toggle_resolved(id) {
            Some(true) => self.status = "resolved thread".into(),
            Some(false) => self.status = "unresolved thread".into(),
            None => return,
        }
        self.rebuild_rows();
    }

    /// Delete the focused *comment* — but only if it was added in this session.
    /// The unit of deletion is a single comment (e.g. a reply you wrote), never
    /// a whole thread; removing a thread's last comment drops the thread.
    /// Comments loaded from the input sidecar are immutable, so `D` on one is a
    /// no-op — which is what keeps the action log free of delete actions.
    fn delete_current_comment(&mut self) {
        let Some((thread_id, comment_id)) = self.focused_comment() else {
            self.status = "put the cursor on a comment to delete".into();
            return;
        };
        if self.base_comment_ids.contains(&comment_id) {
            self.status = "can't delete a comment from the input".into();
            return;
        }
        if self.comments.remove_comment(thread_id, comment_id) {
            self.status = "deleted comment".into();
            self.rebuild_rows();
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
        if let Some(i) = self.stop_for(idx) {
            self.selected = i;
            // A drag-range only makes sense between diff lines; landing on a
            // comment selects that one message and drops any range anchor.
            if self.is_selectable_at(i) {
                if anchor {
                    self.sel_anchor = Some(i);
                }
            } else {
                self.sel_anchor = None;
            }
            self.ensure_visible();
        }
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

    /// The comment-thread line at row `i`, in whichever view is active.
    fn comment_at(&self, i: usize) -> Option<&CommentLine> {
        match self.view {
            View::Unified => match &self.rows.get(i)?.kind {
                RowKind::Comment(cl) => Some(cl),
                _ => None,
            },
            View::Split => match &self.split_rows.get(i)?.kind {
                SplitRowKind::Comment { line, .. } => Some(line),
                _ => None,
            },
        }
    }

    /// `(thread_id, comment_id)` of the message that row `i` belongs to, if it's
    /// a content line of a comment (author/body/gap). Chrome rows return `None`.
    fn comment_unit_at(&self, i: usize) -> Option<(Uuid, Uuid)> {
        let cl = self.comment_at(i)?;
        Some((cl.thread_id, cl.comment_id?))
    }

    /// A "stop" is a place the cursor can land: a diff line, or the *first* row
    /// of a comment message (so a multi-line message is a single stop).
    fn is_stop_at(&self, i: usize) -> bool {
        if self.is_selectable_at(i) {
            return true;
        }
        match self.comment_unit_at(i) {
            // First row of a message: the row above belongs to a different
            // message (or is chrome / a diff line).
            Some((_, cid)) => i == 0 || self.comment_unit_at(i - 1).map(|(_, c)| c) != Some(cid),
            None => false,
        }
    }

    /// Inclusive `[lo, hi]` rows of the comment message covering row `i`, if `i`
    /// is a comment content line. Used to highlight/scroll the whole message.
    fn comment_unit_span(&self, i: usize) -> Option<(usize, usize)> {
        let (_, cid) = self.comment_unit_at(i)?;
        let same = |j: usize| self.comment_unit_at(j).map(|(_, c)| c) == Some(cid);
        let mut lo = i;
        while lo > 0 && same(lo - 1) {
            lo -= 1;
        }
        let mut hi = i;
        let len = self.active_len();
        while hi + 1 < len && same(hi + 1) {
            hi += 1;
        }
        Some((lo, hi))
    }

    /// First stop at/beyond `from` scanning in `dir`, within the file.
    fn nearest_stop(&self, from: usize, dir: isize) -> Option<usize> {
        let (start, end) = self.file_range();
        let mut i = from as isize;
        while i >= start as isize && (i as usize) < end {
            if self.is_stop_at(i as usize) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    /// Map a clicked/landed row to the stop it should select: itself if it's a
    /// stop, the message head if it's inside a message, else the nearest stop.
    fn stop_for(&self, idx: usize) -> Option<usize> {
        if self.is_stop_at(idx) {
            return Some(idx);
        }
        if self.comment_unit_at(idx).is_some() {
            return self.comment_unit_span(idx).map(|(lo, _)| lo);
        }
        self.nearest_stop(idx, 1)
            .or_else(|| self.nearest_stop(idx, -1))
    }

    /// `(thread_id, comment_id)` the cursor is currently on, if it's a comment.
    fn focused_comment(&self) -> Option<(Uuid, Uuid)> {
        self.comment_unit_at(self.selected)
    }

    /// The thread the cursor acts on: the focused comment's thread, else the
    /// thread anchored to the focused diff line.
    fn focused_thread_id(&self) -> Option<Uuid> {
        if let Some(cl) = self.comment_at(self.selected) {
            return Some(cl.thread_id);
        }
        self.current_thread_id()
    }

    /// The file index a row belongs to (header rows included).
    fn row_file_idx(&self, i: usize) -> Option<usize> {
        match self.view {
            View::Unified => self.rows.get(i).map(|r| r.file_idx),
            View::Split => self.split_rows.get(i).map(|r| r.file_idx),
        }
    }

    /// `[start, end)` row range of the current file in the active list. Files
    /// are contiguous, so this is a single slice. Cached (see `file_span`) and
    /// only recomputed when the file, view, or row lists change — it's read on
    /// every keystroke and several times per frame, so the O(rows) scan must
    /// not run on the hot path.
    fn file_range(&self) -> (usize, usize) {
        self.file_span
    }

    /// Recompute the cached current-file row span. Call after any change to
    /// `current_file`, `view`, or the active row list, and before code that
    /// reads `file_range` (navigation, scrolling, rendering).
    fn recompute_file_span(&mut self) {
        let len = self.active_len();
        self.file_span = self
            .file_spans
            .get(self.current_file)
            .copied()
            .unwrap_or((len, len));
    }

    /// Recompute the `[start, end)` row span of *every* file in the active row
    /// list in a single pass. Files emit contiguous row blocks (build order),
    /// so one sweep fills them all; call this whenever the active row list or
    /// view changes. Keeps per-file-switch span lookup O(1).
    fn rebuild_file_spans(&mut self) {
        let n = self.changeset.files.len();
        let len = self.active_len();
        let mut spans = vec![(len, len); n];
        for i in 0..len {
            if let Some(fi) = self.row_file_idx(i) {
                if let Some(span) = spans.get_mut(fi) {
                    if span.0 == len {
                        span.0 = i;
                    }
                    span.1 = i + 1;
                }
            }
        }
        self.file_spans = spans;
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
        self.recompute_file_span();
        // A file in a collapsed directory has no visible row; open its ancestors.
        self.reveal_file_in_tree(self.current_file);
        self.sidebar_sel = self
            .file_to_sbrow
            .get(self.current_file)
            .copied()
            .filter(|&r| r != usize::MAX)
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

    /// Toggle between unified and split, keeping the cursor on the same line
    /// (and preserving any multi-line visual selection across the switch).
    fn toggle_view(&mut self) {
        let key = self.sel_key();
        // Remember the selection anchor by its line identity so a multi-line
        // visual/drag selection survives the layout switch instead of
        // collapsing to the cursor line. The anchor is always a diff line.
        let anchor_key = self
            .sel_anchor
            .and_then(|a| self.anchor_at(a))
            .map(|(f, s, l)| SelKey::Line(f, s, l));
        self.view = match self.view {
            View::Unified => View::Split,
            View::Split => View::Unified,
        };
        // The active list switched (unified/split spans differ); recompute all
        // file spans, then the current one, before first_selectable reads it.
        self.rebuild_file_spans();
        self.recompute_file_span();
        // Re-find the same line / comment message in the other layout.
        let target = key.as_ref().and_then(|k| self.find_sel_key(k));
        self.selected = target
            .or_else(|| self.first_selectable())
            .unwrap_or(0)
            .min(self.active_len().saturating_sub(1));
        // Remap the anchor into the new layout. If it can't be found (e.g. the
        // anchored line has no counterpart in this view), drop the selection
        // rather than leave a stale row index dangling.
        self.sel_anchor = anchor_key.as_ref().and_then(|k| self.find_sel_key(k));
        if self.sel_anchor.is_none() {
            self.visual = false;
        }
        // Stay on the same file across the layout switch.
        self.current_file = self
            .row_file_idx(self.selected)
            .unwrap_or(self.current_file);
        self.recompute_file_span();
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
        self.show_sidebar && !self.changeset.files.is_empty()
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
            theme().cursor_bg
        } else {
            theme().unfocus_bg
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // While composing, the modal owns every keystroke.
        if self.composer.is_some() {
            return self.on_key_compose(code, mods);
        }
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        // Global keys, independent of the focused pane.
        match code {
            // Quit only on q / Ctrl-C / Ctrl-D (never Esc).
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Char('c') | KeyCode::Char('d') if ctrl => return self.quit = true,
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
            Focus::Diff => self.on_key_diff(code, ctrl, mods.contains(KeyModifiers::SHIFT)),
        }
    }

    /// Navigation when the file sidebar (left pane) is focused.
    fn on_key_sidebar(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_sidebar(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sidebar(-1),
            KeyCode::Char('g') | KeyCode::Home => self.sidebar_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.sidebar_edge(true),
            // Left/Right (or h/l): toggle the folder open state on a dir row.
            KeyCode::Left | KeyCode::Char('h') => self.fold_dir(true),
            KeyCode::Right | KeyCode::Char('l') => self.fold_dir(false),
            KeyCode::Char(' ') | KeyCode::Char('o') => self.fold_dir_toggle(),
            // Enter: move focus to the right (diff) pane.
            KeyCode::Enter => self.focus = Focus::Diff,
            _ => {}
        }
    }

    /// Navigation when the diff pane is focused.
    fn on_key_diff(&mut self, code: KeyCode, ctrl: bool, shift: bool) {
        let page = self.height.max(1);
        let half = (self.height / 2).max(1);
        // Shift+Up/Down extends a line selection (an alternative to `v` visual
        // mode). Modified arrow keys ride the standard CSI cursor encoding, so
        // they survive tmux/SSH without the kitty protocol — unlike Shift+Enter.
        if shift {
            match code {
                KeyCode::Down => return self.extend_selection(1),
                KeyCode::Up => return self.extend_selection(-1),
                _ => {}
            }
        }
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1, 1),

            // Half-page up: Ctrl-U (Ctrl-D is reserved for quit).
            KeyCode::Char('u') if ctrl => self.move_by(-1, half),

            // Full page: Space / Ctrl-F / PageDown forward, b / PageUp back.
            KeyCode::Char(' ') | KeyCode::Char('f') | KeyCode::PageDown => self.move_by(1, page),
            KeyCode::Char('b') | KeyCode::PageUp => self.move_by(-1, page),

            // One-line viewport scroll, cursor stays in view: Ctrl-E / Ctrl-Y (less/vim).
            KeyCode::Char('e') if ctrl => self.scroll_view(1),
            KeyCode::Char('y') if ctrl => self.scroll_view(-1),

            // Top / bottom.
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.visual {
                    self.sel_anchor = None;
                }
                self.selected = self.first_selectable().unwrap_or(0);
                self.ensure_visible();
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.visual {
                    self.sel_anchor = None;
                }
                self.selected = self.last_selectable().unwrap_or(0);
                self.ensure_visible();
            }

            // Jump between comment threads.
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),

            // Visual line-select: anchor a comment to a multi-line range.
            KeyCode::Char('v') => self.toggle_visual(),

            // Compose a new thread (i) or reply to the thread here (r).
            KeyCode::Char('i') => self.open_new_thread(),
            KeyCode::Char('r') => self.open_reply(),

            // Resolve/unresolve (R) the thread on this line, or delete (D) the
            // focused comment.
            KeyCode::Char('R') => self.resolve_current_thread(),
            KeyCode::Char('D') => self.delete_current_comment(),

            // Jump between files.
            KeyCode::Char(']') => self.jump_file(1),
            KeyCode::Char('[') => self.jump_file(-1),

            // Copy the selected line(s).
            KeyCode::Char('y') => self.copy_selection(),
            // Esc: drop any drag selection and hand focus back to the sidebar
            // (never quits).
            KeyCode::Esc => {
                self.sel_anchor = None;
                if self.visual {
                    // First Esc just leaves visual mode (keeps focus here).
                    self.visual = false;
                    self.status = "visual off".into();
                } else if self.sidebar_available() {
                    self.focus = Focus::Sidebar;
                }
            }
            _ => {}
        }
    }

    /// Enter/leave visual line-select mode. Entering anchors the selection at
    /// the cursor; leaving drops it.
    fn toggle_visual(&mut self) {
        if self.visual {
            self.visual = false;
            self.sel_anchor = None;
            self.status = "visual off".into();
        } else {
            self.visual = true;
            self.sel_anchor = Some(self.selected);
            self.status = "visual — j/k to extend, i to comment, esc to cancel".into();
        }
    }

    /// Extend the line selection by one row (Shift+Up/Down). Moves only across
    /// selectable *diff lines* (skipping comment rows): a line selection must
    /// stay anchored on diff lines, or `selection_range()` — which reads
    /// `anchor_at(selected)` — would return `None` and `i` (new thread) would
    /// have nothing to anchor to. A no-op when there is no diff line in that
    /// direction.
    ///
    /// Unlike `v`, this does NOT enter persistent visual mode: terminals can't
    /// report Shift key-release, so the heuristic is that the *next* unmodified
    /// movement (plain `j`/`k`) collapses the range (via `move_selection`'s
    /// `!visual` branch). Consecutive Shift+arrows keep growing it because the
    /// anchor survives between presses.
    fn extend_selection(&mut self, dir: isize) {
        let (start, end) = self.file_range();
        let mut i = self.selected as isize + dir;
        let target = loop {
            if i < start as isize || i as usize >= end {
                return;
            }
            if self.is_selectable_at(i as usize) {
                break i as usize;
            }
            i += dir;
        };
        // Anchor the range at the current line on the first Shift+arrow; keep it
        // on subsequent ones (so the span grows). A prior plain move will have
        // cleared it, starting a fresh range here.
        if self.sel_anchor.is_none() {
            self.sel_anchor = Some(self.selected);
        }
        self.selected = target;
        self.ensure_visible();
        self.status = "shift+↑/↓ to extend · i to comment · move to clear".into();
    }

    /// The (file, side, line-range) covered by the current selection, matching
    /// the cursor line's file+side. Falls back to the single cursor line when
    /// there's no active selection. Lines on a different side/file than the
    /// cursor are ignored (a comment anchors to one side).
    fn selection_range(&self) -> Option<(usize, Side, u32, u32)> {
        let (fi, side, cur) = self.anchor_at(self.selected)?;
        let (lo, hi) = self.selection_bounds();
        let (mut start, mut end) = (cur, cur);
        for i in lo..=hi {
            if let Some((f, s, l)) = self.anchor_at(i) {
                if f == fi && s == side {
                    start = start.min(l);
                    end = end.max(l);
                }
            }
        }
        Some((fi, side, start, end))
    }

    fn move_selection(&mut self, delta: isize) {
        if !self.visual {
            self.sel_anchor = None;
        }
        let (start, end) = self.file_range();
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < start as isize || i as usize >= end {
                return;
            }
            if self.is_stop_at(i as usize) {
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
        // Use an effective viewport height of at least 1 so scroll math stays
        // valid even when the bordered diff panel's inner height is 0.
        let height = self.height.max(1);
        // Cap at the last full screen so the wheel can't scroll past the final
        // line into empty space (which would drag the selection along with it).
        // Mirrors the scrollbar's `total - height` maximum.
        let max_top = end.saturating_sub(height).max(start) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(start as isize, max_top) as usize;
        // Scrolling the pane is independent of the selected line: the cursor
        // stays put (and simply scrolls out of view) until the user moves it.
    }

    fn ensure_visible(&mut self) {
        let (start, end) = self.file_range();
        let height = self.height.max(1);
        // For a focused comment, keep the whole message in view (biased to its
        // top when taller than the viewport); otherwise just the cursor row.
        let (top_row, bot_row) = self
            .comment_unit_span(self.selected)
            .unwrap_or((self.selected, self.selected));
        if bot_row >= self.scroll + height {
            self.scroll = bot_row + 1 - height;
        }
        if top_row < self.scroll {
            self.scroll = top_row;
        }
        // Never scroll outside the current file's slice, and never past the
        // last full screen of content.
        self.scroll = self
            .scroll
            .clamp(start, end.saturating_sub(height).max(start));
    }

    fn jump_comment(&mut self, dir: isize) {
        self.sel_anchor = None;
        // Collect the *head* row of each thread in the current file. Navigation
        // (`n`/`N`) deliberately stops once per thread, at its first line
        // (`range.start`) — unlike the act-on-thread operations (reply/resolve/
        // delete), which match anywhere in the range via `range.contains`.
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
                        .any(|t| t.file == path && t.side == side && t.range.start == line)
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

    /// Re-wrap inline comment bodies to the current diff width, rebuilding the
    /// row lists when it changes. This is the sole row-affecting side effect of
    /// drawing: the diff width is only known during layout, yet the wrapped
    /// rows must be rebuilt before the frame reads them (and before any
    /// selection mapping). While the sidebar/diff divider is being dragged the
    /// wrap is frozen — the next draw after release picks up the final width and
    /// rebuilds exactly once instead of on every drag event.
    fn sync_comment_wrap(&mut self, diff_inner_width: u16) {
        let inner = diff_inner_width as usize;
        let cw = match self.view {
            // Unified: the box spans the full inner width. Reserve a 2-col
            // margin + 2 borders + 3-col body indent + 1 scrollbar column = 8.
            View::Unified => inner.saturating_sub(8),
            // Split: the box lives inside one half-column, so wrapping to the
            // full width would clip every line on the right. Mirror
            // `render_split`'s `side_w = (area - SPLIT_DIVIDER.len()) / 2` for
            // the worst case (a scrollbar present trims the area by 1), then
            // reserve the box chrome (2-col margin + 2 borders + 3-col indent
            // = 7).
            View::Split => {
                let side = inner.saturating_sub(1 + SPLIT_DIVIDER.len()) / 2;
                side.saturating_sub(7)
            }
        };
        if cw != self.comment_wrap && !self.resizing {
            self.comment_wrap = cw;
            // Re-wrap when there are inline boxes to re-wrap: comment threads
            // or an open composer (which renders inline too).
            if !self.comments.threads.is_empty() || self.composer.is_some() {
                self.rebuild_rows();
            }
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        // Pre-highlight the file in view off the render path so scrolling stays
        // smooth. Catches every path that changes the visible file (nav, jumps,
        // scroll across file boundaries).
        self.hl.warm(self.current_file);
        let area = f.area();
        // Paint the themed background across the whole frame first; widgets draw
        // on top, and any gaps (padding, short lines) keep the theme bg instead
        // of the terminal default.
        f.render_widget(
            Block::default().style(Style::default().bg(theme().bg)),
            area,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        // Body: optional file sidebar on the left, diff on the right.
        let body = chunks[0];
        let sidebar = self.show_sidebar && !self.changeset.files.is_empty() && body.width >= 60;
        let (diff_outer, sidebar_area) = if sidebar {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(self.sidebar_width), Constraint::Min(1)])
                .split(body);
            (cols[1], cols[0])
        } else {
            (body, Rect::default())
        };
        // The diff is a floating panel sitting *on top of* the sidebar: a
        // rounded border frames it and brightens when it holds focus, so Esc
        // (which drops diff focus back to the sidebar) reads as dismissing the
        // panel. Its left border doubles as the resize divider.
        let diff_focused = self.effective_focus() == Focus::Diff;
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(theme().bg))
            .border_style(Style::default().fg(if diff_focused {
                theme().border_focus
            } else {
                theme().border_unfocus
            }));
        let diff_inner = diff_block.inner(diff_outer);
        // Layout is only known here, so the one row-affecting side effect of
        // drawing (re-wrapping inline comments to the diff width) is isolated
        // in this helper and must run before any code reads the row lists.
        self.sync_comment_wrap(diff_inner.width);
        if sidebar {
            self.render_sidebar(f, sidebar_area);
        }
        self.sidebar_area = sidebar_area;
        self.sidebar_sb =
            if sidebar_area.width > 0 && self.sidebar_rows.len() > sidebar_area.height as usize {
                Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(1),
                    y: sidebar_area.y,
                    width: 1,
                    height: sidebar_area.height,
                }
            } else {
                Rect::default()
            };
        f.render_widget(diff_block, diff_outer);
        self.height = diff_inner.height as usize;
        // Reserve the rightmost inner column for a scrollbar when it overflows.
        let (fr_start, fr_end) = self.file_range();
        let overflow = fr_end - fr_start > self.height;
        let content = if overflow {
            Rect {
                width: diff_inner.width.saturating_sub(1),
                ..diff_inner
            }
        } else {
            diff_inner
        };
        self.diff_area = content;
        self.diff_sb = if overflow {
            Rect {
                x: diff_inner.x + diff_inner.width.saturating_sub(1),
                y: diff_inner.y,
                width: 1,
                height: diff_inner.height,
            }
        } else {
            Rect::default()
        };
        self.render_diff(f, content);
        if overflow {
            self.render_diff_scrollbar(f, diff_inner);
        }

        // Status line.
        f.render_widget(
            Paragraph::new(self.status.clone())
                .style(Style::default().fg(theme().muted).bg(theme().bg)),
            chunks[1],
        );
    }

    fn render_diff(&self, f: &mut Frame, area: Rect) {
        match self.view {
            View::Unified => self.render_unified(f, area),
            View::Split => self.render_split(f, area),
        }
    }

    /// Left-hand collapsible file tree: directories and files (with a one-letter
    /// change status and a comment-state dot), indented by depth.
    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let focused = self.effective_focus() == Focus::Sidebar;
        // No border on the sidebar: the floating diff panel's rounded left
        // border is the visual divider between the two panes.
        let inner = area;

        let h = inner.height as usize;
        let n = self.sidebar_rows.len();
        // The scrollbar sits just left of the right border; reserve a content
        // column for it when the list overflows.
        let need_sb = n > h;
        let w = (inner.width as usize).saturating_sub(if need_sb { 1 } else { 0 });
        let max = n.saturating_sub(h);
        let scroll = self.sidebar_scroll.min(max);

        let mut lines: Vec<Line> = Vec::new();
        for idx in scroll..n.min(scroll + h) {
            let is_cursor = focused && idx == self.sidebar_sel;
            match &self.sidebar_rows[idx] {
                SbRow::Dir { name, depth, path } => {
                    let indent = "  ".repeat(depth + 1);
                    let arrow = if self.collapsed.contains(path) {
                        "▶ "
                    } else {
                        "▼ "
                    };
                    let avail = w.saturating_sub(indent.chars().count() + 2);
                    let label = pad_width(&elide_left(name, avail), avail);
                    let bg = if is_cursor {
                        Some(theme().cursor_bg)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    lines.push(Line::from(vec![
                        Span::styled(indent, wbg(Style::default())),
                        Span::styled(arrow, wbg(Style::default().fg(theme().faint))),
                        Span::styled(label, wbg(Style::default().fg(theme().faint))),
                    ]));
                }
                SbRow::File { idx: fi, depth } => {
                    let fi = *fi;
                    let is_cur = fi == self.current_file;
                    let (status, status_color) = self
                        .changeset
                        .files
                        .get(fi)
                        .map(file_status)
                        .unwrap_or(('M', theme().warn));
                    let indent = "  ".repeat(depth + 1);
                    let path = self
                        .changeset
                        .files
                        .get(fi)
                        .map(|f| f.display_path())
                        .unwrap_or_default();
                    // A comment dot just left of the filename: yellow = open,
                    // hollow gray = all resolved, blank = none.
                    let (dot, dot_color) = match file_comment_state(&self.comments, path) {
                        Some(true) => ("● ", theme().warn),
                        Some(false) => ("○ ", theme().muted),
                        None => ("  ", theme().none),
                    };
                    let (adds, dels) = self.file_stats.get(fi).copied().unwrap_or((0, 0));
                    let counts = format!(" +{adds} -{dels}");
                    let avail = w
                        .saturating_sub(indent.chars().count() + 4)
                        .saturating_sub(counts.chars().count());
                    let base = base_of(path);
                    let name = pad_width(&elide_left(base, avail), avail);
                    let bg = if is_cursor {
                        Some(theme().cursor_bg)
                    } else if is_cur {
                        Some(theme().unfocus_bg)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    let name_style = if is_cur {
                        Style::default()
                            .fg(theme().text_strong)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme().text)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(indent, wbg(Style::default())),
                        Span::styled(format!("{status} "), wbg(Style::default().fg(status_color))),
                        Span::styled(dot, wbg(Style::default().fg(dot_color))),
                        Span::styled(name, wbg(name_style)),
                        Span::styled(format!(" +{adds}"), wbg(Style::default().fg(theme().added))),
                        Span::styled(
                            format!(" -{dels}"),
                            wbg(Style::default().fg(theme().removed)),
                        ),
                    ]));
                }
            }
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            inner,
        );
        if need_sb {
            let mut sb = ScrollbarState::new(max + 1)
                .position(scroll)
                .viewport_content_length(h);
            // Render inside `inner` so the thumb lands one column left of the
            // pane border (which stays a clean, continuous resize divider). The
            // track is blank so it doesn't double up against the border.
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some(" "))
                    .thumb_symbol("█")
                    .thumb_style(Style::default().fg(theme().scrollbar_thumb)),
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
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("█")
                .thumb_style(Style::default().fg(theme().scrollbar_thumb)),
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
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            area,
        );
    }

    fn render_split(&self, f: &mut Frame, area: Rect) {
        let total = area.width as usize;
        let divider = SPLIT_DIVIDER;
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
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            area,
        );
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
                    .fg(theme().text_strong)
                    .bg(theme().file_header_bg)
                    .add_modifier(Modifier::BOLD),
            )),
            SplitRowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default()
                    .fg(theme().faint)
                    .add_modifier(Modifier::ITALIC),
            )),
            SplitRowKind::Pair { left, right } => {
                let mut spans = self.side_spans(left.as_ref(), row.file_idx, side_w, selected);
                spans.push(Span::styled(
                    divider.to_string(),
                    Style::default().fg(theme().subtle),
                ));
                spans.extend(self.side_spans(right.as_ref(), row.file_idx, side_w, selected));
                Line::from(spans)
            }
            SplitRowKind::Comment { side, line: cl } => {
                // Render the thread under the column it is anchored to: old
                // (deletion) comments on the left, new (addition) comments on
                // the right. The opposite column is left blank.
                let body = self.comment_line_to_line(cl, selected, side_w).spans;
                match side {
                    Side::Old => {
                        let mut spans = body;
                        // Pad out to the divider + right column so the row
                        // fills its width.
                        spans.push(Span::styled(
                            " ".repeat(divider.chars().count() + side_w),
                            Style::default(),
                        ));
                        Line::from(spans)
                    }
                    Side::New => {
                        let pad = side_w + divider.chars().count();
                        let mut spans = vec![Span::styled(" ".repeat(pad), Style::default())];
                        spans.extend(body);
                        Line::from(spans)
                    }
                }
            }
            SplitRowKind::Composer { side, line: cl } => {
                // Same side-aware placement as comment rows: render under the
                // anchored column, blank the other.
                let body = self.composer_line_to_line(cl, side_w).spans;
                match side {
                    Side::Old => {
                        let mut spans = body;
                        spans.push(Span::styled(
                            " ".repeat(divider.chars().count() + side_w),
                            Style::default(),
                        ));
                        Line::from(spans)
                    }
                    Side::New => {
                        let pad = side_w + divider.chars().count();
                        let mut spans = vec![Span::styled(" ".repeat(pad), Style::default())];
                        spans.extend(body);
                        Line::from(spans)
                    }
                }
            }
        }
    }

    /// Render one side (old/new) of a split pair into spans of width `width`.
    fn side_spans(
        &self,
        cell: Option<&SideCell>,
        file_idx: usize,
        width: usize,
        selected: bool,
    ) -> Vec<Span<'static>> {
        const PREFIX: usize = 5; // line number(4) + space(1)
        match cell {
            None => vec![Span::styled(
                " ".repeat(width),
                Style::default().bg(theme().comment_bg),
            )],
            Some(c) => {
                let num = c
                    .line
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let bg = if selected {
                    Some(self.diff_cursor_bg())
                } else {
                    match c.kind {
                        LineKind::Addition => Some(theme().add_bg),
                        LineKind::Deletion => Some(theme().del_bg),
                        LineKind::Context => None,
                    }
                };
                let mut spans = vec![Span::styled(
                    format!("{num} "),
                    Style::default().fg(theme().muted),
                )];
                spans.extend(self.styled_fit(file_idx, &c.text, width.saturating_sub(PREFIX), bg));
                spans
            }
        }
    }

    fn row_to_line(&self, row: &Row, selected: bool, width: usize) -> Line<'static> {
        match &row.kind {
            RowKind::FileHeader => {
                let st = Style::default()
                    .fg(theme().text_strong)
                    .bg(theme().file_header_bg)
                    .add_modifier(Modifier::BOLD);
                let text = format!("▌ {}", row.text);
                let pad = width.saturating_sub(str_width(&text));
                Line::from(vec![
                    Span::styled(text, st),
                    Span::styled(" ".repeat(pad), st),
                ])
            }
            RowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default()
                    .fg(theme().faint)
                    .add_modifier(Modifier::ITALIC),
            )),
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
                let (sign, code) = row.text.split_at(1);
                let bg = if selected {
                    Some(self.diff_cursor_bg())
                } else {
                    match kind {
                        LineKind::Addition => Some(theme().add_bg),
                        LineKind::Deletion => Some(theme().del_bg),
                        LineKind::Context => None,
                    }
                };
                let sign_color = match kind {
                    LineKind::Addition => theme().added,
                    LineKind::Deletion => theme().removed,
                    LineKind::Context => theme().muted,
                };
                let with_bg = |st: Style| match bg {
                    Some(b) => st.bg(b),
                    None => st,
                };
                let mut used = str_width(&num) + 1;
                let mut spans = vec![
                    Span::styled(num, with_bg(Style::default().fg(theme().muted))),
                    Span::styled(sign.to_string(), with_bg(Style::default().fg(sign_color))),
                ];
                // Highlighted code, with the diff background tint behind it.
                let hl = self.hl.runs(row.file_idx, code);
                for (c, s) in hl.iter() {
                    used += str_width(s);
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
            RowKind::Comment(cl) => self.comment_line_to_line(cl, selected, width),
            RowKind::Composer(cl) => self.composer_line_to_line(cl, width),
        }
    }

    /// Render one inline comment line as part of a rounded box spanning `width`
    /// (2-column left margin + `╭─╮`/`│ │`/`╰─╯` frame). `focused` is set for
    /// the rows of the message the cursor is on.
    fn comment_line_to_line(&self, cl: &CommentLine, focused: bool, width: usize) -> Line<'static> {
        const MARGIN: usize = 2;
        // Border brightness signals focus: the focused message keeps the bright
        // (theme) border so the cursor is visible even on a resolved thread
        // (whose box is otherwise dimmed). Only an *unfocused* resolved thread
        // reads as fully settled. Without the focus check first, landing on a
        // resolved comment gave no visual feedback at all.
        let border_col = if focused {
            theme().border_focus
        } else if cl.resolved {
            theme().muted
        } else {
            theme().border_unfocus
        };
        let bstyle = Style::default().fg(border_col);
        // Box occupies cols [MARGIN, width); inner_w is the span between borders.
        let inner_w = width.saturating_sub(MARGIN + 2);
        if width <= MARGIN + 2 {
            return Line::from(Span::raw(" ".repeat(width)));
        }
        let margin = Span::raw(" ".repeat(MARGIN));
        match &cl.kind {
            CommentKind::Top => Line::from(vec![
                margin,
                Span::styled(format!("╭{}╮", "─".repeat(inner_w)), bstyle),
            ]),
            CommentKind::Bottom => Line::from(vec![
                margin,
                Span::styled(format!("╰{}╯", "─".repeat(inner_w)), bstyle),
            ]),
            CommentKind::Author { name, date } => {
                // Name on the left, date flush right (1-col gutter before the
                // border). Spans below total exactly `inner_w`.
                let left = format!(" @{name}");
                let llen = str_width(&left);
                let dlen = str_width(date);
                let name_col = if cl.resolved {
                    theme().muted
                } else {
                    theme().warn
                };
                let name_style = Style::default().fg(name_col).add_modifier(Modifier::BOLD);
                let inner = if llen + dlen + 2 <= inner_w {
                    vec![
                        Span::styled(left, name_style),
                        Span::raw(" ".repeat(inner_w - llen - dlen - 1)),
                        Span::styled(date.clone(), Style::default().fg(theme().muted)),
                        Span::raw(" "),
                    ]
                } else {
                    let (l, lw) = take_width(&left, inner_w);
                    let pad = inner_w - lw;
                    vec![Span::styled(l, name_style), Span::raw(" ".repeat(pad))]
                };
                let mut spans = vec![margin, Span::styled("│".to_string(), bstyle)];
                spans.extend(inner);
                spans.push(Span::styled("│".to_string(), bstyle));
                Line::from(spans)
            }
            _ => {
                let (content, color, bold) = match &cl.kind {
                    CommentKind::Head { replies } => (
                        format!(
                            " ▾ {} · {} message{}",
                            if cl.resolved { "resolved" } else { "open" },
                            replies,
                            if *replies == 1 { "" } else { "s" }
                        ),
                        if cl.resolved {
                            theme().muted
                        } else {
                            theme().accent
                        },
                        true,
                    ),
                    CommentKind::Body(b) => (
                        format!("   {b}"),
                        if cl.resolved {
                            theme().muted
                        } else {
                            theme().text
                        },
                        false,
                    ),
                    _ => (String::new(), theme().text, false), // Gap
                };
                let mut cstyle = Style::default().fg(color);
                if bold {
                    cstyle = cstyle.add_modifier(Modifier::BOLD);
                }
                let clen = str_width(&content);
                let content = if clen > inner_w {
                    take_width(&content, inner_w).0
                } else {
                    format!("{content}{}", " ".repeat(inner_w - clen))
                };
                Line::from(vec![
                    margin,
                    Span::styled("│".to_string(), bstyle),
                    Span::styled(content, cstyle),
                    Span::styled("│".to_string(), bstyle),
                ])
            }
        }
    }

    /// Render one inline composer line as part of a rounded accent box spanning
    /// `width` (2-col left margin + framed title / live body / key hint).
    fn composer_line_to_line(&self, cl: &ComposerLine, width: usize) -> Line<'static> {
        const MARGIN: usize = 2;
        let bstyle = Style::default().fg(theme().accent);
        let inner_w = width.saturating_sub(MARGIN + 2);
        if width <= MARGIN + 2 {
            return Line::from(Span::raw(" ".repeat(width)));
        }
        let margin = Span::raw(" ".repeat(MARGIN));
        // Pad/truncate `s` to exactly `inner_w` display cells.
        let fit = |s: &str| -> String {
            let w = str_width(s);
            if w > inner_w {
                take_width(s, inner_w).0
            } else {
                format!("{s}{}", " ".repeat(inner_w - w))
            }
        };
        match &cl.kind {
            ComposerKind::Top { title } => {
                let t = take_width(title, inner_w).0;
                let dashes = inner_w.saturating_sub(str_width(&t));
                Line::from(vec![
                    margin,
                    Span::styled("╭".to_string(), bstyle),
                    Span::styled(
                        t,
                        Style::default()
                            .fg(theme().accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("─".repeat(dashes), bstyle),
                    Span::styled("╮".to_string(), bstyle),
                ])
            }
            ComposerKind::Bottom => Line::from(vec![
                margin,
                Span::styled(format!("╰{}╯", "─".repeat(inner_w)), bstyle),
            ]),
            ComposerKind::Body(b) => Line::from(vec![
                margin,
                Span::styled("│".to_string(), bstyle),
                Span::styled(fit(&format!(" {b}")), Style::default().fg(theme().text)),
                Span::styled("│".to_string(), bstyle),
            ]),
            ComposerKind::Hint => Line::from(vec![
                margin,
                Span::styled("│".to_string(), bstyle),
                Span::styled(
                    fit(" ctrl+s: submit · enter: newline · esc: cancel"),
                    Style::default().fg(theme().muted),
                ),
                Span::styled("│".to_string(), bstyle),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_report;

    // Four additions framed by two context lines, so the new side carries
    // lines 1..=6 and there are six selectable rows in a known order.
    const DIFF: &str = "\
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,6 @@
 a
+b
+c
+d
+e
 f
";

    // Two files, to exercise per-file navigation.
    const TWO_FILES: &str = "\
--- a/one.rs
+++ b/one.rs
@@ -1 +1,2 @@
 x
+y
--- a/two.rs
+++ b/two.rs
@@ -1 +1,2 @@
 p
+q
";

    fn app_with(diff: &str) -> App {
        let cs = parse_report(diff).0;
        let mut app = App::with_comments(cs, CommentStore::default());
        app.height = 4; // deterministic viewport for scroll math
        app
    }

    #[test]
    fn precomputed_file_spans_match_a_full_scan() {
        // The O(1) per-file-switch span lookup must agree with a brute-force
        // scan of the active row list, in both layouts.
        let mut app = app_with(TWO_FILES);
        for toggled in [false, true] {
            if toggled {
                app.toggle_view();
            }
            let len = app.active_len();
            for fi in 0..app.changeset.files.len() {
                app.current_file = fi;
                app.recompute_file_span();
                let (mut s, mut e) = (len, len);
                for i in 0..len {
                    if app.row_file_idx(i) == Some(fi) {
                        if s == len {
                            s = i;
                        }
                        e = i + 1;
                    }
                }
                assert_eq!(
                    app.file_span,
                    (s, e),
                    "file {fi} span mismatch (toggled={toggled})"
                );
            }
        }
    }

    /// Move the cursor onto the row anchored at `(current_file, side, line)`.
    fn goto(app: &mut App, side: Side, line: u32) {
        let (s, e) = app.file_range();
        for i in s..e {
            if app.anchor_at(i) == Some((app.current_file, side, line)) {
                app.selected = i;
                return;
            }
        }
        panic!("no selectable row for {side:?} line {line}");
    }

    #[test]
    fn navigation_clamps_within_the_file() {
        let mut app = app_with(DIFF);
        let first = app.first_selectable().unwrap();
        let last = app.last_selectable().unwrap();
        app.selected = first;

        // Up past the top is a no-op.
        app.move_by(-1, 5);
        assert_eq!(app.selected, first);

        // Down past the bottom clamps to the last selectable row.
        app.move_by(1, 100);
        assert_eq!(app.selected, last);
    }

    #[test]
    fn ensure_visible_keeps_cursor_in_viewport() {
        let mut app = app_with(DIFF);
        let (start, end) = app.file_range();
        app.selected = app.last_selectable().unwrap();
        app.ensure_visible();
        let h = app.height;
        assert!(app.scroll <= app.selected, "cursor above viewport");
        assert!(app.selected < app.scroll + h, "cursor below viewport");
        // Scroll never leaves the file's row slice.
        assert!(app.scroll >= start && app.scroll < end);
    }

    #[test]
    fn visual_selection_spans_a_multi_line_range() {
        let mut app = app_with(DIFF);
        goto(&mut app, Side::New, 2);
        app.toggle_visual();
        assert!(app.visual && app.sel_anchor.is_some());
        goto(&mut app, Side::New, 5);

        // The selection covers new-side lines 2..=5 on a single side.
        let (fi, side, lo, hi) = app.selection_range().unwrap();
        assert_eq!((fi, side, lo, hi), (app.current_file, Side::New, 2, 5));

        // Leaving visual drops the anchor.
        app.toggle_visual();
        assert!(!app.visual && app.sel_anchor.is_none());
    }

    #[test]
    fn shift_arrows_extend_a_line_selection() {
        // Shift+Down/Up builds a multi-line selection without entering `v`
        // visual mode, anchoring at the starting line. `visual` stays false so
        // a later unmodified move collapses the range.
        let mut app = app_with(DIFF);
        goto(&mut app, Side::New, 2);
        assert!(!app.visual && app.sel_anchor.is_none());

        app.on_key_diff(KeyCode::Down, false, true);
        app.on_key_diff(KeyCode::Down, false, true);
        assert!(!app.visual && app.sel_anchor.is_some());
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 4)
        );

        // Shrinking back up narrows the range.
        app.on_key_diff(KeyCode::Up, false, true);
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 3)
        );
    }

    #[test]
    fn unmodified_move_collapses_a_shift_selection() {
        // Regression: Shift+arrow used to flip on persistent visual mode, so the
        // multi-select stayed active after releasing Shift (terminals can't
        // report Shift key-up). A plain j/k must collapse the range back to a
        // single line.
        let mut app = app_with(DIFF);
        goto(&mut app, Side::New, 2);
        app.on_key_diff(KeyCode::Down, false, true);
        app.on_key_diff(KeyCode::Down, false, true);
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 4),
            "shift extended the range"
        );

        // A plain (no-shift) Down collapses to the single cursor line.
        app.on_key_diff(KeyCode::Down, false, false);
        assert!(app.sel_anchor.is_none(), "plain move must drop the anchor");
        let (_, side, lo, hi) = app.selection_range().unwrap();
        assert_eq!(
            (side, lo, hi),
            (Side::New, 5, 5),
            "range collapsed to one line"
        );

        // A fresh Shift+arrow starts a brand-new range from here.
        app.on_key_diff(KeyCode::Down, false, true);
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 5, 6)
        );
    }

    #[test]
    fn shift_arrows_skip_comment_rows_and_keep_a_valid_line_range() {
        // Regression: extend_selection used to step to the next *stop*, which
        // includes comment/collapsed rows — landing the cursor off a diff line
        // so selection_range() went None. It must skip over an inline thread's
        // rows and keep selecting diff lines.
        let (mut app, _tid, _reply) = app_with_thread(3);
        goto(&mut app, Side::New, 2);

        // Walk down across the line-3 thread's inline rows; every step must stay
        // on a diff line with a valid line range.
        for _ in 0..3 {
            app.on_key_diff(KeyCode::Down, false, true);
            assert!(
                app.is_selectable_at(app.selected),
                "cursor landed on a non-diff row"
            );
            assert!(
                app.selection_range().is_some(),
                "selection range went None mid-extend"
            );
        }
        // The range spans diff lines (start stays at the anchor, end advanced).
        let (_, side, lo, hi) = app.selection_range().unwrap();
        assert_eq!((side, lo), (Side::New, 2));
        assert!(hi > lo, "selection should have grown past the anchor line");
    }

    #[test]
    fn toggling_view_preserves_a_multi_line_selection() {
        // Regression: switching unified<->split used to collapse a visual
        // selection down to the cursor line. The whole range must survive.
        let mut app = app_with(DIFF);
        goto(&mut app, Side::New, 2);
        app.toggle_visual();
        goto(&mut app, Side::New, 5);
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 5)
        );

        app.toggle_view(); // -> split
        assert!(app.visual && app.sel_anchor.is_some());
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 5),
            "selection collapsed after toggling to split"
        );

        app.toggle_view(); // -> back to unified
        assert_eq!(
            app.selection_range().unwrap(),
            (app.current_file, Side::New, 2, 5),
            "selection collapsed after toggling back to unified"
        );
    }

    #[test]
    fn selection_range_is_single_line_without_an_anchor() {
        let mut app = app_with(DIFF);
        goto(&mut app, Side::New, 3);
        let (_, side, lo, hi) = app.selection_range().unwrap();
        assert_eq!((side, lo, hi), (Side::New, 3, 3));
    }

    #[test]
    fn jumping_files_moves_the_cursor_into_the_new_file() {
        let mut app = app_with(TWO_FILES);
        assert_eq!(app.current_file, 0);
        app.jump_file(1);
        assert_eq!(app.current_file, 1);
        // The cursor lands on a selectable row belonging to file 1.
        assert!(app.is_selectable_at(app.selected));
        assert_eq!(app.row_file_idx(app.selected), Some(1));
        // ...and back.
        app.jump_file(-1);
        assert_eq!(app.current_file, 0);
        assert_eq!(app.row_file_idx(app.selected), Some(0));
    }

    /// Build an app with one new-side thread (two messages) anchored to `line`,
    /// rendered inline.
    fn app_with_thread(line: u32) -> (App, Uuid, Uuid) {
        let cs = parse_report(DIFF).0;
        let mut store = CommentStore::default();
        let tid = store.add_thread(
            "f.rs".into(),
            Side::New,
            LineRange {
                start: line,
                end: line,
            },
            Some("a".into()),
            "root message".into(),
        );
        store.reply(tid, Some("b".into()), "a reply".into());
        let reply_id = store.threads[0].comments[1].id;
        let mut app = App::with_comments(cs, store);
        app.height = 40; // tall enough to hold the whole thread
        (app, tid, reply_id)
    }

    /// First active row that is a content line of comment `comment_id`.
    fn comment_head(app: &App, comment_id: Uuid) -> usize {
        (0..app.active_len())
            .find(|&i| {
                app.is_stop_at(i) && app.comment_unit_at(i).map(|(_, c)| c) == Some(comment_id)
            })
            .expect("comment head row")
    }

    #[test]
    fn delete_targets_session_comments_only() {
        // Both the root and the reply here come from the input sidecar.
        let (mut app, tid, base_reply_id) = app_with_thread(3);

        // Cursor on an input comment: `D` is a no-op.
        app.selected = comment_head(&app, base_reply_id);
        app.delete_current_comment();
        assert_eq!(app.status, "can't delete a comment from the input");
        assert_eq!(
            app.comments.threads[0].comments.len(),
            2,
            "an input comment must survive D"
        );

        // Add a reply this session (to the same base thread), then delete it.
        app.selected = comment_head(&app, base_reply_id);
        app.open_reply();
        app.on_key_compose(KeyCode::Char('y'), KeyModifiers::NONE);
        app.submit_compose();
        assert_eq!(app.comments.threads[0].comments.len(), 3);
        let new_reply_id = app.comments.threads[0].comments[2].id;

        app.selected = comment_head(&app, new_reply_id);
        app.delete_current_comment();
        assert_eq!(app.status, "deleted comment");
        assert_eq!(
            app.comments.threads[0].comments.len(),
            2,
            "only the session reply is removed"
        );
        assert!(
            app.comments.threads.iter().any(|t| t.id == tid),
            "the thread (and its input comments) survives"
        );
    }

    #[test]
    fn deleting_a_session_thread_last_comment_drops_the_thread() {
        // A wholly in-session thread: deleting its only comment removes it.
        let (mut app, _tid, _reply) = app_with_thread(3);
        goto(&mut app, Side::New, 1);
        app.open_new_thread();
        app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
        app.submit_compose();
        let new_tid = app
            .comments
            .threads
            .iter()
            .find(|t| t.range.contains(1) && t.side == Side::New)
            .expect("new thread")
            .id;
        let cid = app
            .comments
            .threads
            .iter()
            .find(|t| t.id == new_tid)
            .unwrap()
            .comments[0]
            .id;
        app.selected = comment_head(&app, cid);
        app.delete_current_comment();
        assert!(
            !app.comments.threads.iter().any(|t| t.id == new_tid),
            "emptying a thread drops it"
        );
    }

    #[test]
    fn comments_are_navigable_stops() {
        let (mut app, _tid, reply_id) = app_with_thread(3);
        // Land on the diff line the thread anchors to, then walk down: we must
        // eventually stop on each comment message (a stop that is not a line).
        goto(&mut app, Side::New, 3);
        let mut comment_stops = 0;
        for _ in 0..40 {
            app.move_by(1, 1);
            if app.comment_unit_at(app.selected).is_some() {
                comment_stops += 1;
            }
        }
        assert!(
            comment_stops >= 2,
            "navigation should stop on each comment message (got {comment_stops})"
        );
        // And the reply message is reachable as its own stop.
        let head = comment_head(&app, reply_id);
        assert!(app.is_stop_at(head));
    }

    #[test]
    fn paste_inserts_into_composer_and_is_ignored_otherwise() {
        // Outside the composer a paste is a no-op (not replayed as commands).
        let mut app = app_with(DIFF);
        app.on_paste("qqq deletes nothing".into());
        assert!(!app.quit);
        assert!(app.composer.is_none());

        // Inside the composer the whole multi-line paste lands in one shot,
        // with CRLF/CR normalized to `\n`.
        open_composer(&mut app);
        app.on_paste("first line\r\nsecond line\rthird".into());
        assert_eq!(
            app.composer.as_ref().unwrap().textarea.lines().join("\n"),
            "first line\nsecond line\nthird"
        );
        assert!(app.composer.is_some(), "paste must not submit");
    }

    #[test]
    fn resolved_thread_comment_shows_focus_border() {
        // Regression: a resolved thread's box was always drawn with the muted
        // border, even when the cursor was on it — so an individual comment in a
        // resolved thread gave no visual "selected" feedback. Focus must win.
        let (mut app, tid, reply_id) = app_with_thread(3);
        app.comments.toggle_resolved(tid);
        app.rebuild_rows();
        assert!(app.comments.threads[0].resolved);

        // The reply's individual comment is still a reachable stop...
        let head = comment_head(&app, reply_id);
        assert!(app.is_stop_at(head));

        // ...and when focused, its box border is the focus color, not muted.
        let cl = app.comment_at(head).unwrap().clone();
        let focused = app.comment_line_to_line(&cl, true, 40);
        let unfocused = app.comment_line_to_line(&cl, false, 40);
        let border_fg = |line: &ratatui::text::Line| {
            // The box border span (`╭`/`│`/`╰`) carries the border color.
            line.spans
                .iter()
                .find(|s| s.content.chars().any(|c| "╭╮╰╯│".contains(c)))
                .and_then(|s| s.style.fg)
        };
        assert_eq!(border_fg(&focused), Some(theme().border_focus));
        assert_eq!(border_fg(&unfocused), Some(theme().muted));
    }

    #[test]
    fn split_view_wraps_comment_body_into_the_half_column() {
        // Regression: comment bodies were wrapped to the full diff width but
        // rendered into a half-width column in split view, so every line got
        // clipped on the right. `sync_comment_wrap` must wrap to the split
        // column width.
        let cs = parse_report(DIFF).0;
        let mut store = CommentStore::default();
        store.add_thread(
            "f.rs".into(),
            Side::New,
            LineRange { start: 2, end: 2 },
            Some("you".into()),
            "The labor market has shifted into a higher gear, powering through \
             an energy shock and immigration restrictions to pull more people."
                .into(),
        );
        let mut app = App::with_comments(cs, store);
        app.view = View::Split;

        // Mirror render_split's column math for a known inner width.
        let inner: u16 = 90;
        app.sync_comment_wrap(inner);
        // Worst case (scrollbar present) side column, as render computes it.
        let side_w = (inner as usize).saturating_sub(1 + SPLIT_DIVIDER.len()) / 2;
        let inner_w = side_w - 2; // borders
        let indent = 3; // the "   " body indent

        // Every wrapped body fragment must fit the rendered half-column with its
        // indent — i.e. it is never clipped by `take_width`.
        let mut body_rows = 0;
        for i in 0..app.split_rows.len() {
            if let SplitRowKind::Comment { line, .. } = &app.split_rows[i].kind {
                if let CommentKind::Body(b) = &line.kind {
                    body_rows += 1;
                    assert!(
                        str_width(b) + indent <= inner_w,
                        "body fragment {:?} ({}+{}) exceeds split inner width {}",
                        b,
                        str_width(b),
                        indent,
                        inner_w
                    );
                }
            }
        }
        assert!(body_rows >= 2, "long body should wrap to several rows");
    }

    #[test]
    fn focusing_a_comment_selects_the_whole_message_and_its_thread() {
        let (mut app, tid, reply_id) = app_with_thread(3);
        let head = comment_head(&app, reply_id);
        app.selected = head;

        // The focused-thread action target is the comment's thread.
        assert_eq!(app.focused_thread_id(), Some(tid));
        assert_eq!(app.focused_comment(), Some((tid, reply_id)));

        // Every row of the message (and only those) is in the selection.
        let (lo, hi) = app.comment_unit_span(head).unwrap();
        assert!(hi >= lo);
        for i in lo..=hi {
            assert!(
                app.in_selection(i),
                "row {i} of the message should highlight"
            );
        }
        assert!(!app.in_selection(lo.saturating_sub(1)) || lo == 0);
        assert!(!app.in_selection(hi + 1));
    }

    #[test]
    fn comment_selection_survives_view_toggle() {
        let (mut app, tid, reply_id) = app_with_thread(3);
        app.selected = comment_head(&app, reply_id);
        app.toggle_view(); // unified <-> split
        assert_eq!(
            app.focused_comment(),
            Some((tid, reply_id)),
            "the same comment should stay focused across a view switch"
        );
    }

    /// Open a new-thread composer anchored on a known diff line.
    fn open_composer(app: &mut App) {
        goto(app, Side::New, 3);
        app.open_new_thread();
        assert!(app.composer.is_some(), "composer should be open");
    }

    /// The current composer text (no caret), for assertions.
    fn composer_text(app: &App) -> String {
        app.composer.as_ref().unwrap().textarea.lines().join("\n")
    }

    #[test]
    fn composer_supports_readline_cursor_editing() {
        let mut app = app_with(DIFF);
        open_composer(&mut app);
        // Type "ac", move left (←), insert "b" between them — cursor editing.
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('c'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Left, KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(composer_text(&app), "abc");

        // Ctrl+A jumps to line start; typed text lands there.
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL);
        app.on_key_compose(KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(composer_text(&app), "Xabc");

        // Ctrl+E jumps to line end.
        app.on_key_compose(KeyCode::Char('e'), KeyModifiers::CONTROL);
        app.on_key_compose(KeyCode::Char('Z'), KeyModifiers::NONE);
        assert_eq!(composer_text(&app), "XabcZ");
    }

    #[test]
    fn cursor_move_rebuilds_the_rendered_composer() {
        // Regression: a cursor-only key (←) doesn't change the text, so
        // TextArea::input returns false. The row stream must still be rebuilt,
        // or the drawn caret would stay put while the real cursor moved.
        let mut app = app_with(DIFF);
        app.toggle_view(); // Split -> Unified, so the caret rides a `Row`
        open_composer(&mut app);
        // Tests never draw, so `comment_wrap` would be 0 and wrap each glyph
        // onto its own line; give the body a real width so it stays one line.
        app.comment_wrap = 40;
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Left, KeyModifiers::NONE);
        let body = app
            .rows
            .iter()
            .find_map(|r| match &r.kind {
                RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Body(s),
                }) if s.contains(COMPOSER_CARET) => Some(s.clone()),
                _ => None,
            })
            .expect("a composer body row carrying the caret");
        assert_eq!(body, "a\u{2588}b", "the drawn caret must follow the cursor");
    }

    #[test]
    fn composer_caret_renders_at_the_cursor() {
        let mut app = app_with(DIFF);
        open_composer(&mut app);
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL); // to start
        let spec = app.composer_spec().expect("composer spec");
        assert_eq!(
            spec.body, "\u{2588}ab",
            "caret renders at the cursor, not the end"
        );
    }

    #[test]
    fn ctrl_d_deletes_forward_and_does_not_cancel() {
        // Ctrl+D is readline delete-forward now, not a cancel chord.
        let mut app = app_with(DIFF);
        open_composer(&mut app);
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL); // to start
        app.on_key_compose(KeyCode::Char('d'), KeyModifiers::CONTROL); // delete 'a'
        assert!(app.composer.is_some(), "Ctrl+D must not cancel");
        assert_eq!(composer_text(&app), "b");
    }

    #[test]
    fn composer_keeps_caret_visible_when_taller_than_viewport() {
        // Regression: typing a long comment scrolled the box's *top* into view
        // and pushed the caret (its bottom body line) off-screen below. The
        // viewport must follow the caret instead.
        let mut app = app_with(DIFF);
        app.height = 6; // viewport far shorter than the box
        app.toggle_view(); // default Split -> Unified (refreshes file span)
        assert!(matches!(app.view, View::Unified));
        open_composer(&mut app);
        for _ in 0..200 {
            app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
        }
        app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
        app.ensure_composer_visible();

        let (s, e) = app.file_range();
        // The caret rides the last composer Body row.
        let caret = (s..e)
            .rev()
            .find(|&i| {
                matches!(
                    app.rows.get(i).map(|r| &r.kind),
                    Some(RowKind::Composer(ComposerLine {
                        kind: ComposerKind::Body(_)
                    }))
                )
            })
            .expect("composer body row");
        assert!(
            caret >= app.scroll && caret < app.scroll + app.height,
            "caret row {caret} must stay within view [{}, {})",
            app.scroll,
            app.scroll + app.height
        );
    }

    #[test]
    fn composer_caret_visible_on_a_tiny_viewport() {
        // The caret sits on the last Body row, with Hint+Bottom chrome below it.
        // Anchoring to the box's bottom border (height < 3) would leave the
        // caret off-screen, so the scroll must follow the body row instead.
        let mut app = app_with(DIFF);
        app.height = 2;
        app.toggle_view(); // default Split -> Unified
        open_composer(&mut app);
        for _ in 0..50 {
            app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
        }
        app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
        app.ensure_composer_visible();

        let (s, e) = app.file_range();
        let caret = (s..e)
            .find(|&i| app.is_composer_caret_at(i))
            .expect("composer caret row");
        assert!(
            caret >= app.scroll && caret < app.scroll + app.height,
            "caret row {caret} must stay within view [{}, {}) on a tiny viewport",
            app.scroll,
            app.scroll + app.height
        );
    }

    #[test]
    fn ensure_composer_visible_follows_caret_upward() {
        // Regression: scrolling anchored to the *last* body row, so with the
        // cursor moved up in a box taller than the viewport, a bottom-anchored
        // scroll left the caret off-screen above. The pass must pull the
        // viewport up to the caret row.
        let mut app = app_with(DIFF);
        app.height = 4;
        app.toggle_view(); // Split -> Unified
        open_composer(&mut app);
        app.comment_wrap = 40;
        for _ in 0..40 {
            app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
        }
        app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
        // Cursor to the top of the buffer, then force the viewport to the
        // bottom (as if we'd just been typing at the end) and re-run the pass.
        for _ in 0..45 {
            app.on_key_compose(KeyCode::Up, KeyModifiers::NONE);
        }
        let (_, e) = app.file_range();
        app.scroll = e.saturating_sub(app.height); // bottom-anchored
        app.ensure_composer_visible();

        let (s, e) = app.file_range();
        let caret = (s..e)
            .find(|&i| app.is_composer_caret_at(i))
            .expect("composer caret row");
        assert!(
            caret >= app.scroll && caret < app.scroll + app.height,
            "caret row {caret} must stay within view [{}, {}) when the cursor is up",
            app.scroll,
            app.scroll + app.height
        );
    }

    #[test]
    fn bare_enter_inserts_a_newline_in_the_composer() {
        let mut app = app_with(DIFF);
        open_composer(&mut app);
        app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
        // A bare Enter must NOT submit — the composer stays open with a newline.
        assert!(app.composer.is_some(), "bare Enter should not submit");
        assert_eq!(
            app.composer.as_ref().unwrap().textarea.lines().join("\n"),
            "a\nb"
        );
    }

    #[test]
    fn ctrl_enter_submits_the_composer() {
        let (mut app, _tid, _reply) = app_with_thread(3);
        let before = app.comments.threads.len();
        goto(&mut app, Side::New, 1);
        app.open_new_thread();
        app.on_key_compose(KeyCode::Char('h'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('i'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Enter, KeyModifiers::CONTROL);
        // Ctrl+Enter submits: composer closes and a new thread is recorded.
        assert!(app.composer.is_none(), "Ctrl+Enter should submit");
        assert_eq!(app.comments.threads.len(), before + 1);
    }

    #[test]
    fn ctrl_s_submits_the_composer() {
        // The protocol-free primary submit: a C0 control byte that survives
        // tmux/SSH even when the kitty keyboard protocol (and thus Ctrl+Enter)
        // is not forwarded.
        let (mut app, _tid, _reply) = app_with_thread(3);
        let before = app.comments.threads.len();
        goto(&mut app, Side::New, 1);
        app.open_new_thread();
        app.on_key_compose(KeyCode::Char('h'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('i'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(app.composer.is_none(), "Ctrl+S should submit");
        assert_eq!(app.comments.threads.len(), before + 1);
    }

    #[test]
    fn shift_enter_does_not_submit_the_composer() {
        // Regression: Shift+Enter is indistinguishable from a bare Enter under
        // the DISAMBIGUATE_ESCAPE_CODES protocol, so it must behave like one
        // (insert a newline) rather than submit.
        let mut app = app_with(DIFF);
        open_composer(&mut app);
        app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
        app.on_key_compose(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(app.composer.is_some(), "Shift+Enter must not submit");
        assert_eq!(
            app.composer.as_ref().unwrap().textarea.lines().join("\n"),
            "x\n"
        );
    }
}
