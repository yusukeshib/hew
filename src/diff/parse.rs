//! Parse a unified diff into the normalized [`Changeset`] model.
//!
//! Backed by the mature `patch` crate (handles `@@` headers, line prefixes,
//! `\ No newline at end of file`, rename/mode metadata, etc.). We only adapt
//! its AST into our internal representation.

use crate::diff::model::{Changeset, DiffFile, DiffLine, Hunk, LineKind};

/// Parse a unified diff into a [`Changeset`], also reporting a human-readable
/// error when the text *looked* like a unified diff yet failed to parse.
///
/// The `patch` crate handles text hunks but silently skips binary entries
/// (`Binary files a/x and b/x differ`), so we scan for those separately and
/// append them as hunk-less binary files (rendered as a `Binary file` marker
/// rather than disappearing).
///
/// The `patch` crate returns `Err` even for empty/non-patch input, so a bare
/// error is not a reliable signal: a clean `git diff` with no changes would
/// trip it. We therefore only surface an error when (a) parsing failed, (b) no
/// binary-file markers were recovered, and (c) the input contains a patch-like
/// marker (`--- `, `diff --git`, or an `@@` hunk header). That keeps a genuinely
/// empty diff silent while flagging a malformed one instead of showing nothing.
pub fn parse_report(input: &str) -> (Changeset, Option<String>) {
    let (text_files, parse_err) = match patch::Patch::from_multiple(input) {
        Ok(patches) => (patches.iter().map(convert).collect::<Vec<_>>(), None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let mut files = text_files;
    let binaries = binary_files(input);
    let had_binaries = !binaries.is_empty();
    files.extend(binaries);

    let error = parse_err.filter(|_| !had_binaries && looks_like_patch(input));
    (Changeset { files }, error)
}

/// Heuristic: does `input` contain a line that marks it as a unified diff?
fn looks_like_patch(input: &str) -> bool {
    input
        .lines()
        .any(|l| l.starts_with("--- ") || l.starts_with("diff --git ") || l.starts_with("@@"))
}

/// Scan for git's `Binary files a/x and b/y differ` markers and turn each into
/// a hunk-less binary [`DiffFile`]. A `/dev/null` side encodes an add/delete.
fn binary_files(input: &str) -> Vec<DiffFile> {
    input
        .lines()
        .filter_map(|line| {
            let rest = line
                .trim()
                .strip_prefix("Binary files ")?
                .strip_suffix(" differ")?;
            let (a, b) = rest.split_once(" and ")?;
            Some(DiffFile {
                old_path: strip_prefix(a),
                new_path: strip_prefix(b),
                hunks: Vec::new(),
                is_binary: true,
            })
        })
        .collect()
}

fn strip_prefix(path: &str) -> String {
    let p = path.trim();
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

fn convert(p: &patch::Patch) -> DiffFile {
    let old_path = strip_prefix(&p.old.path);
    let new_path = strip_prefix(&p.new.path);
    let hunks = p.hunks.iter().map(convert_hunk).collect();
    DiffFile {
        old_path,
        new_path,
        hunks,
        is_binary: false,
    }
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
        lines.push(DiffLine {
            kind,
            old_line,
            new_line,
            text: text.to_string(),
        });
    }

    let section = {
        let hint = h.range_hint.trim();
        if hint.is_empty() {
            None
        } else {
            Some(hint.to_string())
        }
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
        let cs = parse_report(input).0;
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
        let cs = parse_report(input).0;
        assert_eq!(cs.files.len(), 1);
        assert_eq!(cs.files[0].display_path(), "new.txt");
        assert_eq!(cs.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(parse_report("").0.is_empty());
    }

    #[test]
    fn empty_and_nonpatch_input_report_no_error() {
        // A clean `git diff` (no changes) and arbitrary non-patch text must
        // stay silent — no spurious "failed to parse" warning.
        assert!(parse_report("").1.is_none());
        assert!(parse_report("   \n\n").1.is_none());
        assert!(parse_report("hello world\nnot a patch\n").1.is_none());
    }

    #[test]
    fn malformed_patch_reports_error() {
        // Looks like a diff (`---`/`@@`) but the hunk is bogus → surfaced.
        let (cs, err) = parse_report("--- a/x\n+++ b/x\n@@ bogus @@\n+y\n");
        assert!(cs.is_empty());
        assert!(err.is_some(), "malformed patch should report an error");
    }

    #[test]
    fn valid_patch_reports_no_error() {
        let (cs, err) = parse_report("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(cs.files.len(), 1);
        assert!(err.is_none());
    }

    #[test]
    fn detects_binary_files_alongside_text() {
        let input = "\
diff --git a/logo.png b/logo.png
index e69de29..d95f3ad 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-old
+new
 ctx
";
        let cs = parse_report(input).0;
        // One text file (with a hunk) + one binary file (hunk-less).
        assert_eq!(cs.files.len(), 2);
        let bin = cs
            .files
            .iter()
            .find(|f| f.is_binary)
            .expect("binary file detected");
        assert_eq!(bin.display_path(), "logo.png");
        assert!(bin.hunks.is_empty());
        let text = cs.files.iter().find(|f| !f.is_binary).unwrap();
        assert_eq!(text.hunks.len(), 1);
    }

    #[test]
    fn binary_add_and_delete_resolve_paths() {
        let added = parse_report("Binary files /dev/null and b/new.bin differ\n").0;
        assert_eq!(added.files.len(), 1);
        assert_eq!(added.files[0].display_path(), "new.bin");

        let deleted = parse_report("Binary files a/old.bin and /dev/null differ\n").0;
        assert_eq!(deleted.files.len(), 1);
        assert_eq!(deleted.files[0].display_path(), "old.bin");
    }
}
