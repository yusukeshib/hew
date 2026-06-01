//! Turn CLI inputs into a normalized [`Changeset`].

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
        Source::WorkingTree { repo } => {
            let root = git::repo_root(repo).unwrap_or_else(|_| repo.clone());
            let text = git::working_tree_diff(&root)?;
            Ok(parse::parse_unified(&text))
        }
        Source::Show { repo, rev } => {
            let root = git::repo_root(repo).unwrap_or_else(|_| repo.clone());
            let text = git::show(&root, rev)?;
            Ok(parse::parse_unified(&text))
        }
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
