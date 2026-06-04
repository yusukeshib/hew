//! Turn CLI inputs into a normalized [`Changeset`].

use crate::comments::model::CommentStore;
use crate::diff::{model::Changeset, parse};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

/// Read a unified patch from `path` (or stdin when `None`/`-`) and parse it.
pub fn load_patch(path: Option<&Path>) -> Result<Changeset> {
    let text = read_patch(path)?;
    Ok(parse::parse_unified(&text))
}

/// Load a sidecar comments JSON file into a [`CommentStore`].
///
/// Accepts either `{ "threads": [...] }` or a bare `[ ...threads... ]` array.
pub fn load_comments(path: &Path) -> Result<CommentStore> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading comments file {}", path.display()))?;
    // Try the store shape first, then a bare thread array.
    if let Ok(store) = serde_json::from_str::<CommentStore>(&text) {
        return Ok(store);
    }
    let threads = serde_json::from_str(&text)
        .with_context(|| format!("parsing comments JSON {}", path.display()))?;
    Ok(CommentStore { threads })
}

/// Load comments when the file exists, or start from an empty store when it
/// doesn't. `--comments <file>` names the review document hew opens *and*
/// saves to, so a missing file just means "start a fresh review here".
pub fn load_comments_or_default(path: &Path) -> Result<CommentStore> {
    if path.exists() {
        load_comments(path)
    } else {
        Ok(CommentStore::default())
    }
}

/// Flush the in-memory review store to `path` as canonical pretty JSON
/// (`{ "threads": [...] }`). This is the save half of the `--comments`
/// round-trip.
pub fn save_comments(path: &Path, store: &CommentStore) -> Result<()> {
    let json = serde_json::to_string_pretty(store).context("serializing review comments")?;
    std::fs::write(path, json)
        .with_context(|| format!("writing comments file {}", path.display()))?;
    Ok(())
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
        let dir = std::env::temp_dir();
        let path = dir.join("hew_test_comments.json");
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
    fn save_then_load_roundtrips() {
        use crate::comments::model::{Comment, LineRange, Thread};
        use crate::diff::model::Side;

        let store = CommentStore {
            threads: vec![Thread {
                id: uuid::Uuid::new_v4(),
                file: "src/main.rs".into(),
                side: Side::New,
                range: LineRange { start: 3, end: 5 },
                resolved: true,
                comments: vec![Comment {
                    id: uuid::Uuid::new_v4(),
                    author: Some("agent".into()),
                    body: "looks good".into(),
                    created_at: std::time::SystemTime::now(),
                }],
            }],
        };
        let path = std::env::temp_dir().join("hew_test_roundtrip.json");
        save_comments(&path, &store).unwrap();
        let loaded = load_comments_or_default(&path).unwrap();
        assert_eq!(loaded.threads.len(), 1);
        let t = &loaded.threads[0];
        assert_eq!(t.range.start, 3);
        assert_eq!(t.range.end, 5);
        assert!(t.resolved);
        assert_eq!(t.comments.len(), 1);
        assert_eq!(t.comments[0].body, "looks good");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_or_default_is_empty_when_missing() {
        let path = std::env::temp_dir().join("hew_test_does_not_exist_xyz.json");
        let _ = std::fs::remove_file(&path);
        let store = load_comments_or_default(&path).unwrap();
        assert!(store.threads.is_empty());
    }
}
