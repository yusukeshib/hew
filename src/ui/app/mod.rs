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
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

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

/// Greedy width-wrap a run of highlighted code spans into visual lines, each at
/// most `budget` display cells wide. Used by the soft-wrap renderer. Run (i.e.
/// color) boundaries never force a break — wrapping is purely a function of
/// cumulative display width — so the line count is independent of how the text
/// is split into runs (see [`wrap_count`], the height oracle, which must stay
/// in lockstep). A glyph wider than `budget` lands alone on its own line rather
/// than being dropped. `bg` is applied to every emitted span. Spans are *not*
/// padded to `budget`; the caller adds the prefix and trailing fill.
fn wrap_runs(
    runs: &[(Color, String)],
    budget: usize,
    bg: Option<Color>,
) -> Vec<Vec<Span<'static>>> {
    let budget = budget.max(1);
    let style = |c: Color| {
        let mut st = Style::default().fg(c);
        if let Some(b) = bg {
            st = st.bg(b);
        }
        st
    };
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut w = 0usize;
    for (c, s) in runs {
        // Accumulate same-color glyphs into `buf`, flushing a span (and ending
        // the visual line) at the width boundary.
        let mut buf = String::new();
        for ch in s.chars() {
            let cw = char_width(ch);
            if w + cw > budget && w > 0 {
                if !buf.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut buf), style(*c)));
                }
                lines.push(std::mem::take(&mut cur));
                w = 0;
            }
            buf.push(ch);
            w += cw;
        }
        if !buf.is_empty() {
            cur.push(Span::styled(buf, style(*c)));
        }
    }
    lines.push(cur);
    lines
}

/// Number of visual lines `text` occupies when wrapped to `budget` columns —
/// the height oracle for the wrap layout. Mirrors [`wrap_runs`] exactly (breaks
/// only on cumulative display width), so `wrap_count(text, b) ==
/// wrap_runs(runs_of(text), b).len()` for any run split of `text`. Always >= 1.
fn wrap_count(text: &str, budget: usize) -> usize {
    let budget = budget.max(1);
    let mut lines = 1usize;
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = char_width(ch);
        if w + cw > budget && w > 0 {
            lines += 1;
            w = 0;
        }
        w += cw;
    }
    lines
}

/// A view-independent handle to the selected row, stable across row rebuilds
/// and unified/split switches (raw indices are not).
enum SelKey {
    /// A diff line, keyed by its comment anchor `(file, side, line)`.
    Line(usize, Side, u32),
    /// A comment message, keyed by its stable id.
    Comment(String),
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
    Reply { thread_id: String },
}

/// In-progress comment text and where it lands. The text + cursor live in a
/// [`TextArea`], used purely as an edit model (readline/emacs keybindings,
/// multi-line cursor, undo) — the box is drawn inline in the diff row stream,
/// not via tui-textarea's own widget.
struct Composer {
    target: ComposeTarget,
    textarea: TextArea<'static>,
}

/// Zero-width marker spliced in at the composer's cursor position. It is a
/// non-control format char, so it survives `sanitize_line`; being zero-width,
/// it never consumes a cell during wrapping (so moving the cursor can't reflow
/// the text) and it is never emitted to the terminal — the renderer finds it,
/// drops it, and draws the cursor as a *reversed* overlay on the cell under it.
const COMPOSER_CARET: char = '\u{2060}';

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

/// A clickable on-screen button's effect. Recorded with the button's screen
/// `Rect` during render (see `App::button_hits`) and dispatched on left-click.
/// Thread/comment ids are captured so a click acts on *that* box regardless of
/// where the keyboard cursor currently sits.
#[derive(Clone, Debug)]
enum ButtonAction {
    /// Start a new comment on the current diff-line selection (same as `i`).
    AddComment,
    /// Submit the open composer (same as Ctrl+S).
    Submit,
    /// Cancel the open composer (same as Esc).
    Cancel,
    /// Reply to the given thread.
    Reply(String),
    /// Toggle resolved on the given thread.
    ToggleResolve(String),
    /// Delete a session-added comment (thread_id, comment_id).
    Delete(String, String),
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
    base_comment_ids: HashSet<String>,
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
    /// Clickable button regions recorded during the last render (the inline
    /// composer's submit/cancel and a thread box's reply/resolve/delete), so a
    /// left-click can be mapped to its action. Rebuilt every frame; uses
    /// interior mutability because the render path is `&self`.
    button_hits: RefCell<Vec<(Rect, ButtonAction)>>,
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
    /// Soft-wrap long diff code lines instead of clipping them at the right
    /// edge (on by default; toggled with `w`). When off, every row is exactly
    /// one terminal line (the original 1:1 model and its fast paths are
    /// preserved).
    wrap: bool,
    /// Per-row display height (terminal lines) of the *active* view, valid only
    /// while `wrap` is on. A logical row (`self.selected`/`self.scroll` index)
    /// may span several display lines once wrapped; this caches each row's
    /// height so the viewport, mouse mapping, and scrollbar can convert between
    /// row indices and display lines without re-wrapping every frame.
    row_heights: Vec<u16>,
    /// Prefix sum of `row_heights` (`row_offsets[i]` = display lines before row
    /// `i`, length `active_len + 1`), so `display_lines`/scrollbar range sums
    /// are O(1) instead of O(rows) per draw. Rebuilt with `row_heights`; empty
    /// while wrap is off (heights are all 1 then, so sums are plain row counts).
    row_offsets: Vec<usize>,
    /// Content width `row_heights` was computed for; a resize invalidates them.
    heights_width: usize,
    /// Set when the active row list changes (rebuild / view switch / wrap
    /// toggle), forcing `update_heights` to recompute on the next draw.
    heights_dirty: bool,
    quit: bool,
}

mod comments;
mod input;
mod nav;
mod render;
mod render_boxes;
mod render_lines;
mod wrap;

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
        let base_comment_ids: HashSet<String> = comments
            .threads
            .iter()
            .flat_map(|t| t.comments.iter().map(|c| c.id.clone()))
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
            status: String::new(),
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
            button_hits: RefCell::new(Vec::new()),
            hl,
            file_span: (0, 0),
            file_spans: Vec::new(),
            composer: None,
            visual: false,
            wrap: true,
            row_heights: Vec::new(),
            row_offsets: Vec::new(),
            heights_width: usize::MAX,
            heights_dirty: true,
            quit: false,
        };
        app.rebuild_file_spans();
        app.recompute_file_span();
        app.selected = app.first_selectable().unwrap_or(0);
        app
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
}

#[cfg(test)]
mod tests;
