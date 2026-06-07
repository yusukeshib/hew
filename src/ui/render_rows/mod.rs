//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::comments::model::CommentStore;
use crate::diff::model::{Changeset, LineKind, Side};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

/// Terminal display width of a string in cells (wide/CJK glyphs count as 2,
/// zero-width/control as 0). The whole TUI lays text out in fixed cells, so
/// column math must measure cells, not `char`s, or wide glyphs misalign.
mod text;
use text::sanitize_into;
pub use text::{char_width, sanitize_line, str_width, take_width};

mod threads;
use threads::{
    comment_rows_for, composer_lines, last_anchor_lines, last_anchor_lines_for,
    new_thread_composer, threads_by_path, Injected, ThreadsByPath,
};
pub use threads::{
    thread_lines, CommentKind, CommentLine, ComposerAnchor, ComposerKind, ComposerLine,
    ComposerSpec,
};

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
    build_split_rows_inner(changeset, comments, width, composer, None)
}

/// Split rows for a single file (`fi`). See [`build_file_rows`] for the
/// incremental-rebuild rationale; this is its split-view counterpart.
pub fn build_file_split_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
    fi: usize,
) -> Vec<SplitRow> {
    build_split_rows_inner(changeset, comments, width, composer, Some(fi))
}

fn build_split_rows_inner(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
    only: Option<usize>,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut composer_emitted = false;
    let by_path = threads_by_path(comments);
    let last = last_for(changeset, comments, &by_path, only);
    for (fi, file) in changeset.files.iter().enumerate() {
        if only.is_some_and(|o| o != fi) {
            continue;
        }
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

/// Build the unified (stack) row list for the whole changeset.
pub fn build_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<Row> {
    build_rows_inner(changeset, comments, width, composer, None)
}

/// Build the unified rows for a *single* file (`fi`), used by the incremental
/// per-edit rebuild: a comment/composer edit only changes the rows of the file
/// it is anchored to, so we rebuild just that file's contiguous row block and
/// splice it into the active list instead of re-sanitizing every line of every
/// file. `file_idx` on the returned rows still equals `fi`, so they slot
/// straight back in.
pub fn build_file_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
    fi: usize,
) -> Vec<Row> {
    build_rows_inner(changeset, comments, width, composer, Some(fi))
}

/// Anchor map scoped to what the build needs: every file for a full build, or
/// just the one file for a single-file (`Some(fi)`) rebuild.
fn last_for(
    changeset: &Changeset,
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    only: Option<usize>,
) -> HashMap<String, (Side, u32)> {
    match only.and_then(|fi| changeset.files.get(fi)) {
        Some(file) => last_anchor_lines_for(file, comments, by_path),
        None => last_anchor_lines(changeset, comments, by_path),
    }
}

/// Shared core: build rows for every file (`only == None`) or just one
/// (`only == Some(fi)`). The whole-changeset path threads a single `emitted`
/// set across files (its long-standing behavior, byte-for-byte unchanged); the
/// single-file path starts fresh, which differs only in the pathological case
/// of two diff entries sharing one display path.
fn build_rows_inner(
    changeset: &Changeset,
    comments: &CommentStore,
    width: usize,
    composer: Option<&ComposerSpec>,
    only: Option<usize>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut composer_emitted = false;
    let by_path = threads_by_path(comments);
    let last = last_for(changeset, comments, &by_path, only);
    for (fi, file) in changeset.files.iter().enumerate() {
        if only.is_some_and(|o| o != fi) {
            continue;
        }
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
