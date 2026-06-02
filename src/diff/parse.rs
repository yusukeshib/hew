//! Parse a unified diff into the normalized [`Changeset`] model.
//!
//! Backed by the mature `patch` crate (handles `@@` headers, line prefixes,
//! `\ No newline at end of file`, rename/mode metadata, etc.). We only adapt
//! its AST into our internal representation.

use crate::diff::model::{Changeset, DiffFile, DiffLine, Hunk, LineKind};

/// Parse a unified diff. Returns an empty changeset if the text is not a
/// recognizable patch (e.g. an empty diff).
pub fn parse_unified(input: &str) -> Changeset {
    match patch::Patch::from_multiple(input) {
        Ok(patches) => Changeset {
            files: patches.iter().map(convert).collect(),
        },
        Err(_) => Changeset::default(),
    }
}

fn strip_prefix(path: &str) -> String {
    let p = path.trim();
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p).to_string()
}

fn convert(p: &patch::Patch) -> DiffFile {
    let old_path = strip_prefix(&p.old.path);
    let new_path = strip_prefix(&p.new.path);
    let hunks = p.hunks.iter().map(convert_hunk).collect();
    DiffFile { old_path, new_path, hunks, is_binary: false }
}

fn convert_hunk(h: &patch::Hunk) -> Hunk {
    let old_start = h.old_range.start as u32;
    let new_start = h.new_range.start as u32;
    let mut old_no = old_start;
    let mut new_no = new_start;
    let mut lines = Vec::with_capacity(h.lines.len());

    for line in &h.lines {
        let (kind, text, old_line, new_line) = match line {
            patch::Line::Context(t) => {
                let (o, n) = (old_no, new_no);
                old_no += 1;
                new_no += 1;
                (LineKind::Context, *t, Some(o), Some(n))
            }
            patch::Line::Remove(t) => {
                let o = old_no;
                old_no += 1;
                (LineKind::Deletion, *t, Some(o), None)
            }
            patch::Line::Add(t) => {
                let n = new_no;
                new_no += 1;
                (LineKind::Addition, *t, None, Some(n))
            }
        };
        lines.push(DiffLine { kind, old_line, new_line, text: text.to_string() });
    }

    let section = {
        let hint = h.range_hint.trim();
        if hint.is_empty() { None } else { Some(hint.to_string()) }
    };

    Hunk {
        old_start,
        old_count: h.old_range.count as u32,
        new_start,
        new_count: h.new_range.count as u32,
        section,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_diff() {
        let input = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@ fn main
 a
-b
+B
 c
";
        let cs = parse_unified(input);
        assert_eq!(cs.files.len(), 1);
        let f = &cs.files[0];
        assert_eq!(f.display_path(), "foo.txt");
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(h.section.as_deref(), Some("fn main"));
        assert_eq!(h.lines.len(), 4);
        assert_eq!(h.lines[1].kind, LineKind::Deletion);
        assert_eq!(h.lines[1].old_line, Some(2));
        assert_eq!(h.lines[2].kind, LineKind::Addition);
        assert_eq!(h.lines[2].new_line, Some(2));
    }

    #[test]
    fn parses_added_file() {
        let input = "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let cs = parse_unified(input);
        assert_eq!(cs.files.len(), 1);
        assert_eq!(cs.files[0].display_path(), "new.txt");
        assert_eq!(cs.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(parse_unified("").is_empty());
    }
}
