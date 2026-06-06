//! Compute the session's output: the minimal set of review *actions* that turn
//! the immutable input store (`--comments`) into the final in-memory store.
//!
//! hew never writes back to its input. Instead, on exit it emits a compacted
//! action log to stdout — a delta a consumer (a GitHub bridge, the next agent
//! session, an audit) can replay against the same base. Compaction falls out of
//! diffing: a thread created and deleted in one session is in neither base nor
//! final, so it produces no action; a resolve toggled back to its original
//! state likewise cancels.

use super::model::{CommentStore, Thread};
use crate::diff::model::Side;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;

/// One review action in the session output log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// A new thread's root comment, anchored to a diff line range. `line` is the
    /// thread's last line (GitHub's anchor); `start_line` is present only for a
    /// multi-line range, matching GitHub's `start_line`/`line` review-comment
    /// shape. A single-line thread omits `start_line` (then `line` is that line).
    AddComment {
        thread_id: Uuid,
        file: PathBuf,
        side: Side,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_line: Option<u32>,
        line: u32,
        body: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// A comment appended to an existing (or just-added) thread.
    Reply {
        thread_id: Uuid,
        body: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// Marked a thread resolved.
    Resolve { thread_id: Uuid },
    /// Marked a thread unresolved.
    Unresolve { thread_id: Uuid },
}

/// Emit the actions for a thread that is new relative to the base.
fn added_thread_actions(t: &Thread, out: &mut Vec<Action>) {
    let mut comments = t.comments.iter();
    if let Some(root) = comments.next() {
        let start_line = (t.range.start != t.range.end).then_some(t.range.start);
        out.push(Action::AddComment {
            thread_id: t.id,
            file: t.file.clone(),
            side: t.side,
            start_line,
            line: t.range.end,
            body: root.body.clone(),
            author: root.author.clone(),
        });
    }
    for c in comments {
        out.push(Action::Reply {
            thread_id: t.id,
            body: c.body.clone(),
            author: c.author.clone(),
        });
    }
    if t.resolved {
        out.push(Action::Resolve { thread_id: t.id });
    }
}

/// The minimal action log transforming `base` into `cur`.
///
/// Threads are matched by `Thread.id`. For the log to be **replayable by an
/// external consumer against the base file**, that base must carry stable thread
/// ids: actions reference the ids hew saw at load. A base sidecar that omits
/// `id` gets fresh random ids at load time (see `model`'s serde defaults), so
/// its `resolve`/`reply` actions won't match anything in the on-disk base.
/// Producers that care about replay (e.g. a GitHub bridge) must write stable
/// ids; ad-hoc viewing without replay is unaffected.
///
/// Note there is no delete action: base threads are immutable in the UI (only
/// in-session threads can be removed, and those cancel out of the diff), so a
/// base thread is never absent from `cur`.
pub fn diff(base: &CommentStore, cur: &CommentStore) -> Vec<Action> {
    let base_by_id: HashMap<Uuid, &Thread> = base.threads.iter().map(|t| (t.id, t)).collect();
    let mut out = Vec::new();

    for t in &cur.threads {
        match base_by_id.get(&t.id) {
            None => added_thread_actions(t, &mut out),
            Some(base_t) => {
                // New replies, identified by comment id.
                let base_cids: HashSet<Uuid> = base_t.comments.iter().map(|c| c.id).collect();
                for c in t.comments.iter().filter(|c| !base_cids.contains(&c.id)) {
                    out.push(Action::Reply {
                        thread_id: t.id,
                        body: c.body.clone(),
                        author: c.author.clone(),
                    });
                }
                // Net resolved-state change only.
                if t.resolved != base_t.resolved {
                    out.push(if t.resolved {
                        Action::Resolve { thread_id: t.id }
                    } else {
                        Action::Unresolve { thread_id: t.id }
                    });
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::model::LineRange;

    fn store() -> CommentStore {
        CommentStore::default()
    }

    #[test]
    fn added_thread_with_reply() {
        let base = store();
        let mut cur = store();
        let id = cur.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange { start: 5, end: 5 },
            Some("you".into()),
            "root".into(),
        );
        cur.reply(id, Some("you".into()), "more".into());

        let actions = diff(&base, &cur);
        assert_eq!(actions.len(), 2);
        assert!(matches!(
            &actions[0],
            Action::AddComment {
                start_line: None,
                line: 5,
                ..
            }
        ));
        assert!(matches!(&actions[1], Action::Reply { .. }));
    }

    #[test]
    fn multi_line_add_carries_start_line() {
        let base = store();
        let mut cur = store();
        cur.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange { start: 10, end: 14 },
            None,
            "spans".into(),
        );
        let actions = diff(&base, &cur);
        assert!(matches!(
            &actions[0],
            Action::AddComment {
                start_line: Some(10),
                line: 14,
                ..
            }
        ));
    }

    #[test]
    fn add_then_delete_cancels() {
        let base = store();
        let mut cur = store();
        let id = cur.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange { start: 1, end: 1 },
            None,
            "x".into(),
        );
        // Deleting the lone comment empties (and drops) the in-session thread,
        // so the add and delete cancel to an empty log.
        let cid = cur.threads[0].comments[0].id;
        cur.remove_comment(id, cid);
        assert!(diff(&base, &cur).is_empty());
    }

    #[test]
    fn resolve_toggle_cancels_but_single_resolve_shows() {
        let mut base = store();
        let id = base.add_thread(
            "a.rs".into(),
            Side::Old,
            LineRange { start: 2, end: 2 },
            None,
            "x".into(),
        );

        // Toggle resolve twice => no net change.
        let mut cur = base.clone();
        cur.toggle_resolved(id);
        cur.toggle_resolved(id);
        assert!(diff(&base, &cur).is_empty());

        // Resolve once => one Resolve action.
        let mut cur = base.clone();
        cur.toggle_resolved(id);
        let actions = diff(&base, &cur);
        assert_eq!(actions, vec![Action::Resolve { thread_id: id }]);
    }

    #[test]
    fn reply_to_base_thread() {
        let mut base = store();
        let id = base.add_thread(
            "a.rs".into(),
            Side::New,
            LineRange { start: 4, end: 4 },
            None,
            "root".into(),
        );
        let mut cur = base.clone();
        cur.reply(id, Some("you".into()), "reply".into());
        let actions = diff(&base, &cur);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Reply { body, .. } if body == "reply"));
    }
}
