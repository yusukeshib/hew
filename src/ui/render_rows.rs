//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::diff::model::{Changeset, LineKind, Side};

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
pub fn build_split_rows(changeset: &Changeset) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    for (fi, file) in changeset.files.iter().enumerate() {
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
                        flush_pairs(fi, &mut dels, &mut adds, &mut rows);
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
                    }
                }
            }
            flush_pairs(fi, &mut dels, &mut adds, &mut rows);
        }
    }
    rows
}

/// Emit the accumulated deletion/addition runs as zipped pairs.
fn flush_pairs(
    file_idx: usize,
    dels: &mut Vec<SideCell>,
    adds: &mut Vec<SideCell>,
    rows: &mut Vec<SplitRow>,
) {
    let n = dels.len().max(adds.len());
    let mut di = dels.drain(..);
    let mut ai = adds.drain(..);
    for _ in 0..n {
        rows.push(SplitRow {
            file_idx,
            kind: SplitRowKind::Pair {
                left: di.next(),
                right: ai.next(),
            },
            text: String::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_line;

    #[test]
    fn expands_tabs_and_strips_controls() {
        assert_eq!(sanitize_line("\tx"), "    x");
        assert_eq!(sanitize_line("a\tb"), "a   b"); // tab to next 4-col stop
        assert_eq!(sanitize_line("end\r"), "end");
        assert_eq!(sanitize_line("a\u{0}b"), "ab");
        // ANSI CSI color sequence is removed, payload kept.
        assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m"), "red");
    }
}

/// Build the unified (stack) row list.
pub fn build_rows(changeset: &Changeset) -> Vec<Row> {
    let mut rows = Vec::new();
    for (fi, file) in changeset.files.iter().enumerate() {
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
            }
        }
    }
    rows
}
