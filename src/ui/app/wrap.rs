//! Soft-wrap geometry: per-row display heights and display-line math.

use super::*;

impl App {
    // ---- soft-wrap geometry (no-ops while `self.wrap` is off) ----

    /// Columns reserved before the code on a unified diff line: two 5-wide
    /// line-number columns + a space each (`"{:>5} {:>5} "` = 12) + the 1-col
    /// add/del sign. Continuation lines pad this width to align under the code.
    pub(super) const UNI_PREFIX: usize = 13;

    /// Columns reserved before the code on one side of a split row
    /// (`"{:>4} "`). Matches `side_spans`/`side_line_rows`.
    pub(super) const SPLIT_PREFIX: usize = 5;

    /// Width of one split column for content area `width`. Single source of
    /// truth for `render_split`'s `side_w` and the wrap-height budget. Reserves
    /// the divider's *display* width (cells), not its UTF-8 byte length — the
    /// glyph in `" │ "` is multi-byte but one cell wide.
    pub(super) fn split_side_w(width: usize) -> usize {
        width.saturating_sub(str_width(SPLIT_DIVIDER)) / 2
    }

    /// Display height (terminal lines) of row `idx` in the active view. Always 1
    /// unless `wrap` is on and the row is a code line wide enough to wrap.
    pub(super) fn row_h(&self, idx: usize) -> usize {
        if !self.wrap {
            return 1;
        }
        self.row_heights.get(idx).copied().unwrap_or(1) as usize
    }

    /// Total display lines spanned by rows `[start, end)` in the active view.
    /// O(1): a difference of the `row_offsets` prefix sum (or a plain row count
    /// when wrap is off). Falls back to a direct walk only if the prefix sum is
    /// stale/missing for the requested range.
    pub(super) fn display_lines(&self, start: usize, end: usize) -> usize {
        if !self.wrap {
            return end.saturating_sub(start);
        }
        if end < self.row_offsets.len() && start <= end {
            return self.row_offsets[end] - self.row_offsets[start];
        }
        (start..end).map(|i| self.row_h(i)).sum()
    }

    /// The largest top row (>= `start`) such that the rows from it to `end`
    /// fit within `height` display lines — i.e. the furthest the viewport can
    /// scroll without revealing empty space past the last line.
    pub(super) fn max_scroll_row(&self, start: usize, end: usize, height: usize) -> usize {
        if end <= start {
            return start;
        }
        let mut acc = 0usize;
        let mut t = end;
        while t > start {
            let h = self.row_h(t - 1);
            if acc + h > height {
                break;
            }
            acc += h;
            t -= 1;
        }
        // A single bottom row taller than the viewport leaves `t == end`; clamp
        // so we still scroll to (the top of) that last row.
        t.min(end - 1).max(start)
    }

    /// The topmost row that keeps `bottom` (and everything down to it that
    /// fits) within `height` display lines — used to scroll a selection's last
    /// row into view from below.
    pub(super) fn top_to_show(&self, start: usize, bottom: usize, height: usize) -> usize {
        let mut acc = 0usize;
        let mut t = bottom + 1;
        while t > start {
            let h = self.row_h(t - 1);
            if acc + h > height {
                break;
            }
            acc += h;
            t -= 1;
        }
        t.min(bottom).max(start)
    }

    /// Recompute the per-row display heights for the active view at content
    /// `width`. A cheap no-op when wrap is off, the width is unchanged, and the
    /// cache is clean. Called from `draw` before the viewport reads heights.
    pub(super) fn update_heights(&mut self, width: usize) {
        if !self.wrap {
            if !self.row_heights.is_empty() {
                self.row_heights.clear();
                self.row_offsets.clear();
            }
            self.heights_width = width;
            self.heights_dirty = false;
            return;
        }
        if !self.heights_dirty
            && self.heights_width == width
            && self.row_heights.len() == self.active_len()
        {
            return;
        }
        let uni_budget = width.saturating_sub(Self::UNI_PREFIX);
        let side_budget = Self::split_side_w(width).saturating_sub(Self::SPLIT_PREFIX);
        let heights: Vec<u16> = match self.view {
            View::Unified => self
                .rows
                .iter()
                .map(|r| match &r.kind {
                    // `r.text` is `"{sign}{code}"`; wrap the code only.
                    RowKind::Line { .. } => {
                        // Clamp so a pathologically long/wrapped line (e.g. a
                        // minified file at a tiny budget) can't overflow u16 and
                        // under-report its height, which would desync
                        // scroll/click mapping.
                        wrap_count(r.text.get(1..).unwrap_or(""), uni_budget).min(u16::MAX as usize)
                            as u16
                    }
                    _ => 1,
                })
                .collect(),
            View::Split => self
                .split_rows
                .iter()
                .map(|r| match &r.kind {
                    SplitRowKind::Pair { left, right } => {
                        let l = left
                            .as_ref()
                            .map_or(1, |c| wrap_count(&c.text, side_budget));
                        let rr = right
                            .as_ref()
                            .map_or(1, |c| wrap_count(&c.text, side_budget));
                        l.max(rr).min(u16::MAX as usize) as u16
                    }
                    _ => 1,
                })
                .collect(),
        };
        // Prefix sum for O(1) range/scrollbar queries.
        let mut offsets = Vec::with_capacity(heights.len() + 1);
        let mut acc = 0usize;
        offsets.push(0);
        for &h in &heights {
            acc += h as usize;
            offsets.push(acc);
        }
        self.row_heights = heights;
        self.row_offsets = offsets;
        self.heights_width = width;
        self.heights_dirty = false;
    }

    /// Toggle soft-wrap, keeping the cursor in view under the new geometry.
    pub(super) fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        self.heights_dirty = true;
        // Heights are recomputed on the next draw (the content width is only
        // known there); the cleared cache makes `row_h` fall back to 1 until
        // then, which is harmless for the single ensure_visible below.
        self.row_heights.clear();
        self.row_offsets.clear();
        self.ensure_visible();
        self.status = if self.wrap {
            "wrap on".into()
        } else {
            "wrap off".into()
        };
    }
}
