//! Read-only PR-style review comments, loaded from a sidecar JSON file.
//! hew never mutates these in-app: to change them, edit the JSON (and reload).

use crate::diff::model::Side;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn contains(&self, line: u32) -> bool {
        line >= self.start && line <= self.end
    }
}

/// A single message in a thread (root or reply).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    #[serde(default)]
    pub author: Option<String>,
    pub body: String,
    // Sidecar JSON may omit this; default to "now".
    #[serde(with = "ts", default = "SystemTime::now")]
    pub created_at: SystemTime,
}

/// A review thread anchored to a line range. `comments[0]` is the root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub file: PathBuf,
    pub side: Side,
    pub range: LineRange,
    #[serde(default)]
    pub resolved: bool,
    pub comments: Vec<Comment>,
}

/// Owns every thread loaded from the sidecar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommentStore {
    pub threads: Vec<Thread>,
}

/// Serialize `SystemTime` as a unix-millis integer.
mod ts {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let ms = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        s.serialize_u64(ms)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_millis(ms))
    }
}
