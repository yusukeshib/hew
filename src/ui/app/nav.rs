//! Selection, stops, file spans, and viewport navigation.

use super::*;

impl App {
    pub(super) fn first_selectable(&self) -> Option<usize> {
        let (s, e) = self.file_range();
        (s..e).find(|&i| self.is_stop_at(i))
    }

    pub(super) fn last_selectable(&self) -> Option<usize> {
        let (s, e) = self.file_range();
        (s..e).rev().find(|&i| self.is_stop_at(i))
    }

    /// Column of the draggable sidebar/diff divider, if the sidebar is shown.
    pub(super) fn divider_col(&self) -> Option<u16> {
        // The diff panel's left border (just past the sidebar) is the divider.
        (self.sidebar_area.width > 0).then(|| self.sidebar_area.x + self.sidebar_area.width)
    }

    /// Resize the sidebar so its divider sits at column `col`.
    pub(super) fn resize_to(&mut self, col: u16) {
        let total = self.sidebar_area.width + self.diff_area.width;
        let max = total.saturating_sub(MIN_DIFF).max(MIN_SIDEBAR);
        self.sidebar_width = col
            .saturating_sub(self.sidebar_area.x)
            .clamp(MIN_SIDEBAR, max);
    }

    /// Inclusive `[lo, hi]` row span of the current selection: the cursor line
    /// alone, or the cursor-to-anchor range when a drag/visual selection is
    /// active. The single source of truth for selection extent.
    pub(super) fn selection_bounds(&self) -> (usize, usize) {
        let anchor = self.sel_anchor.unwrap_or(self.selected);
        (anchor.min(self.selected), anchor.max(self.selected))
    }

    /// Whether row `idx` falls within the current selection. When the cursor is
    /// on a comment, the whole message (its contiguous rows) is the selection;
    /// otherwise it's the diff-line cursor/drag range.
    pub(super) fn in_selection(&self, idx: usize) -> bool {
        if let Some((lo, hi)) = self.comment_unit_span(self.selected) {
            return idx >= lo && idx <= hi;
        }
        let (lo, hi) = self.selection_bounds();
        idx >= lo && idx <= hi
    }

    /// A stable handle to the current selection that survives a row rebuild or
    /// a view switch: the focused comment's message id, else the diff-line
    /// anchor `(file, side, line)`.
    pub(super) fn sel_key(&self) -> Option<SelKey> {
        if let Some((_, cid)) = self.focused_comment() {
            return Some(SelKey::Comment(cid));
        }
        self.anchor_at(self.selected)
            .map(|(f, s, l)| SelKey::Line(f, s, l))
    }

    /// Re-find the row matching `key` in the (freshly rebuilt) active list.
    pub(super) fn find_sel_key(&self, key: &SelKey) -> Option<usize> {
        (0..self.active_len()).find(|&i| match key {
            SelKey::Line(f, s, l) => {
                self.is_selectable_at(i) && self.anchor_at(i) == Some((*f, *s, *l))
            }
            SelKey::Comment(cid) => {
                self.is_stop_at(i)
                    && self.comment_unit_at(i).map(|(_, c)| c).as_deref() == Some(cid.as_str())
            }
        })
    }

    // ---- active-list abstraction (unified vs split) ----

    pub(super) fn active_len(&self) -> usize {
        match self.view {
            View::Unified => self.rows.len(),
            View::Split => self.split_rows.len(),
        }
    }

    pub(super) fn is_selectable_at(&self, i: usize) -> bool {
        match self.view {
            View::Unified => self.rows.get(i).is_some_and(|r| r.is_selectable()),
            View::Split => self.split_rows.get(i).is_some_and(|r| r.is_selectable()),
        }
    }

    /// The comment-thread line at row `i`, in whichever view is active.
    pub(super) fn comment_at(&self, i: usize) -> Option<&CommentLine> {
        match self.view {
            View::Unified => match &self.rows.get(i)?.kind {
                RowKind::Comment(cl) => Some(cl),
                _ => None,
            },
            View::Split => match &self.split_rows.get(i)?.kind {
                SplitRowKind::Comment { line, .. } => Some(line),
                _ => None,
            },
        }
    }

    /// `(thread_id, comment_id)` of the message that row `i` belongs to, if it's
    /// a content line of a comment (author/body/gap). Chrome rows return `None`.
    pub(super) fn comment_unit_at(&self, i: usize) -> Option<(String, String)> {
        let cl = self.comment_at(i)?;
        Some((cl.thread_id.clone(), cl.comment_id.clone()?))
    }

    /// A "stop" is a place the cursor can land: a diff line, or the *first* row
    /// of a comment message (so a multi-line message is a single stop).
    pub(super) fn is_stop_at(&self, i: usize) -> bool {
        if self.is_selectable_at(i) {
            return true;
        }
        match self.comment_unit_at(i) {
            // First row of a message: the row above belongs to a different
            // message (or is chrome / a diff line).
            Some((_, cid)) => i == 0 || self.comment_unit_at(i - 1).map(|(_, c)| c) != Some(cid),
            None => false,
        }
    }

    /// Inclusive `[lo, hi]` rows of the comment message covering row `i`, if `i`
    /// is a comment content line. Used to highlight/scroll the whole message.
    pub(super) fn comment_unit_span(&self, i: usize) -> Option<(usize, usize)> {
        let (_, cid) = self.comment_unit_at(i)?;
        let same =
            |j: usize| self.comment_unit_at(j).map(|(_, c)| c).as_deref() == Some(cid.as_str());
        let mut lo = i;
        while lo > 0 && same(lo - 1) {
            lo -= 1;
        }
        let mut hi = i;
        let len = self.active_len();
        while hi + 1 < len && same(hi + 1) {
            hi += 1;
        }
        Some((lo, hi))
    }

    /// First stop at/beyond `from` scanning in `dir`, within the file.
    pub(super) fn nearest_stop(&self, from: usize, dir: isize) -> Option<usize> {
        let (start, end) = self.file_range();
        let mut i = from as isize;
        while i >= start as isize && (i as usize) < end {
            if self.is_stop_at(i as usize) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    /// Map a clicked/landed row to the stop it should select: itself if it's a
    /// stop, the message head if it's inside a message, else the nearest stop.
    pub(super) fn stop_for(&self, idx: usize) -> Option<usize> {
        if self.is_stop_at(idx) {
            return Some(idx);
        }
        if self.comment_unit_at(idx).is_some() {
            return self.comment_unit_span(idx).map(|(lo, _)| lo);
        }
        self.nearest_stop(idx, 1)
            .or_else(|| self.nearest_stop(idx, -1))
    }

    /// `(thread_id, comment_id)` the cursor is currently on, if it's a comment.
    pub(super) fn focused_comment(&self) -> Option<(String, String)> {
        self.comment_unit_at(self.selected)
    }

    /// The thread the cursor acts on: the focused comment's thread, else the
    /// thread anchored to the focused diff line.
    pub(super) fn focused_thread_id(&self) -> Option<String> {
        if let Some(cl) = self.comment_at(self.selected) {
            return Some(cl.thread_id.clone());
        }
        self.current_thread_id()
    }

    /// Move the cursor to the first row of `thread_id` (and switch to its
    /// file). Button clicks (reply/resolve/delete) act on the clicked box
    /// regardless of where the cursor sits, but the rebuild that follows
    /// re-anchors the viewport to `self.selected`. Without this the viewport
    /// snaps to the parked cursor — typically the file's top after a wheel
    /// scroll, since scrolling leaves the selection untouched. Anchoring to the
    /// acted-on thread keeps it on screen (and gives `ensure_composer_visible`
    /// the right `current_file` to find the reply box in).
    pub(super) fn select_thread(&mut self, thread_id: &str) {
        let len = self.active_len();
        let Some(row) = (0..len)
            .find(|&i| self.comment_at(i).map(|cl| cl.thread_id.as_str()) == Some(thread_id))
        else {
            return;
        };
        self.current_file = self.row_file_idx(row).unwrap_or(self.current_file);
        self.recompute_file_span();
        self.selected = self.stop_for(row).unwrap_or(row);
    }

    /// The file index a row belongs to (header rows included).
    pub(super) fn row_file_idx(&self, i: usize) -> Option<usize> {
        match self.view {
            View::Unified => self.rows.get(i).map(|r| r.file_idx),
            View::Split => self.split_rows.get(i).map(|r| r.file_idx),
        }
    }

    /// `[start, end)` row range of the current file in the active list. Files
    /// are contiguous, so this is a single slice. Cached (see `file_span`) and
    /// only recomputed when the file, view, or row lists change — it's read on
    /// every keystroke and several times per frame, so the O(rows) scan must
    /// not run on the hot path.
    pub(super) fn file_range(&self) -> (usize, usize) {
        self.file_span
    }

    /// Recompute the cached current-file row span. Call after any change to
    /// `current_file`, `view`, or the active row list, and before code that
    /// reads `file_range` (navigation, scrolling, rendering).
    pub(super) fn recompute_file_span(&mut self) {
        let len = self.active_len();
        self.file_span = self
            .file_spans
            .get(self.current_file)
            .copied()
            .unwrap_or((len, len));
    }

    /// After the active row list changes, recompute every file's span and the
    /// cached current-file span together. These two always move as a unit (a
    /// row-list edit invalidates both), so bundling them keeps the invariant in
    /// one place instead of relying on each call site to pair them correctly.
    pub(super) fn resync_file_spans(&mut self) {
        self.rebuild_file_spans();
        self.recompute_file_span();
    }

    /// Recompute the `[start, end)` row span of *every* file in the active row
    /// list in a single pass. Files emit contiguous row blocks (build order),
    /// so one sweep fills them all; call this whenever the active row list or
    /// view changes. Keeps per-file-switch span lookup O(1).
    pub(super) fn rebuild_file_spans(&mut self) {
        let n = self.changeset.files.len();
        let len = self.active_len();
        let mut spans = vec![(len, len); n];
        for i in 0..len {
            if let Some(fi) = self.row_file_idx(i) {
                if let Some(span) = spans.get_mut(fi) {
                    if span.0 == len {
                        span.0 = i;
                    }
                    span.1 = i + 1;
                }
            }
        }
        self.file_spans = spans;
    }

    /// Switch the diff pane to the next/prev file.
    pub(super) fn jump_file(&mut self, dir: isize) {
        let n = self.changeset.files.len();
        if n == 0 {
            return;
        }
        let target = (self.current_file as isize + dir).clamp(0, n as isize - 1) as usize;
        if target == self.current_file {
            return;
        }
        self.set_current_file(target);
    }

    /// Point the diff pane at `file`, resetting the cursor to its top.
    pub(super) fn set_current_file(&mut self, file: usize) {
        self.sel_anchor = None;
        self.current_file = file.min(self.changeset.files.len().saturating_sub(1));
        self.recompute_file_span();
        // A file in a collapsed directory has no visible row; open its ancestors.
        self.reveal_file_in_tree(self.current_file);
        self.sidebar_sel = self
            .file_to_sbrow
            .get(self.current_file)
            .copied()
            .filter(|&r| r != usize::MAX)
            .unwrap_or(0);
        self.reveal_sidebar();
        let (start, _) = self.file_range();
        self.scroll = start;
        self.selected = self.first_selectable().unwrap_or(start);
        self.ensure_visible();
    }

    /// `(file_idx, side, line)` anchor for the row at `i`, if it carries one.
    pub(super) fn anchor_at(&self, i: usize) -> Option<(usize, Side, u32)> {
        match self.view {
            View::Unified => {
                let r = self.rows.get(i)?;
                let (s, l) = r.anchor()?;
                Some((r.file_idx, s, l))
            }
            View::Split => {
                let r = self.split_rows.get(i)?;
                let (s, l) = r.anchor()?;
                Some((r.file_idx, s, l))
            }
        }
    }

    /// Toggle between unified and split, keeping the cursor on the same line
    /// (and preserving any multi-line visual selection across the switch).
    pub(super) fn toggle_view(&mut self) {
        let key = self.sel_key();
        // Remember the selection anchor by its line identity so a multi-line
        // visual/drag selection survives the layout switch instead of
        // collapsing to the cursor line. The anchor is always a diff line.
        let anchor_key = self
            .sel_anchor
            .and_then(|a| self.anchor_at(a))
            .map(|(f, s, l)| SelKey::Line(f, s, l));
        self.view = match self.view {
            View::Unified => View::Split,
            View::Split => View::Unified,
        };
        // The active list switched (unified/split spans differ). The target
        // view's row list may be stale from an edit made while it was inactive
        // (lazy build), so reconstruct it before anything reads it; then
        // recompute all file spans and the current one before first_selectable.
        self.ensure_active_view_built();
        self.resync_file_spans();
        self.geom.dirty = true;
        // Re-find the same line / comment message in the other layout.
        let target = key.as_ref().and_then(|k| self.find_sel_key(k));
        self.selected = target
            .or_else(|| self.first_selectable())
            .unwrap_or(0)
            .min(self.active_len().saturating_sub(1));
        // Remap the anchor into the new layout. If it can't be found (e.g. the
        // anchored line has no counterpart in this view), drop the selection
        // rather than leave a stale row index dangling.
        self.sel_anchor = anchor_key.as_ref().and_then(|k| self.find_sel_key(k));
        if self.sel_anchor.is_none() {
            self.visual = false;
        }
        // Stay on the same file across the layout switch.
        self.current_file = self
            .row_file_idx(self.selected)
            .unwrap_or(self.current_file);
        self.recompute_file_span();
        // Recenter so the cursor is roughly mid-viewport (clamped to the file).
        self.scroll = self.selected.saturating_sub(self.height / 2);
        self.ensure_visible();
        self.status = match self.view {
            View::Unified => "unified view".into(),
            View::Split => "split view".into(),
        };
    }

    /// Is the file sidebar an actual pane the user can focus?
    pub(super) fn sidebar_available(&self) -> bool {
        self.show_sidebar && !self.changeset.files.is_empty()
    }

    /// Focus clamped to reality (never Sidebar when there's no sidebar).
    pub(super) fn effective_focus(&self) -> Focus {
        if self.sidebar_available() {
            self.focus
        } else {
            Focus::Diff
        }
    }

    /// Selection background for the diff pane (dim when it isn't focused).
    pub(super) fn diff_cursor_bg(&self) -> Color {
        if self.effective_focus() == Focus::Diff {
            theme().cursor_bg
        } else {
            theme().unfocus_bg
        }
    }

    /// Enter/leave visual line-select mode. Entering anchors the selection at
    /// the cursor; leaving drops it.
    pub(super) fn toggle_visual(&mut self) {
        if self.visual {
            self.visual = false;
            self.sel_anchor = None;
            self.status = "visual off".into();
        } else {
            self.visual = true;
            self.sel_anchor = Some(self.selected);
            self.status = "visual — j/k to extend, i to comment, esc to cancel".into();
        }
    }

    /// Extend the line selection by one row (Shift+Up/Down). Moves only across
    /// selectable *diff lines* (skipping comment rows): a line selection must
    /// stay anchored on diff lines, or `selection_range()` — which reads
    /// `anchor_at(selected)` — would return `None` and `i` (new thread) would
    /// have nothing to anchor to. A no-op when there is no diff line in that
    /// direction.
    ///
    /// Unlike `v`, this does NOT enter persistent visual mode: terminals can't
    /// report Shift key-release, so the heuristic is that the *next* unmodified
    /// movement (plain `j`/`k`) collapses the range (via `move_selection`'s
    /// `!visual` branch). Consecutive Shift+arrows keep growing it because the
    /// anchor survives between presses.
    pub(super) fn extend_selection(&mut self, dir: isize) {
        let (start, end) = self.file_range();
        let mut i = self.selected as isize + dir;
        let target = loop {
            if i < start as isize || i as usize >= end {
                return;
            }
            if self.is_selectable_at(i as usize) {
                break i as usize;
            }
            i += dir;
        };
        // Anchor the range at the current line on the first Shift+arrow; keep it
        // on subsequent ones (so the span grows). A prior plain move will have
        // cleared it, starting a fresh range here.
        if self.sel_anchor.is_none() {
            self.sel_anchor = Some(self.selected);
        }
        self.selected = target;
        self.ensure_visible();
        self.status = "shift+↑/↓ to extend · i to comment · move to clear".into();
    }

    /// The (file, side, line-range) covered by the current selection, matching
    /// the cursor line's file+side. Falls back to the single cursor line when
    /// there's no active selection. Lines on a different side/file than the
    /// cursor are ignored (a comment anchors to one side).
    pub(super) fn selection_range(&self) -> Option<(usize, Side, u32, u32)> {
        let (fi, side, cur) = self.anchor_at(self.selected)?;
        let (lo, hi) = self.selection_bounds();
        let (mut start, mut end) = (cur, cur);
        for i in lo..=hi {
            if let Some((f, s, l)) = self.anchor_at(i) {
                if f == fi && s == side {
                    start = start.min(l);
                    end = end.max(l);
                }
            }
        }
        Some((fi, side, start, end))
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if !self.visual {
            self.sel_anchor = None;
        }
        let (start, end) = self.file_range();
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < start as isize || i as usize >= end {
                return;
            }
            if self.is_stop_at(i as usize) {
                self.selected = i as usize;
                self.ensure_visible();
                return;
            }
        }
    }

    /// Move the selection `count` selectable rows in `dir` (+1 down / -1 up).
    pub(super) fn move_by(&mut self, dir: isize, count: usize) {
        for _ in 0..count {
            let before = self.selected;
            self.move_selection(dir);
            if self.selected == before {
                break; // hit top/bottom
            }
        }
    }

    /// Scroll the viewport by `delta` rows, dragging the selection back into
    /// view if it would fall outside (less/vim Ctrl-E / Ctrl-Y behavior).
    pub(super) fn scroll_view(&mut self, delta: isize) {
        let (start, end) = self.file_range();
        // Use an effective viewport height of at least 1 so scroll math stays
        // valid even when the bordered diff panel's inner height is 0.
        let height = self.height.max(1);
        // Cap at the last full screen so the wheel can't scroll past the final
        // line into empty space (which would drag the selection along with it).
        // Mirrors the scrollbar's `total - height` maximum.
        let max_top = self.max_scroll_row(start, end, height) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(start as isize, max_top) as usize;
        // Scrolling the pane is independent of the selected line: the cursor
        // stays put (and simply scrolls out of view) until the user moves it.
    }

    pub(super) fn ensure_visible(&mut self) {
        let (start, end) = self.file_range();
        let height = self.height.max(1);
        // For a focused comment, keep the whole message in view (biased to its
        // top when taller than the viewport); otherwise just the cursor row.
        let (top_row, bot_row) = self
            .comment_unit_span(self.selected)
            .unwrap_or((self.selected, self.selected));
        // Scroll down so the unit's last row fits from below, then up so its
        // first row is visible (the latter wins for a unit taller than the
        // viewport). Display-line aware, so a wrapped cursor line is followed
        // by all of its visual lines.
        let need_top = self.top_to_show(start, bot_row, height);
        if self.scroll < need_top {
            self.scroll = need_top;
        }
        if top_row < self.scroll {
            self.scroll = top_row;
        }
        // Never scroll outside the current file's slice, and never past the
        // last full screen of content.
        self.scroll = self
            .scroll
            .clamp(start, self.max_scroll_row(start, end, height));
    }

    pub(super) fn jump_comment(&mut self, dir: isize) {
        self.sel_anchor = None;
        // Collect the *head* row of each thread in the current file. Navigation
        // (`n`/`N`) deliberately stops once per thread, at its first line
        // (`range.start`) — unlike the act-on-thread operations (reply/resolve/
        // delete), which match anywhere in the range via `range.contains`.
        let (start, end) = self.file_range();
        let mut targets: Vec<usize> = Vec::new();
        for i in start..end {
            if let Some((file_idx, side, line)) = self.anchor_at(i) {
                if let Some(file) = self.changeset.files.get(file_idx) {
                    let path = PathBuf::from(file.display_path());
                    if self
                        .comments
                        .threads
                        .iter()
                        .any(|t| t.file == path && t.side == side && t.range.start == line)
                    {
                        targets.push(i);
                    }
                }
            }
        }
        if targets.is_empty() {
            self.status = "no comments".into();
            return;
        }
        let next = if dir > 0 {
            targets
                .iter()
                .find(|&&i| i > self.selected)
                .copied()
                .or_else(|| targets.first().copied())
        } else {
            targets
                .iter()
                .rev()
                .find(|&&i| i < self.selected)
                .copied()
                .or_else(|| targets.last().copied())
        };
        if let Some(i) = next {
            self.selected = i;
            self.ensure_visible();
        }
    }
}
