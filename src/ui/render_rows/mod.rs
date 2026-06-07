//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::comments::model::{CommentStore, Thread};
use crate::diff::model::{Changeset, LineKind, Side};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

/// Terminal display width of a string in cells (wide/CJK glyphs count as 2,
/// zero-width/control as 0). The whole TUI lays text out in fixed cells, so
/// column math must measure cells, not `char`s, or wide glyphs misalign.
mod text;
pub use text::{char_width, sanitize_line, str_width, take_width};
use text::{fmt_date, sanitize_into, wrap_preserve, wrap_text};

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
            for wl in wrap_preserve(&s, width) {
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

/// Map each thread (across the whole changeset, keyed by thread id) to the
/// *last* `(side, line)` anchor within its range that is actually present in
/// the diff. A range comment renders after this line, so its box sits below the
/// last selected line (GitHub-style) rather than the first.
fn last_anchor_lines(
    changeset: &Changeset,
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
) -> HashMap<String, (Side, u32)> {
    let mut m = HashMap::new();
    for file in &changeset.files {
        let Some(indices) = by_path.get(Path::new(file.display_path())) else {
            continue;
        };
        for &i in indices {
            let t = &comments.threads[i];
            let mut best: Option<u32> = None;
            for line in file.hunks.iter().flat_map(|h| h.lines.iter()) {
                let l = match t.side {
                    Side::Old => line.old_line,
                    Side::New => line.new_line,
                };
                if let Some(l) = l {
                    if t.range.contains(l) {
                        best = Some(best.map_or(l, |b| b.max(l)));
                    }
                }
            }
            if let Some(l) = best {
                m.insert(t.id.clone(), (t.side, l));
            }
        }
    }
    m
}

/// Inline-comment lines to inject after a code row, for every thread whose
/// last in-diff anchor line (see [`last_anchor_lines`]) is one of the row's
/// `(side, line)` anchors.
#[allow(clippy::too_many_arguments)]
fn comment_rows_for(
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    last: &HashMap<String, (Side, u32)>,
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
        // Emit each thread once, at the last line of its range present in the
        // diff. `emitted` guards against a repeated emit.
        if emitted.contains(&t.id) {
            continue;
        }
        if last
            .get(&t.id)
            .is_some_and(|&(ts, tl)| anchors.iter().any(|&(s, l)| s == ts && l == tl))
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
    let last = last_anchor_lines(changeset, comments, &by_path);
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
                            &last,
                            &mut emitted,
                            path,
                            width,
                            composer,
                            &mut composer_emitted,
                        );
                        rows.push(SplitRow {
                            file_idx: fi,
                            kind: SplitRowKind::Pair {
                                // Context lines are identical on both sides;
                                // reuse the already-sanitized text instead of
                                // re-scanning `line.text` a second time.
                                left: Some(SideCell {
                                    kind: LineKind::Context,
                                    line: line.old_line,
                                    text: cell.text.clone(),
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
                            &last,
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
                &last,
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
    last: &HashMap<String, (Side, u32)>,
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
        for (side, inj) in comment_rows_for(
            comments, by_path, last, emitted, path, &anchors, width, composer,
        ) {
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
    let last = last_anchor_lines(changeset, comments, &by_path);
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
                // Build `"{prefix}{sanitized}"` in one allocation: push the
                // diff sign, then sanitize straight into the same buffer
                // (avoids the extra alloc+copy a `format!` would do).
                let mut text = String::with_capacity(line.text.len() + 1);
                text.push(prefix);
                sanitize_into(&mut text, &line.text);
                rows.push(Row {
                    file_idx: fi,
                    kind: RowKind::Line {
                        kind: line.kind,
                        old_line: line.old_line,
                        new_line: line.new_line,
                    },
                    text,
                });
                let (side, ln) = match line.kind {
                    LineKind::Deletion => (Side::Old, line.old_line),
                    _ => (Side::New, line.new_line),
                };
                if let Some(ln) = ln {
                    for (_, inj) in comment_rows_for(
                        comments,
                        &by_path,
                        &last,
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
mod tests;
