//! Generate a [`DiffFile`] from two text blobs using `similar`.

use crate::diff::model::{DiffFile, DiffLine, Hunk, LineKind};
use similar::{ChangeTag, TextDiff};

/// Build a single-file diff between `old` and `new` text.
///
/// `context` is the number of unchanged lines kept around each change.
pub fn diff_texts(old_path: &str, new_path: &str, old: &str, new: &str, context: usize) -> DiffFile {
    let text_diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    for group in text_diff.grouped_ops(context) {
        let mut lines = Vec::new();
        let (mut old_start, mut new_start) = (u32::MAX, u32::MAX);
        let (mut old_count, mut new_count) = (0u32, 0u32);

        for op in &group {
            for change in text_diff.iter_changes(op) {
                let old_index = change.old_index().map(|i| i as u32 + 1);
                let new_index = change.new_index().map(|i| i as u32 + 1);
                if let Some(o) = old_index {
                    old_start = old_start.min(o);
                    old_count += 1;
                }
                if let Some(n) = new_index {
                    new_start = new_start.min(n);
                    new_count += 1;
                }
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Delete => LineKind::Deletion,
                    ChangeTag::Insert => LineKind::Addition,
                };
                let text = change.value().trim_end_matches('\n').to_string();
                lines.push(DiffLine { kind, old_line: old_index, new_line: new_index, text });
            }
        }

        let old_start = if old_start == u32::MAX { 0 } else { old_start };
        let new_start = if new_start == u32::MAX { 0 } else { new_start };
        hunks.push(Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            section: None,
            lines,
        });
    }

    DiffFile {
        old_path: old_path.to_string(),
        new_path: new_path.to_string(),
        hunks,
        is_binary: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_change() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let f = diff_texts("foo", "foo", old, new, 3);
        assert_eq!(f.hunks.len(), 1);
        let kinds: Vec<_> = f.hunks[0].lines.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&LineKind::Deletion));
        assert!(kinds.contains(&LineKind::Addition));
    }

    #[test]
    fn identical_has_no_hunks() {
        let f = diff_texts("foo", "foo", "x\ny\n", "x\ny\n", 3);
        assert!(f.hunks.is_empty());
    }
}
