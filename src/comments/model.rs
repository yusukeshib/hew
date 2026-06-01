//! In-memory PR-style review comments. No persistence (plan Non-Goals).

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

#[allow(dead_code)] // single() used in tests + session server
impl LineRange {
    pub fn single(line: u32) -> Self {
        LineRange { start: line, end: line }
    }
    pub fn contains(&self, line: u32) -> bool {
        line >= self.start && line <= self.end
    }
}

/// A single message in a thread (root or reply).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: Uuid,
    pub author: Option<String>,
    pub body: String,
    #[serde(with = "ts")]
    pub created_at: SystemTime,
}

impl Comment {
    pub fn new(author: Option<String>, body: String) -> Self {
        Comment {
            id: Uuid::new_v4(),
            author,
            body,
            created_at: SystemTime::now(),
        }
    }
}

/// A review thread anchored to a line range. `comments[0]` is the root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    pub file: PathBuf,
    pub side: Side,
    pub range: LineRange,
    pub resolved: bool,
    pub comments: Vec<Comment>,
}

#[allow(dead_code)] // accessors used by the session server (milestone 3)
impl Thread {
    pub fn root(&self) -> Option<&Comment> {
        self.comments.first()
    }
    /// The line the thread is anchored at (range end, where the marker shows).
    pub fn anchor_line(&self) -> u32 {
        self.range.end
    }
}

/// Owns every thread for the session. Single source of truth (plan §3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommentStore {
    pub threads: Vec<Thread>,
}

// Several methods are exercised only by the session server (milestone 3).
#[allow(dead_code)]
impl CommentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new thread and return its id.
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
            comments: vec![Comment::new(author, body)],
        });
        id
    }

    /// Append a reply to an existing thread. Returns false if not found.
    pub fn reply(&mut self, thread_id: Uuid, author: Option<String>, body: String) -> bool {
        match self.thread_mut(thread_id) {
            Some(t) => {
                t.comments.push(Comment::new(author, body));
                true
            }
            None => false,
        }
    }

    pub fn set_resolved(&mut self, thread_id: Uuid, resolved: bool) -> bool {
        match self.thread_mut(thread_id) {
            Some(t) => {
                t.resolved = resolved;
                true
            }
            None => false,
        }
    }

    /// Edit a single comment's body (root or reply). Returns false if not found.
    pub fn edit(&mut self, comment_id: Uuid, body: String) -> bool {
        for t in &mut self.threads {
            for c in &mut t.comments {
                if c.id == comment_id {
                    c.body = body;
                    return true;
                }
            }
        }
        false
    }

    /// Remove a whole thread. Returns false if not found.
    pub fn remove_thread(&mut self, thread_id: Uuid) -> bool {
        let before = self.threads.len();
        self.threads.retain(|t| t.id != thread_id);
        self.threads.len() != before
    }

    pub fn thread_mut(&mut self, id: Uuid) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }

    /// Threads anchored to a given file + side + line, in creation order.
    pub fn threads_at(&self, file: &std::path::Path, side: Side, line: u32) -> Vec<&Thread> {
        self.threads
            .iter()
            .filter(|t| t.file == file && t.side == side && t.range.contains(line))
            .collect()
    }

    /// Anchor lines (file-agnostic) for next/prev navigation, sorted.
    pub fn count(&self) -> usize {
        self.threads.len()
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

    #[test]
    fn thread_lifecycle() {
        let mut store = CommentStore::new();
        let id = store.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange::single(10),
            Some("agent".into()),
            "looks off".into(),
        );
        assert_eq!(store.count(), 1);
        assert!(store.reply(id, None, "fixed".into()));
        assert_eq!(store.thread_mut(id).unwrap().comments.len(), 2);
        assert!(store.set_resolved(id, true));
        assert!(store.thread_mut(id).unwrap().resolved);
        assert!(store.remove_thread(id));
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn threads_at_matches_range() {
        let mut store = CommentStore::new();
        store.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange { start: 5, end: 8 },
            None,
            "range".into(),
        );
        assert_eq!(store.threads_at(std::path::Path::new("a.rs"), Side::New, 6).len(), 1);
        assert_eq!(store.threads_at(std::path::Path::new("a.rs"), Side::New, 9).len(), 0);
    }
}
