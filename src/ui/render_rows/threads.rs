//! Comment-thread and composer line layout (text -> visual box lines).
//!
//! Pure layout helpers shared by the unified and split row builders: expanding
//! threads/composers into wrapped box lines and indexing threads by path/anchor.

use super::text::{fmt_date, sanitize_line, wrap_preserve, wrap_text};
use crate::comments::model::{CommentStore, Thread};
use crate::diff::model::{Changeset, Side};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One visual line of an inline-expanded comment thread.
#[derive(Debug, Clone)]
pub enum CommentKind {
    /// Top rounded border of the thread box.
    Top,
    /// Thread header: message count (resolved state lives on `CommentLine`).
    Head { replies: usize },
    /// A message author line (`@name`) with its formatted date.
    Author { name: String, date: String },
    /// A (pre-wrapped) body line.
    Body(String),
    /// Blank spacer between messages.
    Gap,
    /// The action-button row (reply / resolve / delete) above the bottom border.
    Actions,
    /// Bottom rounded border of the thread box.
    Bottom,
}

/// One visual line of a thread box, tagged with the thread's `resolved` state
/// so the renderer can dim the whole box (not just the header) when resolved,
/// plus identity so the line can be selected/focused. `comment_id` is the
/// owning message for content lines (`Author`/`Body`/`Gap`) and `None` for
/// thread chrome (`Top`/`Head`/`Bottom`) — selection keys off it, so a message
/// (author + its body lines) forms one selectable unit.
#[derive(Debug, Clone)]
pub struct CommentLine {
    pub kind: CommentKind,
    pub resolved: bool,
    pub thread_id: String,
    pub comment_id: Option<String>,
}

/// Where an open inline composer attaches in the row stream.
#[derive(Debug, Clone)]
pub enum ComposerAnchor {
    /// A new thread, anchored to the first line of the selected diff range.
    NewThread {
        file_idx: usize,
        side: Side,
        line: u32,
    },
    /// A reply, injected just below an existing thread's box.
    Reply { thread_id: String },
}

/// An open inline composer to inject into the row stream while typing.
#[derive(Debug, Clone)]
pub struct ComposerSpec {
    pub anchor: ComposerAnchor,
    pub title: String,
    pub body: String,
}

/// One visual line of the inline composer box.
#[derive(Debug, Clone)]
pub enum ComposerKind {
    /// Top rounded border, carrying the box title.
    Top { title: String },
    /// A (pre-wrapped) body line; the line at the cursor carries the caret glyph.
    Body(String),
    /// The one-line key hint above the bottom border.
    Hint,
    /// Bottom rounded border.
    Bottom,
}

#[derive(Debug, Clone)]
pub struct ComposerLine {
    pub kind: ComposerKind,
}

/// A row injected after a code line: an existing comment thread line, or a line
/// of the live composer box. Internal to row building — not part of the crate
/// API.
pub(super) enum Injected {
    Comment(CommentLine),
    Composer(ComposerLine),
}

/// Expand an open composer into wrapped visual lines (a rounded box with the
/// title, the live buffer + caret, and a key hint).
pub(super) fn composer_lines(spec: &ComposerSpec, width: usize) -> Vec<ComposerLine> {
    let mut out = vec![ComposerLine {
        kind: ComposerKind::Top {
            title: spec.title.clone(),
        },
    }];
    // `spec.body` already carries the caret glyph at the cursor position (see
    // `body_with_caret`); the glyph survives `sanitize_line` (not a control
    // char) and wraps with the surrounding text.
    for raw in spec.body.split('\n') {
        let s = sanitize_line(raw);
        if s.is_empty() {
            out.push(ComposerLine {
                kind: ComposerKind::Body(String::new()),
            });
        } else {
            for wl in wrap_preserve(&s, width) {
                out.push(ComposerLine {
                    kind: ComposerKind::Body(wl),
                });
            }
        }
    }
    out.push(ComposerLine {
        kind: ComposerKind::Hint,
    });
    out.push(ComposerLine {
        kind: ComposerKind::Bottom,
    });
    out
}

/// Composer lines for a *new* thread anchored exactly at `(file_idx, side,
/// line)`, emitted at most once per build (tracked via `emitted`).
pub(super) fn new_thread_composer(
    composer: Option<&ComposerSpec>,
    emitted: &mut bool,
    file_idx: usize,
    side: Side,
    line: u32,
    width: usize,
) -> Vec<ComposerLine> {
    if *emitted {
        return Vec::new();
    }
    if let Some(spec) = composer {
        if let ComposerAnchor::NewThread {
            file_idx: f,
            side: s,
            line: l,
        } = spec.anchor
        {
            if f == file_idx && s == side && l == line {
                *emitted = true;
                return composer_lines(spec, width);
            }
        }
    }
    Vec::new()
}

/// Expand a thread into wrapped visual lines.
pub fn thread_lines(t: &Thread, width: usize) -> Vec<CommentLine> {
    // Chrome lines (Top/Head/Bottom) carry no message id; content lines carry
    // their owning message's id so author + body + trailing gap select as one.
    let chrome = |kind: CommentKind| CommentLine {
        kind,
        resolved: t.resolved,
        thread_id: t.id.clone(),
        comment_id: None,
    };
    let content = |kind: CommentKind, cid: &str| CommentLine {
        kind,
        resolved: t.resolved,
        thread_id: t.id.clone(),
        comment_id: Some(cid.to_string()),
    };
    let mut out = vec![
        chrome(CommentKind::Top),
        chrome(CommentKind::Head {
            replies: t.comments.len(),
        }),
    ];
    for (i, c) in t.comments.iter().enumerate() {
        out.push(content(
            CommentKind::Author {
                name: c.author.clone().unwrap_or_else(|| "?".into()),
                date: fmt_date(c.created_at),
            },
            &c.id,
        ));
        for raw in c.body.split('\n') {
            let s = sanitize_line(raw);
            if s.is_empty() {
                out.push(content(CommentKind::Body(String::new()), &c.id));
            } else {
                for wl in wrap_text(&s, width) {
                    out.push(content(CommentKind::Body(wl), &c.id));
                }
            }
        }
        if i + 1 < t.comments.len() {
            out.push(content(CommentKind::Gap, &c.id));
        }
    }
    out.push(chrome(CommentKind::Actions));
    out.push(chrome(CommentKind::Bottom));
    out
}

/// Index thread positions by their anchored file path, so the per-line
/// injection scans only the (usually few) threads on the current file instead
/// of every thread in the changeset. Built once per row rebuild.
pub(super) type ThreadsByPath<'a> = std::collections::HashMap<&'a Path, Vec<usize>>;

pub(super) fn threads_by_path(comments: &CommentStore) -> ThreadsByPath<'_> {
    let mut map: ThreadsByPath<'_> = std::collections::HashMap::new();
    for (i, t) in comments.threads.iter().enumerate() {
        map.entry(t.file.as_path()).or_default().push(i);
    }
    map
}

/// Map each thread (across the whole changeset, keyed by thread id) to the
/// *last* `(side, line)` anchor within its range that is actually present in
/// the diff. A range comment renders after this line, so its box sits below the
/// last selected line (GitHub-style) rather than the first.
pub(super) fn last_anchor_lines(
    changeset: &Changeset,
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
) -> HashMap<String, (Side, u32)> {
    let mut m = HashMap::new();
    for file in &changeset.files {
        let Some(indices) = by_path.get(Path::new(file.display_path())) else {
            continue;
        };
        // Collect the diff's present line numbers per side once, in hunk order
        // (so each list is ascending), then binary-search each thread's range
        // instead of re-scanning every line of the file per thread.
        let mut old_lines: Vec<u32> = Vec::new();
        let mut new_lines: Vec<u32> = Vec::new();
        for line in file.hunks.iter().flat_map(|h| h.lines.iter()) {
            if let Some(l) = line.old_line {
                old_lines.push(l);
            }
            if let Some(l) = line.new_line {
                new_lines.push(l);
            }
        }
        for &i in indices {
            let t = &comments.threads[i];
            let lines = match t.side {
                Side::Old => &old_lines,
                Side::New => &new_lines,
            };
            // Largest present line <= range.end (the lists are ascending); it's
            // the thread's anchor when it also falls at/after range.start.
            let idx = lines.partition_point(|&l| l <= t.range.end);
            if idx > 0 && lines[idx - 1] >= t.range.start {
                m.insert(t.id.clone(), (t.side, lines[idx - 1]));
            }
        }
    }
    m
}

/// Inline-comment lines to inject after a code row, for every thread whose
/// last in-diff anchor line (see [`last_anchor_lines`]) is one of the row's
/// `(side, line)` anchors.
#[allow(clippy::too_many_arguments)]
pub(super) fn comment_rows_for(
    comments: &CommentStore,
    by_path: &ThreadsByPath<'_>,
    last: &HashMap<String, (Side, u32)>,
    emitted: &mut HashSet<String>,
    path: &str,
    anchors: &[(Side, u32)],
    width: usize,
    composer: Option<&ComposerSpec>,
) -> Vec<(Side, Injected)> {
    let mut out = Vec::new();
    let Some(indices) = by_path.get(Path::new(path)) else {
        return out;
    };
    for &i in indices {
        let t = &comments.threads[i];
        // Emit each thread once, at the last line of its range present in the
        // diff. `emitted` guards against a repeated emit.
        if emitted.contains(&t.id) {
            continue;
        }
        if last
            .get(&t.id)
            .is_some_and(|&(ts, tl)| anchors.iter().any(|&(s, l)| s == ts && l == tl))
        {
            emitted.insert(t.id.clone());
            out.extend(
                thread_lines(t, width)
                    .into_iter()
                    .map(|cl| (t.side, Injected::Comment(cl))),
            );
            // A reply composer sits directly under the thread it replies to.
            if let Some(spec) = composer {
                if matches!(&spec.anchor, ComposerAnchor::Reply { thread_id } if *thread_id == t.id)
                {
                    out.extend(
                        composer_lines(spec, width)
                            .into_iter()
                            .map(|cl| (t.side, Injected::Composer(cl))),
                    );
                }
            }
        }
    }
    out
}
