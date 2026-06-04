//! PR-style review comments, loaded from a sidecar JSON file and edited in
//! place. The store is the single in-memory source of truth: the TUI (and,
//! the in-app composer) mutate it through the methods on
//! [`CommentStore`]; on exit it is diffed against the immutable base into an
//! action log (see [`super::diff`]).

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

impl CommentStore {
    /// Start a new thread anchored to `(file, side, range)` with a root
    /// comment, returning its thread id. This is the single write path used by
    /// the TUI composer.
    pub fn add_thread(
        &mut self,
        file: PathBuf,
        side: Side,
        range: LineRange,
        author: Option<String>,
        body: String,
    ) -> Uuid {
        let id = Uuid::new_v4();
        self.threads.push(Thread {
            id,
            file,
            side,
            range,
            resolved: false,
            comments: vec![Comment {
                id: Uuid::new_v4(),
                author,
                body,
                created_at: SystemTime::now(),
            }],
        });
        id
    }

    /// Append a reply to the thread with `thread_id`. Returns `false` when no
    /// such thread exists.
    pub fn reply(&mut self, thread_id: Uuid, author: Option<String>, body: String) -> bool {
        match self.threads.iter_mut().find(|t| t.id == thread_id) {
            Some(t) => {
                t.comments.push(Comment {
                    id: Uuid::new_v4(),
                    author,
                    body,
                    created_at: SystemTime::now(),
                });
                true
            }
            None => false,
        }
    }

    /// Remove the thread with `id`. Returns `true` when one was removed.
    pub fn remove_thread(&mut self, id: Uuid) -> bool {
        let before = self.threads.len();
        self.threads.retain(|t| t.id != id);
        self.threads.len() != before
    }

    /// Flip the resolved flag on the thread with `id`, returning the new state
    /// (or `None` when no such thread exists).
    pub fn toggle_resolved(&mut self, id: Uuid) -> Option<bool> {
        let t = self.threads.iter_mut().find(|t| t.id == id)?;
        t.resolved = !t.resolved;
        Some(t.resolved)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u32, end: u32) -> LineRange {
        LineRange { start, end }
    }

    #[test]
    fn add_thread_then_reply() {
        let mut store = CommentStore::default();
        let id = store.add_thread(
            "src/main.rs".into(),
            Side::New,
            range(10, 12),
            Some("agent".into()),
            "looks off".into(),
        );
        assert_eq!(store.threads.len(), 1);
        assert_eq!(store.threads[0].comments.len(), 1);
        assert!(!store.threads[0].resolved);

        assert!(store.reply(id, Some("you".into()), "good catch".into()));
        assert_eq!(store.threads[0].comments.len(), 2);
        assert_eq!(store.threads[0].comments[1].body, "good catch");

        // Replying to an unknown thread is a no-op.
        assert!(!store.reply(Uuid::new_v4(), None, "x".into()));
        assert_eq!(store.threads[0].comments.len(), 2);
    }

    #[test]
    fn toggle_resolved_flips_and_reports_missing() {
        let mut store = CommentStore::default();
        let id = store.add_thread("f".into(), Side::Old, range(1, 1), None, "hi".into());
        assert_eq!(store.toggle_resolved(id), Some(true));
        assert!(store.threads[0].resolved);
        assert_eq!(store.toggle_resolved(id), Some(false));
        assert!(!store.threads[0].resolved);
        // Unknown ids report failure without panicking.
        assert_eq!(store.toggle_resolved(Uuid::new_v4()), None);
    }

    #[test]
    fn remove_thread_reports_hit() {
        let mut store = CommentStore::default();
        let id = store.add_thread("f".into(), Side::New, range(2, 2), None, "hi".into());
        assert!(store.remove_thread(id));
        assert!(store.threads.is_empty());
        // Removing again is a no-op.
        assert!(!store.remove_thread(id));
    }
}
