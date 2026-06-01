//! Git access via subprocess (plan §5: correctness first, optimize later).

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("failed to spawn git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Resolve the repository root for `path`.
pub fn repo_root(path: &Path) -> Result<std::path::PathBuf> {
    let out = run_git(path, &["rev-parse", "--show-toplevel"])?;
    Ok(std::path::PathBuf::from(out.trim()))
}

/// Unified diff of the working tree (staged + unstaged) against HEAD.
pub fn working_tree_diff(repo: &Path) -> Result<String> {
    // HEAD diff covers staged+unstaged tracked changes.
    run_git(repo, &["--no-pager", "diff", "--no-color", "HEAD"])
}

/// Unified diff for a commit (defaults to HEAD).
pub fn show(repo: &Path, rev: &str) -> Result<String> {
    run_git(
        repo,
        &["--no-pager", "show", "--no-color", "--format=", rev],
    )
}
