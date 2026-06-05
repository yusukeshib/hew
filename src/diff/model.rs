//! Normalized diff representation produced by the unified-patch parser.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

/// The kind of a single diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

/// One line inside a hunk. `old_line`/`new_line` are 1-based line numbers on
/// each side, or `None` when the line does not exist on that side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

/// A contiguous block of changes (a `@@ ... @@` hunk).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Optional section heading captured from the hunk header (after `@@`).
    pub section: Option<String>,
    pub lines: Vec<DiffLine>,
}

/// All hunks for a single file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    /// Path on the old side (may equal `new_path`).
    pub old_path: String,
    /// Path on the new side.
    pub new_path: String,
    pub hunks: Vec<Hunk>,
    pub is_binary: bool,
}

impl DiffFile {
    /// Display path: prefers the new path, falling back to the old.
    pub fn display_path(&self) -> &str {
        if self.new_path != "/dev/null" && !self.new_path.is_empty() {
            &self.new_path
        } else {
            &self.old_path
        }
    }
}

/// A full changeset: many files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changeset {
    pub files: Vec<DiffFile>,
}

impl Changeset {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}
