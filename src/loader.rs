//! Turn CLI inputs into a normalized [`Changeset`].

use crate::comments::model::CommentStore;
use crate::diff::{generate, model::Changeset, parse};
use crate::vcs::git;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

const CONTEXT_LINES: usize = 3;

/// What the user asked to review.
#[derive(Debug, Clone)]
pub enum Source {
    /// `hew diff` with no args → working tree, or two files.
    WorkingTree { repo: PathBuf },
    TwoFiles { old: PathBuf, new: PathBuf },
    /// `hew show [rev]`.
    Show { repo: PathBuf, rev: String },
    /// `hew patch <file|->`.
    Patch { path: Option<PathBuf> },
}

impl Source {
    /// A short human label for the title bar.
    pub fn label(&self) -> String {
        match self {
            Source::WorkingTree { .. } => "diff (working tree)".into(),
            Source::TwoFiles { old, new } => {
                format!("{} → {}", old.display(), new.display())
            }
            Source::Show { rev, .. } => format!("show {rev}"),
            Source::Patch { path } => match path {
                Some(p) => format!("patch {}", p.display()),
                None => "patch -".into(),
            },
        }
    }
}

pub fn load(source: &Source) -> Result<Changeset> {
    match source {
        Source::WorkingTree { repo } => git::working_tree_changeset(repo),
        Source::Show { repo, rev } => git::show_changeset(repo, rev),
        Source::Patch { path } => {
            let text = read_patch(path.as_deref())?;
            Ok(parse::parse_unified(&text))
        }
        Source::TwoFiles { old, new } => {
            let old_text = std::fs::read_to_string(old)
                .with_context(|| format!("reading {}", old.display()))?;
            let new_text = std::fs::read_to_string(new)
                .with_context(|| format!("reading {}", new.display()))?;
            let file = generate::diff_texts(
                &old.to_string_lossy(),
                &new.to_string_lossy(),
                &old_text,
                &new_text,
                CONTEXT_LINES,
            );
            Ok(Changeset { files: vec![file] })
        }
    }
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
