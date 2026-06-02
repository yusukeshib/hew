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
