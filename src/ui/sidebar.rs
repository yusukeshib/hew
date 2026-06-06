//! Pure file-tree model for the sidebar: building the collapsible row list and
//! the per-file status/comment markers. All functions here are side-effect free
//! and unit-tested; the stateful navigation/rendering stays in `app`.

use crate::comments::model::CommentStore;
use crate::diff::model::{Changeset, DiffFile};
use crate::ui::theme::theme;
use ratatui::style::Color;
use std::collections::HashSet;
use std::path::Path;

/// A row in the file tree: a directory node (collapsible) or a file entry (by
/// file index). `depth` is the visual nesting level used for indentation.
pub enum SbRow {
    Dir {
        path: String,
        name: String,
        depth: usize,
    },
    File {
        idx: usize,
        depth: usize,
    },
}

/// One-letter change status for a file, with its accent color.
pub fn file_status(f: &DiffFile) -> (char, Color) {
    let added = f.old_path == "/dev/null" || f.old_path.is_empty();
    let deleted = f.new_path == "/dev/null" || f.new_path.is_empty();
    if added {
        ('A', theme().added)
    } else if deleted {
        ('D', theme().removed)
    } else if f.old_path != f.new_path {
        ('R', theme().accent)
    } else {
        ('M', theme().warn)
    }
}

/// The directory portion of a path (everything before the last `/`), or `""`.
pub fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// The final path segment (filename).
pub fn base_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Build the collapsible file tree (keeping file order). Directory segments
/// become `Dir` nodes; files nest under them. Subtrees under a collapsed
/// directory are omitted. Returns the rows plus a `file_idx -> row` map
/// (`usize::MAX` for files hidden by a collapse).
pub fn build_sidebar_rows(
    changeset: &Changeset,
    collapsed: &HashSet<String>,
) -> (Vec<SbRow>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut map = vec![usize::MAX; changeset.files.len()];
    let mut prev: Vec<String> = Vec::new();
    for (i, f) in changeset.files.iter().enumerate() {
        let dir = dir_of(f.display_path());
        let segs: Vec<String> = if dir.is_empty() {
            Vec::new()
        } else {
            dir.split('/').map(|s| s.to_string()).collect()
        };
        // Longest directory prefix shared with the previous file (already open).
        let mut common = 0;
        while common < segs.len() && common < prev.len() && segs[common] == prev[common] {
            common += 1;
        }
        // Emit any newly-entered directory segments, unless an ancestor is
        // collapsed (then the whole subtree is hidden).
        for d in common..segs.len() {
            let ancestor_collapsed = (0..d).any(|a| collapsed.contains(&segs[..=a].join("/")));
            if ancestor_collapsed {
                continue;
            }
            rows.push(SbRow::Dir {
                path: segs[..=d].join("/"),
                name: segs[d].clone(),
                depth: d,
            });
        }
        prev = segs.clone();
        // Hide the file when any ancestor dir is collapsed.
        let hidden = (0..segs.len()).any(|d| collapsed.contains(&segs[..=d].join("/")));
        if hidden {
            continue;
        }
        map[i] = rows.len();
        rows.push(SbRow::File {
            idx: i,
            depth: segs.len(),
        });
    }
    (rows, map)
}

/// Comment marker for a file: `Some(true)` when it has an unresolved thread,
/// `Some(false)` when it only has resolved threads, `None` when it has none.
pub fn file_comment_state(comments: &CommentStore, path: &str) -> Option<bool> {
    let p = Path::new(path);
    let mut any = false;
    let mut open = false;
    for t in comments.threads.iter().filter(|t| t.file == p) {
        any = true;
        if !t.resolved {
            open = true;
        }
    }
    any.then_some(open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::model::DiffFile;

    fn file(path: &str) -> DiffFile {
        DiffFile {
            old_path: path.into(),
            new_path: path.into(),
            is_binary: false,
            hunks: vec![],
        }
    }

    fn cs(paths: &[&str]) -> Changeset {
        Changeset {
            files: paths.iter().map(|p| file(p)).collect(),
        }
    }

    #[test]
    fn dir_and_base_split() {
        assert_eq!(dir_of("a/b/c.rs"), "a/b");
        assert_eq!(dir_of("top.rs"), "");
        assert_eq!(base_of("a/b/c.rs"), "c.rs");
        assert_eq!(base_of("top.rs"), "top.rs");
    }

    #[test]
    fn nests_dirs_and_shares_common_prefix() {
        let changeset = cs(&["src/a.rs", "src/b.rs", "src/ui/c.rs"]);
        let (rows, map) = build_sidebar_rows(&changeset, &HashSet::new());
        // src (dir), a.rs, b.rs, src/ui (dir), c.rs — the shared `src` prefix
        // is emitted once, not per file.
        let dirs: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SbRow::Dir { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(dirs, vec!["src", "src/ui"]);
        // Every file maps to a real row.
        assert!(map.iter().all(|&r| r != usize::MAX));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn collapsed_dir_hides_its_subtree() {
        let changeset = cs(&["src/a.rs", "src/ui/c.rs", "top.rs"]);
        let mut collapsed = HashSet::new();
        collapsed.insert("src".to_string());
        let (rows, map) = build_sidebar_rows(&changeset, &collapsed);
        // The `src` dir row stays, but nothing beneath it (a.rs, src/ui, c.rs).
        assert!(rows
            .iter()
            .any(|r| matches!(r, SbRow::Dir { path, .. } if path == "src")));
        assert!(!rows
            .iter()
            .any(|r| matches!(r, SbRow::Dir { path, .. } if path == "src/ui")));
        // Hidden files map to usize::MAX; the visible top-level file does not.
        assert_eq!(map[0], usize::MAX); // src/a.rs hidden
        assert_eq!(map[1], usize::MAX); // src/ui/c.rs hidden
        assert_ne!(map[2], usize::MAX); // top.rs visible
    }

    #[test]
    fn file_status_classifies_add_delete_rename_modify() {
        assert_eq!(file_status(&file("a.rs")).0, 'M');
        let mut added = file("a.rs");
        added.old_path = "/dev/null".into();
        assert_eq!(file_status(&added).0, 'A');
        let mut deleted = file("a.rs");
        deleted.new_path = "/dev/null".into();
        assert_eq!(file_status(&deleted).0, 'D');
        let renamed = DiffFile {
            old_path: "a.rs".into(),
            new_path: "b.rs".into(),
            is_binary: false,
            hunks: vec![],
        };
        assert_eq!(file_status(&renamed).0, 'R');
    }
}
