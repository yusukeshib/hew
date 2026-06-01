//! Parse a unified diff (git-style) into the normalized [`Changeset`] model.
//!
//! Handles the subset emitted by `git diff` / `git show` and most tools:
//! `diff --git`, `---`/`+++` headers, `@@ -a,b +c,d @@` hunk headers,
//! ` `/`+`/`-` line prefixes, `Binary files ... differ`, and
//! `\ No newline at end of file`.

use crate::diff::model::{Changeset, DiffFile, DiffLine, Hunk, LineKind};

pub fn parse_unified(input: &str) -> Changeset {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut lines = input.lines().peekable();

    while let Some(&line) = lines.peek() {
        if line.starts_with("diff --git ") || line.starts_with("--- ") {
            let file = parse_file(&mut lines);
            if let Some(file) = file {
                files.push(file);
            }
        } else {
            lines.next();
        }
    }

    Changeset { files }
}

fn strip_path_prefix(raw: &str) -> String {
    // Strip a/ or b/ prefix and surrounding quotes/whitespace.
    let raw = raw.trim();
    let raw = raw.strip_prefix("a/").or_else(|| raw.strip_prefix("b/")).unwrap_or(raw);
    raw.to_string()
}

fn parse_file<'a, I>(lines: &mut std::iter::Peekable<I>) -> Option<DiffFile>
where
    I: Iterator<Item = &'a str>,
{
    let mut old_path = String::new();
    let mut new_path = String::new();
    let mut is_binary = false;
    let mut hunks: Vec<Hunk> = Vec::new();

    // Consume the `diff --git a/x b/y` line if present (gives fallback paths).
    if let Some(&l) = lines.peek() {
        if l.starts_with("diff --git ") {
            let rest = &l["diff --git ".len()..];
            if let Some((a, b)) = split_git_paths(rest) {
                old_path = strip_path_prefix(&a);
                new_path = strip_path_prefix(&b);
            }
            lines.next();
        }
    }

    // Header lines until the first hunk or the next file.
    while let Some(&l) = lines.peek() {
        if l.starts_with("@@") || l.starts_with("diff --git ") {
            break;
        }
        if let Some(p) = l.strip_prefix("--- ") {
            old_path = strip_path_prefix(p);
        } else if let Some(p) = l.strip_prefix("+++ ") {
            new_path = strip_path_prefix(p);
        } else if l.starts_with("Binary files ") || l.starts_with("GIT binary patch") {
            is_binary = true;
        }
        lines.next();
    }

    // Hunks.
    while let Some(&l) = lines.peek() {
        if l.starts_with("diff --git ") || l.starts_with("--- ") {
            break;
        }
        if l.starts_with("@@") {
            if let Some(hunk) = parse_hunk(lines) {
                hunks.push(hunk);
            } else {
                lines.next();
            }
        } else {
            lines.next();
        }
    }

    if old_path.is_empty() && new_path.is_empty() && hunks.is_empty() && !is_binary {
        return None;
    }
    Some(DiffFile { old_path, new_path, hunks, is_binary })
}

/// Split the `a/x b/y` part of a `diff --git` line, tolerating spaces in names.
fn split_git_paths(rest: &str) -> Option<(String, String)> {
    // Common simple case: no spaces in names.
    let parts: Vec<&str> = rest.split(' ').collect();
    if parts.len() == 2 {
        return Some((parts[0].to_string(), parts[1].to_string()));
    }
    // Fallback: split in the middle on the " b/" marker.
    if let Some(idx) = rest.find(" b/") {
        let (a, b) = rest.split_at(idx);
        return Some((a.to_string(), b[1..].to_string()));
    }
    None
}

fn parse_hunk<'a, I>(lines: &mut std::iter::Peekable<I>) -> Option<Hunk>
where
    I: Iterator<Item = &'a str>,
{
    let header = lines.next()?; // the `@@ ... @@` line
    let (old_start, old_count, new_start, new_count, section) = parse_hunk_header(header)?;

    let mut diff_lines = Vec::new();
    let mut old_no = old_start;
    let mut new_no = new_start;

    while let Some(&l) = lines.peek() {
        if l.starts_with("@@") || l.starts_with("diff --git ") || l.starts_with("--- ") {
            break;
        }
        if l.starts_with('\\') {
            // "\ No newline at end of file" — metadata, skip.
            lines.next();
            continue;
        }
        let (kind, text) = match l.chars().next() {
            Some('+') => (LineKind::Addition, &l[1..]),
            Some('-') => (LineKind::Deletion, &l[1..]),
            Some(' ') => (LineKind::Context, &l[1..]),
            None => (LineKind::Context, ""), // empty context line
            _ => break,                       // unknown prefix → end of hunk
        };
        let (old_line, new_line) = match kind {
            LineKind::Context => {
                let o = old_no;
                let n = new_no;
                old_no += 1;
                new_no += 1;
                (Some(o), Some(n))
            }
            LineKind::Deletion => {
                let o = old_no;
                old_no += 1;
                (Some(o), None)
            }
            LineKind::Addition => {
                let n = new_no;
                new_no += 1;
                (None, Some(n))
            }
        };
        diff_lines.push(DiffLine { kind, old_line, new_line, text: text.to_string() });
        lines.next();
    }

    Some(Hunk { old_start, old_count, new_start, new_count, section, lines: diff_lines })
}

/// Parse `@@ -old_start,old_count +new_start,new_count @@ section`.
fn parse_hunk_header(header: &str) -> Option<(u32, u32, u32, u32, Option<String>)> {
    let rest = header.strip_prefix("@@")?;
    let end = rest.find("@@")?;
    let ranges = rest[..end].trim();
    let section = rest[end + 2..].trim();
    let section = if section.is_empty() { None } else { Some(section.to_string()) };

    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count, section))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_diff() {
        let input = "\
diff --git a/foo.txt b/foo.txt
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
diff --git a/new.txt b/new.txt
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
    fn detects_binary() {
        let input = "\
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";
        let cs = parse_unified(input);
        assert_eq!(cs.files.len(), 1);
        assert!(cs.files[0].is_binary);
    }
}
