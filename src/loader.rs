//! Turn CLI inputs into a normalized [`Changeset`].

use crate::comments::model::CommentStore;
use crate::diff::{model::Changeset, parse};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

/// Read a unified patch from `path` (or stdin when `None`/`-`) and parse it.
///
/// A patch that *looks* like a unified diff but fails to parse is reported on
/// stderr (stdout stays reserved for the action log) so the user isn't left
/// staring at a silent "no changes to review". A genuinely empty diff stays
/// quiet.
pub fn load_patch(path: Option<&Path>) -> Result<Changeset> {
    let text = read_patch(path)?;
    let (changeset, parse_err) = parse::parse_report(&text);
    if let Some(err) = parse_err {
        eprintln!("hew: warning: input looks like a patch but failed to parse: {err}");
    }
    Ok(changeset)
}

/// Load a sidecar comments JSON file into a [`CommentStore`].
///
/// Accepts either `{ "threads": [...] }` or a bare `[ ...threads... ]` array.
/// The top-level JSON kind (object vs array) selects the shape, so an error
/// inside the *right* shape is reported directly instead of being masked by a
/// confusing "expected array" from a blind fallback parse.
pub fn load_comments(path: &Path) -> Result<CommentStore> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading comments file {}", path.display()))?;
    // Peek at the first non-whitespace byte: `[` means a bare thread array,
    // anything else is parsed as the `{ "threads": [...] }` store object.
    let is_array = text.trim_start().starts_with('[');
    if is_array {
        let threads = serde_json::from_str(&text)
            .with_context(|| format!("parsing comments JSON array {}", path.display()))?;
        Ok(CommentStore { threads })
    } else {
        serde_json::from_str::<CommentStore>(&text)
            .with_context(|| format!("parsing comments JSON {}", path.display()))
    }
}

/// Load comments when the file exists, or start from an empty store when it
/// doesn't. `--comments <file>` is an immutable input — the review's starting
/// point — so a missing file just means "start from an empty base".
pub fn load_comments_or_default(path: &Path) -> Result<CommentStore> {
    if path.exists() {
        load_comments(path)
    } else {
        Ok(CommentStore::default())
    }
}

fn read_patch(path: Option<&Path>) -> Result<String> {
    match path {
        Some(p) if p.as_os_str() != "-" => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))
        }
        _ => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading patch from stdin")?;
            Ok(buf)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp path so parallel tests (cargo runs them in one process)
    /// can't collide on a fixed filename and flake.
    fn unique_temp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("hew_test_{tag}_{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn loads_handwritten_comments() {
        let json = r#"{
          "threads": [
            {
              "file": "src/main.rs",
              "side": "new",
              "range": { "start": 10, "end": 12 },
              "comments": [
                { "author": "agent", "body": "this range looks off" },
                { "author": "you", "body": "good catch" }
              ]
            }
          ]
        }"#;
        let path = unique_temp("comments");
        std::fs::write(&path, json).unwrap();
        let store = load_comments(&path).unwrap();
        assert_eq!(store.threads.len(), 1);
        let t = &store.threads[0];
        assert_eq!(t.range.start, 10);
        assert_eq!(t.comments.len(), 2);
        assert!(!t.resolved); // default
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loads_bare_thread_array() {
        let json = r#"[
            { "file": "a.rs", "side": "new", "range": { "start": 1, "end": 1 },
              "comments": [ { "author": "x", "body": "hi" } ] }
        ]"#;
        let path = unique_temp("array");
        std::fs::write(&path, json).unwrap();
        let store = load_comments(&path).unwrap();
        assert_eq!(store.threads.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_shape_error_is_not_masked_by_array_fallback() {
        // Object shape with a bad `side` value: the error must mention the
        // store parse, not a misleading "expected array".
        let json = r#"{ "threads": [ { "file": "a.rs", "side": "sideways",
            "range": { "start": 1, "end": 1 }, "comments": [] } ] }"#;
        let path = unique_temp("bad_store");
        std::fs::write(&path, json).unwrap();
        let err = load_comments(&path).unwrap_err().to_string();
        assert!(err.contains("parsing comments JSON"), "got: {err}");
        assert!(!err.contains("array"), "should not mention array: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_or_default_is_empty_when_missing() {
        let path = unique_temp("missing");
        let _ = std::fs::remove_file(&path);
        let store = load_comments_or_default(&path).unwrap();
        assert!(store.threads.is_empty());
    }
}
