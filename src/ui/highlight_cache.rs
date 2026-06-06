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

/// How many files on each side of the focused file to pre-highlight in the
/// background, so jumping to an adjacent file hits a warm cache instead of
/// paying syntect's per-line parse on the first frame after the switch.
const PREFETCH_RADIUS: usize = 2;

/// How many files' worth of highlights to keep cached at once. Sized to hold
/// the focused file plus the full prefetch window on both sides, so neighbor
/// prefetches aren't immediately evicted.
const KEEP_FILES: usize = 2 * PREFETCH_RADIUS + 1;

/// Build the prefetch order for a focused file: the focused file first (it's
/// what the render thread needs now), then alternating nearer neighbors
/// outward (`+1, -1, +2, -2, …`) so the most likely next jump warms soonest.
fn prefetch_order(focus: usize, n: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(KEEP_FILES);
    if focus < n {
        order.push(focus);
    }
    for d in 1..=PREFETCH_RADIUS {
        if focus + d < n {
            order.push(focus + d);
        }
        if focus >= d && focus - d < n {
            order.push(focus - d);
        }
    }
    order
}

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
/// Jobs are focus file indices: collecting/sanitizing the lines happens here,
/// off the render thread. The worker always tracks the freshest job — if the
/// user switches files while one is still warming, the newer request preempts
/// the current one so we never burn cycles highlighting a file nobody views.
///
/// For each focus it warms the file itself first, then its neighbors (see
/// [`prefetch_order`]), so the common next/prev-file jump lands on an already
/// highlighted file and renders its first frame from cache.
fn spawn_warm_worker(cache: SharedCache, changeset: Arc<Changeset>) -> Sender<usize> {
    let (tx, rx) = mpsc::channel::<usize>();
    let n = changeset.files.len();
    std::thread::spawn(move || {
        let hl = Highlighter::new();
        let mut pending: Option<usize> = None;
        'jobs: loop {
            // Take the freshest queued focus (collapsing any backlog).
            let mut focus = match pending.take() {
                Some(i) => i,
                None => match rx.recv() {
                    Ok(i) => i,
                    Err(_) => return, // app dropped the sender; exit.
                },
            };
            while let Ok(newer) = rx.try_recv() {
                focus = newer;
            }
            // Warm the focused file, then prefetch neighbors outward. A newer
            // focus preempts at any point (between lines or files).
            for file_idx in prefetch_order(focus, n) {
                let Some(file) = changeset.files.get(file_idx) else {
                    continue;
                };
                let syntax = hl.syntax_for(file.display_path());
                for line in file.hunks.iter().flat_map(|h| h.lines.iter()) {
                    // A newer job means this work is stale: stash and restart.
                    match rx.try_recv() {
                        Ok(newer) => {
                            pending = Some(newer);
                            continue 'jobs;
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
                    // Highlight outside the lock (it's the expensive part),
                    // then insert without clobbering an entry the render thread
                    // may have added in the meantime.
                    let runs = Arc::new(hl.line(syntax, &text));
                    lock_cache(&cache)
                        .entry(file_idx)
                        .or_default()
                        .entry(text)
                        .or_insert(runs);
                }
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
        self.evict_outside_prefetch_window(file_idx);
        // If the worker has gone away the render path still highlights lazily.
        let _ = self.tx.send(file_idx);
    }

    /// Evict cached highlights for files outside the current prefetch window,
    /// so the shared cache stays bounded as the user moves through the
    /// changeset. The retained set mirrors exactly what the worker warms for
    /// `file_idx` (the focused file plus its neighbors), so prefetched
    /// neighbors survive instead of being evicted right after the worker fills
    /// them.
    fn evict_outside_prefetch_window(&mut self, file_idx: usize) {
        let keep = prefetch_order(file_idx, self.changeset.files.len());
        lock_cache(&self.cache).retain(|fi, _| keep.contains(fi));
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
    fn prefetch_order_is_focus_then_outward_neighbors() {
        // Interior file: focus first, then +1, -1, +2, -2 within bounds.
        assert_eq!(prefetch_order(5, 10), vec![5, 6, 4, 7, 3]);
        // Clamped at the edges: out-of-range neighbors are dropped.
        assert_eq!(prefetch_order(0, 10), vec![0, 1, 2]);
        assert_eq!(prefetch_order(9, 10), vec![9, 8, 7]);
        // Out-of-range focus yields nothing.
        assert!(prefetch_order(9, 0).is_empty());
    }

    #[test]
    fn cache_is_bounded_by_prefetch_window() {
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
