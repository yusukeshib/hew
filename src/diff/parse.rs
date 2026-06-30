//! Parse a unified diff into the normalized [`Changeset`] model.
//!
//! Backed by the mature `patch` crate for text hunks (`@@` headers, line
//! prefixes, `\ No newline at end of file`, etc.). We supplement it with a
//! small scan for hunk-less git metadata entries (pure renames, mode-only
//! changes, empty-file adds/deletes) so they do not disappear from the TUI.

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
    let metadata = metadata_only_files(input);
    let recovered = !binaries.is_empty() || !metadata.is_empty();
    files.extend(binaries);
    extend_missing(&mut files, metadata);

    let error = parse_err.filter(|_| !recovered && looks_like_patch(input));
    (Changeset { files }, error)
}

/// Heuristic: does `input` contain a line that marks it as a unified diff?
fn looks_like_patch(input: &str) -> bool {
    // `@@ -` is the real unified-diff hunk-header prefix; a bare `@@` check
    // would false-positive on non-diff text that merely starts with `@@`
    // (email quotes, logs), since `patch::from_multiple` errors on any
    // non-patch input.
    input
        .lines()
        .any(|l| l.starts_with("--- ") || l.starts_with("diff --git ") || l.starts_with("@@ -"))
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

fn metadata_only_files(input: &str) -> Vec<DiffFile> {
    input
        .split("\ndiff --git ")
        .filter_map(|block| {
            let block = block.strip_prefix("diff --git ").unwrap_or(block);
            parse_git_block(block)
        })
        .filter(|file| !file.old_path.is_empty() || !file.new_path.is_empty())
        .collect()
}

fn parse_git_block(block: &str) -> Option<DiffFile> {
    let mut lines = block.lines();
    let header = lines.next()?.trim();
    let (mut old_path, mut new_path) = parse_git_header(header)?;
    let mut has_hunks = false;
    let mut has_binary = false;
    let mut new_file = false;
    let mut deleted_file = false;

    for line in lines {
        if line.starts_with("@@ -") || line.starts_with("--- ") || line.starts_with("+++ ") {
            has_hunks = true;
        } else if line.trim().starts_with("Binary files ") {
            has_binary = true;
        } else if line.starts_with("rename from ") || line.starts_with("copy from ") {
            old_path = strip_prefix(line.split_once(' ')?.1.trim_start_matches("from "));
        } else if line.starts_with("rename to ") || line.starts_with("copy to ") {
            new_path = strip_prefix(line.split_once(' ')?.1.trim_start_matches("to "));
        } else if line.starts_with("new file mode ") {
            new_file = true;
        } else if line.starts_with("deleted file mode ") {
            deleted_file = true;
        }
    }

    if has_hunks || has_binary {
        return None;
    }
    if new_file {
        old_path = "/dev/null".into();
    } else if deleted_file {
        new_path = "/dev/null".into();
    }
    Some(DiffFile {
        old_path,
        new_path,
        hunks: Vec::new(),
        is_binary: false,
    })
}

fn parse_git_header(header: &str) -> Option<(String, String)> {
    let paths = parse_header_paths(header);
    let old = *paths.first()?;
    let new = *paths.get(1)?;
    if !old.starts_with("a/") || !new.starts_with("b/") {
        return None;
    }
    Some((strip_prefix(old), strip_prefix(new)))
}

fn parse_header_paths(header: &str) -> Vec<&str> {
    header.split_whitespace().take(2).collect()
}

fn extend_missing(files: &mut Vec<DiffFile>, candidates: Vec<DiffFile>) {
    for candidate in candidates {
        if !files.iter().any(|file| same_file(file, &candidate)) {
            files.push(candidate);
        }
    }
}

fn same_file(a: &DiffFile, b: &DiffFile) -> bool {
    a.old_path == b.old_path && a.new_path == b.new_path
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

    #[test]
    fn pure_rename_is_not_dropped() {
        let input = "\
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
";
        let (cs, err) = parse_report(input);
        assert!(err.is_none());
        assert_eq!(cs.files.len(), 1);
        let f = &cs.files[0];
        assert_eq!(f.old_path, "old.txt");
        assert_eq!(f.new_path, "new.txt");
        assert!(f.hunks.is_empty());
        assert!(!f.is_binary);
    }

    #[test]
    fn mode_only_change_is_not_dropped() {
        let input = "\
diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
";
        let (cs, err) = parse_report(input);
        assert!(err.is_none());
        assert_eq!(cs.files.len(), 1);
        assert_eq!(cs.files[0].display_path(), "script.sh");
        assert!(cs.files[0].hunks.is_empty());
    }

    #[test]
    fn content_rename_is_not_duplicated() {
        let input = "\
diff --git a/old.txt b/new.txt
similarity index 88%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1 +1 @@
-old
+new
";
        let cs = parse_report(input).0;
        assert_eq!(cs.files.len(), 1);
        assert_eq!(cs.files[0].old_path, "old.txt");
        assert_eq!(cs.files[0].new_path, "new.txt");
        assert_eq!(cs.files[0].hunks.len(), 1);
    }
}
