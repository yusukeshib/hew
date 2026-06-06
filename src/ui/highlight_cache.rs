//! Background-warmed, bounded syntax-highlight cache.
//!
//! Highlighting one diff line costs syntect a per-line parse (hundreds of µs).
//! To keep scrolling smooth we cache `(file, line text) -> colored runs` and
//! front-run the viewport with a background worker that highlights the whole
//! file off the render thread. The cache is bounded to a few recently-viewed
//! files so it can't grow without limit on a large changeset.

use crate::diff::model::Changeset;
use crate::ui::highlight::Highlighter;
use crate::ui::render_rows::sanitize_line;
use crate::ui::theme::theme;
use ratatui::style::Color;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

/// Highlighted runs for one line: `(fg color, text)`. `Arc` (not `Rc`) so the
/// background pre-warm worker can share entries with the render thread.
pub type LineRuns = Arc<Vec<(Color, String)>>;
/// Per-file map from a line's exact text to its highlighted runs.
type FileCache = HashMap<String, LineRuns>;
/// Highlight cache shared between the render thread and the warm worker, keyed
/// `file_idx -> (line text -> runs)`. The nested shape lets the hot `runs`
/// lookup hit by `&str` (no per-frame key allocation) and makes per-file
/// eviction a single outer-key removal.
type SharedCache = Arc<Mutex<HashMap<usize, FileCache>>>;

/// How many files' worth of highlights to keep cached at once.
const KEEP_FILES: usize = 3;

/// Lock the highlight cache, recovering from a poisoned mutex instead of
/// panicking. The cached data is just colorized spans — a worker panic mid-
/// insert can't leave it logically corrupt — so a poisoned lock should never
/// be allowed to crash the render thread.
fn lock_cache(
    cache: &Mutex<HashMap<usize, FileCache>>,
) -> MutexGuard<'_, HashMap<usize, FileCache>> {
    cache.lock().unwrap_or_else(|e| e.into_inner())
}

/// Spawn the background highlighter. It owns its own `Highlighter`, reads line
/// text straight from the shared `changeset`, and fills `cache` ahead of the
/// viewport so scrolling hits warm entries instead of paying syntect's per-
/// line parse cost mid-scroll.
///
/// Jobs are just file indices: collecting/sanitizing the lines happens here,
/// off the render thread. The worker always tracks the freshest job — if the
/// user switches files while one is still warming, the newer request preempts
/// the current one so we never burn cycles highlighting a file nobody views.
fn spawn_warm_worker(cache: SharedCache, changeset: Arc<Changeset>) -> Sender<usize> {
    let (tx, rx) = mpsc::channel::<usize>();
    std::thread::spawn(move || {
        let hl = Highlighter::new();
        let mut pending: Option<usize> = None;
        loop {
            // Take the freshest queued job (collapsing any backlog).
            let mut file_idx = match pending.take() {
                Some(i) => i,
                None => match rx.recv() {
                    Ok(i) => i,
                    Err(_) => return, // app dropped the sender; exit.
                },
            };
            while let Ok(newer) = rx.try_recv() {
                file_idx = newer;
            }
            let Some(file) = changeset.files.get(file_idx) else {
                continue;
            };
            let syntax = hl.syntax_for(file.display_path());
            'lines: for line in file.hunks.iter().flat_map(|h| h.lines.iter()) {
                // A newer job means this file is stale: stash and restart.
                match rx.try_recv() {
                    Ok(newer) => {
                        pending = Some(newer);
                        break 'lines;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                let text = sanitize_line(&line.text);
                if lock_cache(&cache)
                    .get(&file_idx)
                    .is_some_and(|m| m.contains_key(&text))
                {
                    continue;
                }
                // Highlight outside the lock (it's the expensive part), then
                // insert without clobbering an entry the render thread may have
                // added in the meantime.
                let runs = Arc::new(hl.line(syntax, &text));
                lock_cache(&cache)
                    .entry(file_idx)
                    .or_default()
                    .entry(text)
                    .or_insert(runs);
            }
        }
    });
    tx
}

/// Owns the highlight cache, the synchronous fallback highlighter, and the
/// background warm worker. The render thread calls [`Self::runs`] on the hot
/// path; [`Self::warm`] is called once per file switch to front-run scrolling.
pub struct HighlightCache {
    changeset: Arc<Changeset>,
    highlighter: Highlighter,
    cache: SharedCache,
    tx: Sender<usize>,
    /// Last file we asked the worker to pre-highlight (so we only enqueue on
    /// change, not every frame).
    warmed_file: Option<usize>,
    /// Most-recently-viewed files (most recent last), used to bound the cache:
    /// entries for files outside this window are evicted.
    warmed_recent: Vec<usize>,
}

impl HighlightCache {
    pub fn new(changeset: Arc<Changeset>) -> Self {
        let cache: SharedCache = Arc::new(Mutex::new(HashMap::new()));
        let tx = spawn_warm_worker(cache.clone(), changeset.clone());
        HighlightCache {
            changeset,
            highlighter: Highlighter::new(),
            cache,
            tx,
            warmed_file: None,
            warmed_recent: Vec::new(),
        }
    }

    /// Highlighted `(color, text)` runs for a line, cached per (file, text).
    ///
    /// On a miss we still highlight synchronously so the current frame is
    /// correct; the background worker just front-runs us so misses are rare
    /// during scroll.
    pub fn runs(&self, file_idx: usize, text: &str) -> LineRuns {
        // Hot-path lookup hits by `&str` (HashMap's `Borrow<str>`), so a cached
        // line costs no allocation — only a miss builds an owned key.
        if let Some(v) = lock_cache(&self.cache)
            .get(&file_idx)
            .and_then(|m| m.get(text))
        {
            return v.clone();
        }
        let spans = match self.changeset.files.get(file_idx) {
            Some(f) => {
                let syntax = self.highlighter.syntax_for(f.display_path());
                self.highlighter.line(syntax, text)
            }
            None => vec![(theme().text, text.to_string())],
        };
        let rc = Arc::new(spans);
        // `or_insert` keeps any entry the warm worker added while we computed,
        // so both threads converge on a single canonical `Arc`.
        lock_cache(&self.cache)
            .entry(file_idx)
            .or_default()
            .entry(text.to_string())
            .or_insert(rc)
            .clone()
    }

    /// Ask the background worker to pre-highlight `file_idx`, unless we already
    /// requested it. Cheap no-op when the file is unchanged.
    pub fn warm(&mut self, file_idx: usize) {
        if self.warmed_file == Some(file_idx) {
            return;
        }
        self.warmed_file = Some(file_idx);
        self.touch_recent(file_idx);
        // If the worker has gone away the render path still highlights lazily.
        let _ = self.tx.send(file_idx);
    }

    /// Record `file_idx` as most-recently-viewed and evict cached highlights
    /// for files that have fallen outside the retained window, so the shared
    /// cache stays bounded regardless of how many files are visited.
    fn touch_recent(&mut self, file_idx: usize) {
        self.warmed_recent.retain(|&f| f != file_idx);
        self.warmed_recent.push(file_idx);
        if self.warmed_recent.len() > KEEP_FILES {
            self.warmed_recent.remove(0);
            let keep = self.warmed_recent.clone();
            lock_cache(&self.cache).retain(|fi, _| keep.contains(fi));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::model::{Changeset, DiffFile, DiffLine, Hunk, LineKind};

    fn file_with(lines: &[&str]) -> DiffFile {
        DiffFile {
            old_path: "a.rs".into(),
            new_path: "a.rs".into(),
            is_binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_count: lines.len() as u32,
                new_start: 1,
                new_count: lines.len() as u32,
                section: None,
                lines: lines
                    .iter()
                    .map(|t| DiffLine {
                        kind: LineKind::Context,
                        old_line: Some(1),
                        new_line: Some(1),
                        text: (*t).to_string(),
                    })
                    .collect(),
            }],
        }
    }

    fn cs(n_files: usize) -> Arc<Changeset> {
        Arc::new(Changeset {
            files: (0..n_files).map(|_| file_with(&["let x = 1;"])).collect(),
        })
    }

    #[test]
    fn runs_are_cached_and_stable() {
        let hc = HighlightCache::new(cs(1));
        let a = hc.runs(0, "let x = 1;");
        let b = hc.runs(0, "let x = 1;");
        // Second call returns the same cached Arc (no re-highlight).
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!a.is_empty());
    }

    #[test]
    fn missing_file_falls_back_to_plain() {
        let hc = HighlightCache::new(cs(1));
        let runs = hc.runs(99, "anything");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, "anything");
    }

    #[test]
    fn touch_recent_bounds_the_cache() {
        let mut hc = HighlightCache::new(cs(10));
        // Populate + mark many files as viewed; only KEEP_FILES survive.
        for fi in 0..10 {
            hc.runs(fi, "let x = 1;");
            hc.warm(fi);
        }
        let len = lock_cache(&hc.cache).len();
        assert!(len <= KEEP_FILES, "cache should be bounded, got {len}");
    }
}
