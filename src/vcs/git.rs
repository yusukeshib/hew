//! Git access via `git2` (libgit2): structured diffs, no subprocess, no text parsing.

use crate::diff::model::{Changeset, DiffFile, DiffLine, Hunk, LineKind};
use anyhow::{Context, Result};
use git2::{Diff, DiffOptions, Repository};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

const CONTEXT_LINES: u32 = 3;

fn open(path: &Path) -> Result<Repository> {
    Repository::discover(path).with_context(|| format!("opening git repo at {}", path.display()))
}

fn diff_opts() -> DiffOptions {
    let mut opts = DiffOptions::new();
    opts.context_lines(CONTEXT_LINES);
    opts
}

/// Diff of the working tree (staged + unstaged tracked changes) against HEAD.
pub fn working_tree_changeset(path: &Path) -> Result<Changeset> {
    let repo = open(path)?;
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = diff_opts();
    let diff = repo
        .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))
        .context("computing working-tree diff")?;
    build_changeset(&diff)
}

/// Diff for a commit (defaults handled by the caller; `rev` is any revspec).
pub fn show_changeset(path: &Path, rev: &str) -> Result<Changeset> {
    let repo = open(path)?;
    let obj = repo
        .revparse_single(rev)
        .with_context(|| format!("resolving rev {rev}"))?;
    let commit = obj.peel_to_commit().with_context(|| format!("{rev} is not a commit"))?;
    let tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let mut opts = diff_opts();
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .context("computing commit diff")?;
    build_changeset(&diff)
}

/// Walk a libgit2 `Diff` into our normalized [`Changeset`].
fn build_changeset(diff: &Diff) -> Result<Changeset> {
    let files: Rc<RefCell<Vec<DiffFile>>> = Rc::new(RefCell::new(Vec::new()));

    let f_files = files.clone();
    let mut file_cb = move |delta: git2::DiffDelta, _progress: f32| -> bool {
        let old_path = path_of(delta.old_file().path());
        let new_path = path_of(delta.new_file().path());
        f_files.borrow_mut().push(DiffFile {
            old_path,
            new_path,
            hunks: Vec::new(),
            is_binary: false,
        });
        true
    };

    let b_files = files.clone();
    let mut binary_cb = move |_delta: git2::DiffDelta, _binary: git2::DiffBinary| -> bool {
        if let Some(f) = b_files.borrow_mut().last_mut() {
            f.is_binary = true;
        }
        true
    };

    let h_files = files.clone();
    let mut hunk_cb = move |_delta: git2::DiffDelta, hunk: git2::DiffHunk| -> bool {
        if let Some(f) = h_files.borrow_mut().last_mut() {
            f.hunks.push(Hunk {
                old_start: hunk.old_start(),
                old_count: hunk.old_lines(),
                new_start: hunk.new_start(),
                new_count: hunk.new_lines(),
                section: section_of(hunk.header()),
                lines: Vec::new(),
            });
        }
        true
    };

    let l_files = files.clone();
    let mut line_cb =
        move |_delta: git2::DiffDelta, _hunk: Option<git2::DiffHunk>, line: git2::DiffLine| -> bool {
            let kind = match line.origin() {
                '+' => LineKind::Addition,
                '-' => LineKind::Deletion,
                ' ' => LineKind::Context,
                _ => return true, // file/hunk headers, eofnl markers, etc.
            };
            let text = String::from_utf8_lossy(line.content())
                .trim_end_matches('\n')
                .to_string();
            let mut files = l_files.borrow_mut();
            if let Some(f) = files.last_mut() {
                if let Some(h) = f.hunks.last_mut() {
                    h.lines.push(DiffLine {
                        kind,
                        old_line: line.old_lineno(),
                        new_line: line.new_lineno(),
                        text,
                    });
                }
            }
            true
        };

    diff.foreach(&mut file_cb, Some(&mut binary_cb), Some(&mut hunk_cb), Some(&mut line_cb))
        .context("walking diff")?;

    // Drop the callbacks so their Rc clones are released, then take the data.
    drop((file_cb, binary_cb, hunk_cb, line_cb));
    let files = std::mem::take(&mut *files.borrow_mut());
    Ok(Changeset { files })
}

fn path_of(p: Option<&Path>) -> String {
    p.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/dev/null".into())
}

/// Extract the section heading after the second `@@` in a hunk header.
fn section_of(header: &[u8]) -> Option<String> {
    let h = String::from_utf8_lossy(header);
    let rest = h.strip_prefix("@@")?;
    let end = rest.find("@@")?;
    let section = rest[end + 2..].trim();
    if section.is_empty() {
        None
    } else {
        Some(section.to_string())
    }
}
