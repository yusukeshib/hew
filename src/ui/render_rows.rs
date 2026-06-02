//! Flatten a [`Changeset`] into lightweight rows for rendering + anchoring.

use crate::diff::model::{Changeset, LineKind, Side};

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
            let header = format!(
                "@@ -{},{} +{},{} @@{}",
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                hunk.section
                    .as_ref()
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default()
            );
            rows.push(Row {
                file_idx: fi,
                kind: RowKind::HunkHeader,
                text: header,
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
                    text: format!("{prefix}{}", line.text),
                });
            }
        }
    }
    rows
}
