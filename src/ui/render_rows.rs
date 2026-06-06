//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::comments::model::{CommentStore, Thread};
use crate::diff::model::{Changeset, LineKind, Side};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

/// Terminal display width of a string in cells (wide/CJK glyphs count as 2,
/// zero-width/control as 0). The whole TUI lays text out in fixed cells, so
/// column math must measure cells, not `char`s, or wide glyphs misalign.
pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Display width of a single char (control chars treated as 0; they're stripped
/// before rendering anyway).
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Take the longest prefix of `s` whose display width does not exceed `max`,
/// returning the prefix and its actual width. A wide glyph straddling the
/// boundary is dropped (so the result never overflows `max`).
pub fn take_width(s: &str, max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    (out, w)
}

/// Format a `SystemTime` as a UTC `YYYY-MM-DD HH:MM` timestamp (no external
/// date crate).
fn fmt_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm) = (tod / 3600, (tod % 3600) / 60);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// One visual line of an inline-expanded comment thread.
#[derive(Debug, Clone)]
pub enum CommentKind {
    /// Top rounded border of the thread box.
    Top,
    /// Thread header: message count (resolved state lives on `CommentLine`).
    Head { replies: usize },
    /// A message author line (`@name`) with its formatted date.
    Author { name: String, date: String },
    /// A (pre-wrapped) body line.
    Body(String),
    /// Blank spacer between messages.
    Gap,
    /// The action-button row (reply / resolve / delete) above the bottom border.
    Actions,
    /// Bottom rounded border of the thread box.
    Bottom,
}

/// One visual line of a thread box, tagged with the thread's `resolved` state
/// so the renderer can dim the whole box (not just the header) when resolved,
/// plus identity so the line can be selected/focused. `comment_id` is the
/// owning message for content lines (`Author`/`Body`/`Gap`) and `None` for
/// thread chrome (`Top`/`Head`/`Bottom`) — selection keys off it, so a message
/// (author + its body lines) forms one selectable unit.
#[derive(Debug, Clone)]
pub struct CommentLine {
    pub kind: CommentKind,
    pub resolved: bool,
    pub thread_id: String,
    pub comment_id: Option<String>,
}

/// Where an open inline composer attaches in the row stream.
#[derive(Debug, Clone)]
pub enum ComposerAnchor {
    /// A new thread, anchored to the first line of the selected diff range.
    NewThread {
        file_idx: usize,
        side: Side,
        line: u32,
    },
    /// A reply, injected just below an existing thread's box.
    Reply { thread_id: String },
}

/// An open inline composer to inject into the row stream while typing.
#[derive(Debug, Clone)]
pub struct ComposerSpec {
    pub anchor: ComposerAnchor,
    pub title: String,
    pub body: String,
}

/// One visual line of the inline composer box.
#[derive(Debug, Clone)]
pub enum ComposerKind {
    /// Top rounded border, carrying the box title.
    Top { title: String },
    /// A (pre-wrapped) body line; the line at the cursor carries the caret glyph.
    Body(String),
    /// The one-line key hint above the bottom border.
    Hint,
    /// Bottom rounded border.
    Bottom,
}

#[derive(Debug, Clone)]
pub struct ComposerLine {
    pub kind: ComposerKind,
}

/// A row injected after a code line: an existing comment thread line, or a line
/// of the live composer box. Internal to row building — not part of the crate
/// API.
enum Injected {
    Comment(CommentLine),
    Composer(ComposerLine),
}

/// Expand an open composer into wrapped visual lines (a rounded box with the
/// title, the live buffer + caret, and a key hint).
fn composer_lines(spec: &ComposerSpec, width: usize) -> Vec<ComposerLine> {
    let mut out = vec![ComposerLine {
        kind: ComposerKind::Top {
            title: spec.title.clone(),
        },
    }];
    // `spec.body` already carries the caret glyph at the cursor position (see
    // `body_with_caret`); the glyph survives `sanitize_line` (not a control
    // char) and wraps with the surrounding text.
    for raw in spec.body.split('\n') {
        let s = sanitize_line(raw);
        if s.is_empty() {
            out.push(ComposerLine {
                kind: ComposerKind::Body(String::new()),
            });
        } else {
            for wl in wrap_text(&s, width) {
                out.push(ComposerLine {
                    kind: ComposerKind::Body(wl),
                });
            }
        }
    }
    out.push(ComposerLine {
        kind: ComposerKind::Hint,
    });
    out.push(ComposerLine {
        kind: ComposerKind::Bottom,
    });
    out
}

/// Composer lines for a *new* thread anchored exactly at `(file_idx, side,
/// line)`, emitted at most once per build (tracked via `emitted`).
fn new_thread_composer(
    composer: Option<&ComposerSpec>,
    emitted: &mut bool,
    file_idx: usize,
    side: Side,
    line: u32,
    width: usize,
) -> Vec<ComposerLine> {
    if *emitted {
        return Vec::new();
    }
    if let Some(spec) = composer {
        if let ComposerAnchor::NewThread {
            file_idx: f,
            side: s,
            line: l,
        } = spec.anchor
        {
            if f == file_idx && s == side && l == line {
                *emitted = true;
                return composer_lines(spec, width);
            }
        }
    }
    Vec::new()
}

/// Greedy word-wrap to `width` display columns, hard-splitting over-long words.
/// All measurements are in terminal cells (wide glyphs count as 2).
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut w = 0usize;
    // Append `word` to the current line, hard-splitting it across lines when it
    // overflows. Only break before a glyph when the current line is non-empty,
    // so a single glyph wider than `width` (e.g. a CJK char at width 1) lands on
    // its own line instead of emitting a spurious empty line ahead of it.
    let push_overlong = |word: &str, out: &mut Vec<String>, line: &mut String, w: &mut usize| {
        for ch in word.chars() {
            let cw = char_width(ch);
            if *w > 0 && *w + cw > width {
                out.push(std::mem::take(line));
                *w = 0;
            }
            line.push(ch);
            *w += cw;
        }
    };
    let push_word = |word: &str, out: &mut Vec<String>, line: &mut String, w: &mut usize| {
        let ww = str_width(word);
        if *w == 0 {
            push_overlong(word, out, line, w);
        } else if *w + 1 + ww <= width {
            line.push(' ');
            line.push_str(word);
            *w += 1 + ww;
        } else {
            out.push(std::mem::take(line));
            *w = 0;
            push_overlong(word, out, line, w);
        }
    };
    for word in s.split_whitespace() {
        push_word(word, &mut out, &mut line, &mut w);
    }
    out.push(line);
    out
}

/// Expand a thread into wrapped visual lines.
pub fn thread_lines(t: &Thread, width: usize) -> Vec<CommentLine> {
    // Chrome lines (Top/Head/Bottom) carry no message id; content lines carry
    // their owning message's id so author + body + trailing gap select as one.
    let chrome = |kind: CommentKind| CommentLine {
        kind,
        resolved: t.resolved,
        thread_id: t.id.clone(),
        comment_id: None,
    };
    let content = |kind: CommentKind, cid: &str| CommentLine {
        kind,
        resolved: t.resolved,
        thread_id: t.id.clone(),
        comment_id: Some(cid.to_string()),
    };
    let mut out = vec![
        chrome(CommentKind::Top),
        chrome(CommentKind::Head {
            replies: t.comments.len(),
        }),
    ];
    for (i, c) in t.comments.iter().enumerate() {
        out.push(content(
            CommentKind::Author {
                name: c.author.clone().unwrap_or_else(|| "?".into()),
                date: fmt_date(c.created_at),
            },
            &c.id,
        ));
        for raw in c.body.split('\n') {
            let s = sanitize_line(raw);
            if s.is_empty() {
                out.push(content(CommentKind::Body(String::new()), &c.id));
            } else {
                for wl in wrap_text(&s, width) {
                    out.push(content(CommentKind::Body(wl), &c.id));
                }
            }
        }
        if i + 1 < t.comments.len() {
            out.push(content(CommentKind::Gap, &c.id));
        }
    }
    out.push(chrome(CommentKind::Actions));
    out.push(chrome(CommentKind::Bottom));
    out
}

/// Index thread positions by their anchored file path, so the per-line
/// injection scans only the (usually few) threads on the current file instead
/// of every thread in the changeset. Built once per row rebuild.
type ThreadsByPath<'a> = std::collections::HashMap<&'a Path, Vec<usize>>;

fn threads_by_path(comments: &CommentStore) -> ThreadsByPath<'_> {
    let mut map: ThreadsByPath<'_> = std::collections::HashMap::new();
    for (i, t) in comments.threads.iter().enumerate() {
        map.entry(t.file.as_path()).or_default().push(i);
    }
    map
}

/// Inline-comment lines to inject after a code row, for every thread whose
/// anchor matches one of the row's `(side, line)` anchors.
#[allow(clippy::too_many_arguments)]
fn comment_rows_for(
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    emitted: &mut HashSet<String>,
    path: &str,
    anchors: &[(Side, u32)],
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<(Side, Injected)> {
    let mut out = Vec::new();
    let Some(indices) = by_path.get(Path::new(path)) else {
        return out;
    };
    for &i in indices {
        let t = &comments.threads[i];
        // Emit each thread once, at the first line of its range present in the
        // diff (anchor ranges can span several lines). `emitted` dedups across
        // multiple matching anchor lines.
        if emitted.contains(&t.id) {
            continue;
        }
        if anchors
            .iter()
            .any(|(s, l)| *s == t.side && t.range.contains(*l))
        {
            emitted.insert(t.id.clone());
            out.extend(
                thread_lines(t, width)
                    .into_iter()
                    .map(|cl| (t.side, Injected::Comment(cl))),
            );
            // A reply composer sits directly under the thread it replies to.
            if let Some(spec) = composer {
                if matches!(&spec.anchor, ComposerAnchor::Reply { thread_id } if *thread_id == t.id)
                {
                    out.extend(
                        composer_lines(spec, width)
                            .into_iter()
                            .map(|cl| (t.side, Injected::Composer(cl))),
                    );
                }
            }
        }
    }
    out
}

/// Make a line safe for a TUI cell grid. ratatui diffs cells between frames, so
/// a stray `\r`, tab, or ANSI escape corrupts the terminal and never self-heals.
/// We expand tabs (4-col stops), drop CR/LF, strip ANSI CSI/OSC sequences, and
/// drop any remaining control characters.
pub fn sanitize_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => {
                let n = 4 - (col % 4);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' | '\n' => {}
            '\u{1b}' => match chars.peek() {
                // CSI: ESC [ ... <final 0x40..=0x7e>
                Some('[') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&p) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... (BEL | ESC \)
                Some(']') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if p == '\u{7}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() => {}
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub enum RowKind {
    FileHeader,
    HunkHeader,
    Line {
        kind: LineKind,
        old_line: Option<u32>,
        new_line: Option<u32>,
    },
    /// An inline-expanded comment-thread line (non-selectable).
    Comment(CommentLine),
    /// A line of the live inline composer box (non-selectable).
    Composer(ComposerLine),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub file_idx: usize,
    pub kind: RowKind,
    pub text: String,
}

impl Row {
    /// The comment anchor (side, line) for a code line, if any.
    pub fn anchor(&self) -> Option<(Side, u32)> {
        match &self.kind {
            RowKind::Line {
                kind,
                old_line,
                new_line,
            } => match kind {
                LineKind::Deletion => old_line.map(|l| (Side::Old, l)),
                _ => new_line.map(|l| (Side::New, l)),
            },
            _ => None,
        }
    }
    pub fn is_selectable(&self) -> bool {
        matches!(self.kind, RowKind::Line { .. })
    }
}

/// One side (left=old / right=new) of a split row.
#[derive(Debug, Clone)]
pub struct SideCell {
    pub kind: LineKind,
    pub line: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum SplitRowKind {
    FileHeader,
    HunkHeader,
    /// A side-by-side pair. Either side may be empty (padding).
    Pair {
        left: Option<SideCell>,
        right: Option<SideCell>,
    },
    /// An inline-expanded comment-thread line (non-selectable). `side` records
    /// which column (old=left / new=right) the thread is anchored to so split
    /// view can render it under the correct side.
    Comment {
        side: Side,
        line: CommentLine,
    },
    /// A line of the live inline composer box (non-selectable). `side` records
    /// the anchored column so split view renders it under the correct side.
    Composer {
        side: Side,
        line: ComposerLine,
    },
}

#[derive(Debug, Clone)]
pub struct SplitRow {
    pub file_idx: usize,
    pub kind: SplitRowKind,
    pub text: String, // header text
}

impl SplitRow {
    pub fn is_selectable(&self) -> bool {
        matches!(self.kind, SplitRowKind::Pair { .. })
    }

    /// Comment anchor: prefer the new (right) side, fall back to old (left).
    pub fn anchor(&self) -> Option<(Side, u32)> {
        match &self.kind {
            SplitRowKind::Pair { left, right } => right
                .as_ref()
                .and_then(|c| c.line.map(|l| (Side::New, l)))
                .or_else(|| left.as_ref().and_then(|c| c.line.map(|l| (Side::Old, l)))),
            _ => None,
        }
    }
}

fn hunk_header(hunk: &crate::diff::model::Hunk) -> String {
    format!(
        "@@ -{},{} +{},{} @@{}",
        hunk.old_start,
        hunk.old_count,
        hunk.new_start,
        hunk.new_count,
        hunk.section
            .as_ref()
            .map(|s| format!(" {s}"))
            .unwrap_or_default()
    )
}

/// Build the side-by-side (split) row list. Within a change block, consecutive
/// deletions (left) are zipped with consecutive additions (right); context
/// lines appear on both sides.
pub fn build_split_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut composer_emitted = false;
    let by_path = threads_by_path(comments);
    for (fi, file) in changeset.files.iter().enumerate() {
        let path = file.display_path();
        rows.push(SplitRow {
            file_idx: fi,
            kind: SplitRowKind::FileHeader,
            text: file.display_path().to_string(),
        });
        if file.is_binary {
            rows.push(SplitRow {
                file_idx: fi,
                kind: SplitRowKind::HunkHeader,
                text: "Binary file".into(),
            });
            continue;
        }
        for hunk in &file.hunks {
            rows.push(SplitRow {
                file_idx: fi,
                kind: SplitRowKind::HunkHeader,
                text: hunk_header(hunk),
            });
            let mut dels: Vec<SideCell> = Vec::new();
            let mut adds: Vec<SideCell> = Vec::new();
            for line in &hunk.lines {
                let cell = SideCell {
                    kind: line.kind,
                    line: match line.kind {
                        LineKind::Deletion => line.old_line,
                        _ => line.new_line,
                    },
                    text: sanitize_line(&line.text),
                };
                match line.kind {
                    LineKind::Deletion => dels.push(cell),
                    LineKind::Addition => adds.push(cell),
                    LineKind::Context => {
                        flush_pairs(
                            fi,
                            &mut dels,
                            &mut adds,
                            &mut rows,
                            comments,
                            &by_path,
                            &mut emitted,
                            path,
                            width,
                            composer,
                            &mut composer_emitted,
                        );
                        rows.push(SplitRow {
                            file_idx: fi,
                            kind: SplitRowKind::Pair {
                                left: Some(SideCell {
                                    kind: LineKind::Context,
                                    line: line.old_line,
                                    text: sanitize_line(&line.text),
                                }),
                                right: Some(cell),
                            },
                            text: String::new(),
                        });
                        let mut anchors = Vec::new();
                        if let Some(l) = line.old_line {
                            anchors.push((Side::Old, l));
                        }
                        if let Some(l) = line.new_line {
                            anchors.push((Side::New, l));
                        }
                        for (side, inj) in comment_rows_for(
                            comments,
                            &by_path,
                            &mut emitted,
                            path,
                            &anchors,
                            width,
                            composer,
                        ) {
                            rows.push(SplitRow {
                                file_idx: fi,
                                kind: split_injected(side, inj),
                                text: String::new(),
                            });
                        }
                        for (side, l) in &anchors {
                            for cl in new_thread_composer(
                                composer,
                                &mut composer_emitted,
                                fi,
                                *side,
                                *l,
                                width,
                            ) {
                                rows.push(SplitRow {
                                    file_idx: fi,
                                    kind: SplitRowKind::Composer {
                                        side: *side,
                                        line: cl,
                                    },
                                    text: String::new(),
                                });
                            }
                        }
                    }
                }
            }
            flush_pairs(
                fi,
                &mut dels,
                &mut adds,
                &mut rows,
                comments,
                &by_path,
                &mut emitted,
                path,
                width,
                composer,
                &mut composer_emitted,
            );
        }
        for (side, inj) in
            orphan_thread_rows(comments, &by_path, &mut emitted, path, width, composer)
        {
            rows.push(SplitRow {
                file_idx: fi,
                kind: split_injected(side, inj),
                text: String::new(),
            });
        }
    }
    rows
}

/// Map an injected row to its split-view kind under column `side`.
fn split_injected(side: Side, inj: Injected) -> SplitRowKind {
    match inj {
        Injected::Comment(line) => SplitRowKind::Comment { side, line },
        Injected::Composer(line) => SplitRowKind::Composer { side, line },
    }
}

/// Emit the accumulated deletion/addition runs as zipped pairs.
#[allow(clippy::too_many_arguments)]
fn flush_pairs(
    file_idx: usize,
    dels: &mut Vec<SideCell>,
    adds: &mut Vec<SideCell>,
    rows: &mut Vec<SplitRow>,
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    emitted: &mut HashSet<String>,
    path: &str,
    width: usize,
    composer: Option<&ComposerSpec>,
    composer_emitted: &mut bool,
) {
    let n = dels.len().max(adds.len());
    let mut di = dels.drain(..);
    let mut ai = adds.drain(..);
    for _ in 0..n {
        let left = di.next();
        let right = ai.next();
        let mut anchors = Vec::new();
        if let Some(l) = left.as_ref().and_then(|c| c.line) {
            anchors.push((Side::Old, l));
        }
        if let Some(l) = right.as_ref().and_then(|c| c.line) {
            anchors.push((Side::New, l));
        }
        rows.push(SplitRow {
            file_idx,
            kind: SplitRowKind::Pair { left, right },
            text: String::new(),
        });
        for (side, inj) in
            comment_rows_for(comments, by_path, emitted, path, &anchors, width, composer)
        {
            rows.push(SplitRow {
                file_idx,
                kind: split_injected(side, inj),
                text: String::new(),
            });
        }
        for (side, l) in &anchors {
            for cl in new_thread_composer(composer, composer_emitted, file_idx, *side, *l, width) {
                rows.push(SplitRow {
                    file_idx,
                    kind: SplitRowKind::Composer {
                        side: *side,
                        line: cl,
                    },
                    text: String::new(),
                });
            }
        }
    }
}

/// Threads on `path` that never matched a visible diff line. Their anchor sits
/// outside every shown hunk — e.g. a review comment on an unchanged line, or one
/// GitHub repositioned to an out-of-hunk line. Such a thread is still counted by
/// the sidebar's comment dot, so without this it would be advertised yet
/// unreachable. Emitted once, after the file's hunks, so every thread the
/// sidebar promises is navigable. `emitted` dedups against inline threads.
fn orphan_thread_rows(
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    emitted: &mut HashSet<String>,
    path: &str,
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<(Side, Injected)> {
    let mut out = Vec::new();
    let Some(indices) = by_path.get(Path::new(path)) else {
        return out;
    };
    for &i in indices {
        let t = &comments.threads[i];
        if emitted.contains(t.id.as_str()) {
            continue;
        }
        emitted.insert(t.id.clone());
        out.extend(
            thread_lines(t, width)
                .into_iter()
                .map(|cl| (t.side, Injected::Comment(cl))),
        );
        // A reply composer sits directly under the thread it replies to.
        if let Some(spec) = composer {
            if matches!(&spec.anchor, ComposerAnchor::Reply { thread_id } if *thread_id == t.id) {
                out.extend(
                    composer_lines(spec, width)
                        .into_iter()
                        .map(|cl| (t.side, Injected::Composer(cl))),
                );
            }
        }
    }
    out
}

/// Build the unified (stack) row list.
pub fn build_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut composer_emitted = false;
    let by_path = threads_by_path(comments);
    for (fi, file) in changeset.files.iter().enumerate() {
        let path = file.display_path();
        rows.push(Row {
            file_idx: fi,
            kind: RowKind::FileHeader,
            text: file.display_path().to_string(),
        });
        if file.is_binary {
            rows.push(Row {
                file_idx: fi,
                kind: RowKind::HunkHeader,
                text: "Binary file".into(),
            });
            continue;
        }
        for hunk in &file.hunks {
            rows.push(Row {
                file_idx: fi,
                kind: RowKind::HunkHeader,
                text: hunk_header(hunk),
            });
            for line in &hunk.lines {
                let prefix = match line.kind {
                    LineKind::Addition => '+',
                    LineKind::Deletion => '-',
                    LineKind::Context => ' ',
                };
                rows.push(Row {
                    file_idx: fi,
                    kind: RowKind::Line {
                        kind: line.kind,
                        old_line: line.old_line,
                        new_line: line.new_line,
                    },
                    text: format!("{prefix}{}", sanitize_line(&line.text)),
                });
                let (side, ln) = match line.kind {
                    LineKind::Deletion => (Side::Old, line.old_line),
                    _ => (Side::New, line.new_line),
                };
                if let Some(ln) = ln {
                    for (_, inj) in comment_rows_for(
                        comments,
                        &by_path,
                        &mut emitted,
                        path,
                        &[(side, ln)],
                        width,
                        composer,
                    ) {
                        let kind = match inj {
                            Injected::Comment(cl) => RowKind::Comment(cl),
                            Injected::Composer(cl) => RowKind::Composer(cl),
                        };
                        rows.push(Row {
                            file_idx: fi,
                            kind,
                            text: String::new(),
                        });
                    }
                    for cl in
                        new_thread_composer(composer, &mut composer_emitted, fi, side, ln, width)
                    {
                        rows.push(Row {
                            file_idx: fi,
                            kind: RowKind::Composer(cl),
                            text: String::new(),
                        });
                    }
                }
            }
        }
        for (_, inj) in orphan_thread_rows(comments, &by_path, &mut emitted, path, width, composer)
        {
            let kind = match inj {
                Injected::Comment(cl) => RowKind::Comment(cl),
                Injected::Composer(cl) => RowKind::Composer(cl),
            };
            rows.push(Row {
                file_idx: fi,
                kind,
                text: String::new(),
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::model::{CommentStore, LineRange};
    use crate::diff::parse::parse_report;
    use crate::loader::{load_comments, load_patch};
    use std::path::{Path, PathBuf};

    // A two-line change: old line 2 (`b`) deleted, new line 2 (`B`) added.
    const SIMPLE_DIFF: &str = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@ fn main
 a
-b
+B
 c
";

    fn store_with(side: Side, line: u32) -> CommentStore {
        let mut store = CommentStore::default();
        store.add_thread(
            PathBuf::from("foo.txt"),
            side,
            LineRange {
                start: line,
                end: line,
            },
            Some("me".into()),
            "a comment".into(),
        );
        store
    }

    #[test]
    fn split_comment_renders_under_anchored_side() {
        let cs = parse_report(SIMPLE_DIFF).0;

        // Old-side thread (anchored to the deleted line 2) is tagged Old.
        let old = store_with(Side::Old, 2);
        let rows = build_split_rows(&cs, &old, 80, None);
        assert!(
            rows.iter().any(|r| matches!(
                r.kind,
                SplitRowKind::Comment {
                    side: Side::Old,
                    ..
                }
            )),
            "old-side comment should be tagged Side::Old"
        );
        assert!(
            !rows.iter().any(|r| matches!(
                r.kind,
                SplitRowKind::Comment {
                    side: Side::New,
                    ..
                }
            )),
            "old-side comment must not be tagged Side::New"
        );

        // New-side thread (anchored to the added line 2) is tagged New.
        let new = store_with(Side::New, 2);
        let rows = build_split_rows(&cs, &new, 80, None);
        assert!(
            rows.iter().any(|r| matches!(
                r.kind,
                SplitRowKind::Comment {
                    side: Side::New,
                    ..
                }
            )),
            "new-side comment should be tagged Side::New"
        );
    }

    #[test]
    fn thread_anchored_outside_any_hunk_is_still_emitted() {
        // A comment on a line the diff never shows (new-side line 99, far past
        // the only hunk) used to be counted by the sidebar yet never rendered.
        // It must still appear, appended after the file's hunks, in both views.
        let cs = parse_report(SIMPLE_DIFF).0;
        let orphan = store_with(Side::New, 99);

        let unified = build_rows(&cs, &orphan, 80, None);
        assert!(
            unified
                .iter()
                .any(|r| matches!(r.kind, RowKind::Comment(_))),
            "an out-of-hunk thread must still render in the unified view"
        );

        let split = build_split_rows(&cs, &orphan, 80, None);
        assert!(
            split
                .iter()
                .any(|r| matches!(r.kind, SplitRowKind::Comment { .. })),
            "an out-of-hunk thread must still render in the split view"
        );
    }

    #[test]
    fn in_hunk_thread_is_not_double_emitted_by_orphan_pass() {
        // Dedup: a thread shown inline must not be re-emitted as an orphan.
        let cs = parse_report(SIMPLE_DIFF).0;
        let inhunk = store_with(Side::New, 2); // anchored to the added line 2
        let rows = build_rows(&cs, &inhunk, 80, None);
        let box_tops = rows
            .iter()
            .filter(
                |r| matches!(&r.kind, RowKind::Comment(cl) if matches!(cl.kind, CommentKind::Top)),
            )
            .count();
        assert_eq!(box_tops, 1, "thread emitted exactly once");
    }

    #[test]
    fn expands_tabs_and_strips_controls() {
        assert_eq!(sanitize_line("\tx"), "    x");
        assert_eq!(sanitize_line("a\tb"), "a   b"); // tab to next 4-col stop
        assert_eq!(sanitize_line("end\r"), "end");
        assert_eq!(sanitize_line("a\u{0}b"), "ab");
        // ANSI CSI color sequence is removed, payload kept.
        assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn display_width_counts_wide_glyphs() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本語"), 6); // 3 wide CJK glyphs = 6 cells
        assert_eq!(str_width("a日b"), 4);
    }

    #[test]
    fn take_width_never_overflows_on_wide_glyphs() {
        // Budget 3 over "a日本": fits 'a'(1)+'日'(2)=3; '本' would overflow.
        let (s, w) = take_width("a日本", 3);
        assert_eq!(s, "a日");
        assert_eq!(w, 3);
        // Odd budget straddling a wide glyph drops it (no half-cell).
        let (s, w) = take_width("日本", 1);
        assert_eq!(s, "");
        assert_eq!(w, 0);
    }

    #[test]
    fn wrap_text_wraps_on_display_width() {
        // Three wide glyphs (6 cells) wrap at width 4 (2 glyphs per line).
        let lines = wrap_text("日本語", 4);
        assert!(lines.iter().all(|l| str_width(l) <= 4));
        assert_eq!(lines.concat(), "日本語");
    }

    #[test]
    fn wrap_text_no_empty_lines_for_unsplittable_glyphs() {
        // A glyph wider than the width can't be split; it must land on its own
        // line with no spurious empty line ahead of it.
        let lines = wrap_text("日本", 1);
        assert_eq!(lines, vec!["日".to_string(), "本".to_string()]);
        assert!(lines.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn injects_inline_thread_rows() {
        let cs = load_patch(Some(Path::new("examples/rust-long-en.patch"))).unwrap();
        let comments = load_comments(Path::new("examples/rust-long-en.comments.json")).unwrap();
        let base = build_rows(&cs, &CommentStore::default(), 80, None);
        let rows = build_rows(&cs, &comments, 80, None);
        // Threads inject extra (comment) rows over a no-comment baseline.
        assert!(rows.len() > base.len());
        // Those rows are comment rows: non-selectable and anchorless.
        let comment_rows = rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Comment(_)))
            .count();
        assert!(comment_rows > 0);
        // Each thread renders exactly one header, regardless of multi-line
        // anchor ranges (no per-line duplication).
        let heads = rows
            .iter()
            .filter(|r| {
                matches!(
                    &r.kind,
                    RowKind::Comment(CommentLine {
                        kind: CommentKind::Head { .. },
                        ..
                    })
                )
            })
            .count();
        assert_eq!(heads, comments.threads.len());
        assert!(rows.iter().all(|r| !matches!(r.kind, RowKind::Comment(_))
            || (!r.is_selectable() && r.anchor().is_none())));
    }

    #[test]
    fn new_thread_composer_injects_inline_box() {
        let cs = parse_report(SIMPLE_DIFF).0;
        let store = CommentStore::default();
        let spec = ComposerSpec {
            anchor: ComposerAnchor::NewThread {
                file_idx: 0,
                side: Side::New,
                line: 2,
            },
            title: " new comment ".into(),
            body: "hi".into(),
        };
        // Without a composer, no composer rows; with one, a box appears.
        assert!(!build_rows(&cs, &store, 80, None)
            .iter()
            .any(|r| matches!(r.kind, RowKind::Composer(_))));
        let rows = build_rows(&cs, &store, 80, Some(&spec));
        // Exactly one top + one bottom border (a single box), and it carries
        // the live body text. Composer rows are never selectable.
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(
                    r.kind,
                    RowKind::Composer(ComposerLine {
                        kind: ComposerKind::Top { .. }
                    })
                ))
                .count(),
            1
        );
        assert!(rows
            .iter()
            .any(|r| matches!(&r.kind, RowKind::Composer(ComposerLine { kind: ComposerKind::Body(b) }) if b.contains("hi"))));
        assert!(rows
            .iter()
            .all(|r| !matches!(r.kind, RowKind::Composer(_)) || !r.is_selectable()));
    }

    #[test]
    fn reply_composer_injects_under_its_thread() {
        let cs = parse_report(SIMPLE_DIFF).0;
        let store = store_with(Side::New, 2);
        let thread_id = store.threads[0].id.clone();
        let spec = ComposerSpec {
            anchor: ComposerAnchor::Reply { thread_id },
            title: " reply ".into(),
            body: "ok".into(),
        };
        let rows = build_rows(&cs, &store, 80, Some(&spec));
        // The reply box renders after the thread's bottom border.
        let bottom = rows.iter().position(|r| {
            matches!(
                &r.kind,
                RowKind::Comment(CommentLine {
                    kind: CommentKind::Bottom,
                    ..
                })
            )
        });
        let comp_top = rows.iter().position(|r| {
            matches!(
                r.kind,
                RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Top { .. }
                })
            )
        });
        assert!(bottom.is_some() && comp_top.is_some());
        assert!(
            comp_top > bottom,
            "reply composer must sit below its thread"
        );
    }
}
