//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::comments::model::{CommentStore, Thread};
use crate::diff::model::{Changeset, LineKind, Side};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Bottom rounded border of the thread box.
    Bottom,
}

/// One visual line of a thread box, tagged with the thread's `resolved` state
/// so the renderer can dim the whole box (not just the header) when resolved.
#[derive(Debug, Clone)]
pub struct CommentLine {
    pub kind: CommentKind,
    pub resolved: bool,
}

/// Greedy word-wrap to `width` columns, hard-splitting over-long words.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut w = 0usize;
    let push_word = |word: &str, out: &mut Vec<String>, line: &mut String, w: &mut usize| {
        let ww = word.chars().count();
        if *w == 0 {
            if ww <= width {
                line.push_str(word);
                *w = ww;
            } else {
                let mut cw = 0;
                for ch in word.chars() {
                    if cw == width {
                        out.push(std::mem::take(line));
                        cw = 0;
                    }
                    line.push(ch);
                    cw += 1;
                }
                *w = cw;
            }
        } else if *w + 1 + ww <= width {
            line.push(' ');
            line.push_str(word);
            *w += 1 + ww;
        } else {
            out.push(std::mem::take(line));
            *w = 0;
            // re-handle this word at the start of a fresh line
            if ww <= width {
                line.push_str(word);
                *w = ww;
            } else {
                let mut cw = 0;
                for ch in word.chars() {
                    if cw == width {
                        out.push(std::mem::take(line));
                        cw = 0;
                    }
                    line.push(ch);
                    cw += 1;
                }
                *w = cw;
            }
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
    let tag = |kind: CommentKind| CommentLine {
        kind,
        resolved: t.resolved,
    };
    let mut out = vec![
        tag(CommentKind::Top),
        tag(CommentKind::Head {
            replies: t.comments.len(),
        }),
    ];
    for (i, c) in t.comments.iter().enumerate() {
        out.push(tag(CommentKind::Author {
            name: c.author.clone().unwrap_or_else(|| "?".into()),
            date: fmt_date(c.created_at),
        }));
        for raw in c.body.split('\n') {
            let s = sanitize_line(raw);
            if s.is_empty() {
                out.push(tag(CommentKind::Body(String::new())));
            } else {
                for wl in wrap_text(&s, width) {
                    out.push(tag(CommentKind::Body(wl)));
                }
            }
        }
        if i + 1 < t.comments.len() {
            out.push(tag(CommentKind::Gap));
        }
    }
    out.push(tag(CommentKind::Bottom));
    out
}

/// Inline-comment lines to inject after a code row, for every expanded thread
/// whose anchor matches one of the row's `(side, line)` anchors.
fn comment_rows_for(
    comments: &CommentStore,
    expanded: &HashSet<usize>,
    emitted: &mut HashSet<usize>,
    path: &str,
    anchors: &[(Side, u32)],
    width: usize,
) -> Vec<(Side, CommentLine)> {
    let mut out = Vec::new();
    for (i, t) in comments.threads.iter().enumerate() {
        if !expanded.contains(&i) || emitted.contains(&i) || t.file.as_path() != Path::new(path) {
            continue;
        }
        // Emit once per thread, at the first line of its range present in the
        // diff (anchor ranges can span several lines).
        if anchors
            .iter()
            .any(|(s, l)| *s == t.side && t.range.contains(*l))
        {
            emitted.insert(i);
            out.extend(thread_lines(t, width).into_iter().map(|cl| (t.side, cl)));
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
    expanded: &HashSet<usize>,
    width: usize,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut emitted = HashSet::new();
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
                            expanded,
                            &mut emitted,
                            path,
                            width,
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
                        for (side, cl) in comment_rows_for(
                            comments,
                            expanded,
                            &mut emitted,
                            path,
                            &anchors,
                            width,
                        ) {
                            rows.push(SplitRow {
                                file_idx: fi,
                                kind: SplitRowKind::Comment { side, line: cl },
                                text: String::new(),
                            });
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
                expanded,
                &mut emitted,
                path,
                width,
            );
        }
    }
    rows
}

/// Emit the accumulated deletion/addition runs as zipped pairs.
#[allow(clippy::too_many_arguments)]
fn flush_pairs(
    file_idx: usize,
    dels: &mut Vec<SideCell>,
    adds: &mut Vec<SideCell>,
    rows: &mut Vec<SplitRow>,
    comments: &CommentStore,
    expanded: &HashSet<usize>,
    emitted: &mut HashSet<usize>,
    path: &str,
    width: usize,
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
        for (side, cl) in comment_rows_for(comments, expanded, emitted, path, &anchors, width) {
            rows.push(SplitRow {
                file_idx,
                kind: SplitRowKind::Comment { side, line: cl },
                text: String::new(),
            });
        }
    }
}

/// Build the unified (stack) row list.
pub fn build_rows(
    changeset: &Changeset,
    comments: &CommentStore,
    expanded: &HashSet<usize>,
    width: usize,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut emitted = HashSet::new();
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
                    for (_, cl) in comment_rows_for(
                        comments,
                        expanded,
                        &mut emitted,
                        path,
                        &[(side, ln)],
                        width,
                    ) {
                        rows.push(Row {
                            file_idx: fi,
                            kind: RowKind::Comment(cl),
                            text: String::new(),
                        });
                    }
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::model::{CommentStore, LineRange};
    use crate::diff::parse::parse_unified;
    use crate::loader::{load_comments, load_patch};
    use std::collections::HashSet;
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
        let cs = parse_unified(SIMPLE_DIFF);
        let all: HashSet<usize> = [0].into_iter().collect();

        // Old-side thread (anchored to the deleted line 2) is tagged Old.
        let old = store_with(Side::Old, 2);
        let rows = build_split_rows(&cs, &old, &all, 80);
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
        let rows = build_split_rows(&cs, &new, &all, 80);
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
    fn expands_tabs_and_strips_controls() {
        assert_eq!(sanitize_line("\tx"), "    x");
        assert_eq!(sanitize_line("a\tb"), "a   b"); // tab to next 4-col stop
        assert_eq!(sanitize_line("end\r"), "end");
        assert_eq!(sanitize_line("a\u{0}b"), "ab");
        // ANSI CSI color sequence is removed, payload kept.
        assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn injects_expanded_thread_rows() {
        let cs = load_patch(Some(Path::new("examples/rust-long-en.patch"))).unwrap();
        let comments = load_comments(Path::new("examples/rust-long-en.comments.json")).unwrap();
        let none = HashSet::new();
        let all: HashSet<usize> = (0..comments.threads.len()).collect();
        let base = build_rows(&cs, &comments, &none, 80);
        let rows = build_rows(&cs, &comments, &all, 80);
        // Expanding threads injects extra rows.
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
}
