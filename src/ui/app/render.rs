//! Frame layout and pane rendering.

use super::*;

impl App {
    /// Highlighted spans for `text`, truncated/padded to exactly `width` chars,
    /// with an optional background applied to every run (and the padding).
    pub(super) fn styled_fit(
        &self,
        file_idx: usize,
        text: &str,
        width: usize,
        bg: Option<Color>,
    ) -> Vec<Span<'static>> {
        let hl = self.hl.runs(file_idx, text);
        let mut out = Vec::new();
        let mut used = 0usize;
        for (c, s) in hl.iter() {
            if used >= width {
                break;
            }
            let (take, tw) = take_width(s, width - used);
            if take.is_empty() {
                continue;
            }
            used += tw;
            let mut st = Style::default().fg(*c);
            if let Some(b) = bg {
                st = st.bg(b);
            }
            out.push(Span::styled(take, st));
        }
        if used < width {
            let mut st = Style::default();
            if let Some(b) = bg {
                st = st.bg(b);
            }
            out.push(Span::styled(" ".repeat(width - used), st));
        }
        out
    }

    /// Re-wrap inline comment bodies to the current diff width, rebuilding the
    /// row lists when it changes. This is the sole row-affecting side effect of
    /// drawing: the diff width is only known during layout, yet the wrapped
    /// rows must be rebuilt before the frame reads them (and before any
    /// selection mapping). While the sidebar/diff divider is being dragged the
    /// wrap is frozen — the next draw after release picks up the final width and
    /// rebuilds exactly once instead of on every drag event.
    pub(super) fn sync_comment_wrap(&mut self, diff_inner_width: u16) {
        let inner = diff_inner_width as usize;
        let cw = match self.view {
            // Unified: the box spans the full inner width. Reserve a 2-col
            // margin + 2 borders + 3-col body indent + 1 scrollbar column = 8.
            View::Unified => inner.saturating_sub(8),
            // Split: the box lives inside one half-column, so wrapping to the
            // full width would clip every line on the right. Mirror
            // `render_split`'s `side_w = (area - str_width(SPLIT_DIVIDER)) / 2` for
            // the worst case (a scrollbar present trims the area by 1), then
            // reserve the box chrome (2-col margin + 2 borders + 3-col indent
            // = 7).
            View::Split => {
                let side = inner.saturating_sub(1 + str_width(SPLIT_DIVIDER)) / 2;
                side.saturating_sub(7)
            }
        };
        if cw != self.comment_wrap && !self.resizing {
            self.comment_wrap = cw;
            // Re-wrap when there are inline boxes to re-wrap: comment threads
            // or an open composer (which renders inline too).
            if !self.comments.threads.is_empty() || self.composer.is_some() {
                self.rebuild_rows();
            }
        }
    }

    pub(super) fn draw(&mut self, f: &mut Frame) {
        // Pre-highlight the file in view off the render path so scrolling stays
        // smooth. Catches every path that changes the visible file (nav, jumps,
        // scroll across file boundaries).
        self.hl.warm(self.current_file);
        let area = f.area();
        // Paint the themed background across the whole frame first; widgets draw
        // on top, and any gaps (padding, short lines) keep the theme bg instead
        // of the terminal default.
        f.render_widget(
            Block::default().style(Style::default().bg(theme().bg)),
            area,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        // Body: optional file sidebar on the left, diff on the right.
        let body = chunks[0];
        let sidebar = self.show_sidebar && !self.changeset.files.is_empty() && body.width >= 60;
        let (diff_outer, sidebar_area) = if sidebar {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(self.sidebar_width), Constraint::Min(1)])
                .split(body);
            (cols[1], cols[0])
        } else {
            (body, Rect::default())
        };
        // The diff is a floating panel sitting *on top of* the sidebar: a
        // rounded border frames it and brightens when it holds focus, so Esc
        // (which drops diff focus back to the sidebar) reads as dismissing the
        // panel. Its left border doubles as the resize divider.
        let diff_focused = self.effective_focus() == Focus::Diff;
        let diff_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(theme().bg))
            .border_style(Style::default().fg(if diff_focused {
                theme().border_focus
            } else {
                theme().border_unfocus
            }));
        let diff_inner = diff_block.inner(diff_outer);
        // Layout is only known here, so the one row-affecting side effect of
        // drawing (re-wrapping inline comments to the diff width) is isolated
        // in this helper and must run before any code reads the row lists.
        self.sync_comment_wrap(diff_inner.width);
        if sidebar {
            self.render_sidebar(f, sidebar_area);
        }
        self.sidebar_area = sidebar_area;
        self.sidebar_sb =
            if sidebar_area.width > 0 && self.sidebar_rows.len() > sidebar_area.height as usize {
                Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(1),
                    y: sidebar_area.y,
                    width: 1,
                    height: sidebar_area.height,
                }
            } else {
                Rect::default()
            };
        f.render_widget(diff_block, diff_outer);
        self.height = diff_inner.height as usize;
        // Reserve the rightmost inner column for a scrollbar when it overflows.
        // Heights depend on the wrap width, so compute them at the full inner
        // width first to decide overflow; if a scrollbar then steals a column,
        // recompute at the narrower content width before rendering so the
        // viewport/mouse/scrollbar all agree on row heights.
        self.update_heights(diff_inner.width as usize);
        let (fr_start, fr_end) = self.file_range();
        let overflow = self.display_lines(fr_start, fr_end) > self.height;
        let content = if overflow {
            Rect {
                width: diff_inner.width.saturating_sub(1),
                ..diff_inner
            }
        } else {
            diff_inner
        };
        self.diff_area = content;
        self.diff_sb = if overflow {
            Rect {
                x: diff_inner.x + diff_inner.width.saturating_sub(1),
                y: diff_inner.y,
                width: 1,
                height: diff_inner.height,
            }
        } else {
            Rect::default()
        };
        self.update_heights(content.width as usize);
        // Heights for the final content width are now known. A resize (notably
        // widening, which shrinks wrap heights) can leave `self.scroll` past
        // the new last full screen, painting empty space beyond EOF; clamp it
        // before rendering so the viewport stays anchored to content.
        self.scroll = self.scroll.clamp(
            fr_start,
            self.max_scroll_row(fr_start, fr_end, self.height.max(1)),
        );
        self.render_diff(f, content);
        if overflow {
            self.render_diff_scrollbar(f, diff_inner);
        }

        // Footer: transient status on the left, a context key-hint legend on
        // the right (so shortcuts like Tab are discoverable). Comment actions
        // live on buttons now, so they're omitted from the legend.
        let fw = chunks[1].width as usize;
        let legend = self.footer_legend();
        let sl = str_width(&self.status);
        let ll = str_width(&legend);
        let footer = if sl + ll + 2 <= fw {
            Line::from(vec![
                Span::styled(self.status.clone(), Style::default().fg(theme().muted)),
                Span::raw(" ".repeat(fw - sl - ll)),
                Span::styled(legend, Style::default().fg(theme().faint)),
            ])
        } else if sl > 0 {
            // Not enough room for both: the transient status wins.
            Line::from(Span::styled(
                self.status.clone(),
                Style::default().fg(theme().muted),
            ))
        } else {
            Line::from(Span::styled(legend, Style::default().fg(theme().faint)))
        };
        f.render_widget(
            Paragraph::new(footer).style(Style::default().bg(theme().bg)),
            chunks[1],
        );
    }

    /// Context-aware key-hint legend shown on the right of the footer.
    pub(super) fn footer_legend(&self) -> String {
        if self.composer.is_some() {
            return "ctrl+s submit · esc cancel".into();
        }
        match self.effective_focus() {
            Focus::Sidebar => "j/k move · enter open · h/l fold · tab view · q quit".into(),
            Focus::Diff => {
                "j/k move · v select · n/N comments · [/] files · tab view · w wrap · y copy · esc back · q quit"
                    .into()
            }
        }
    }

    pub(super) fn render_diff(&self, f: &mut Frame, area: Rect) {
        // Button hit regions are recreated each frame as the boxes render.
        self.button_hits.borrow_mut().clear();
        match self.view {
            View::Unified => self.render_unified(f, area),
            View::Split => self.render_split(f, area),
        }
    }

    /// Left-hand collapsible file tree: directories and files (with a one-letter
    /// change status and a comment-state dot), indented by depth.
    pub(super) fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let focused = self.effective_focus() == Focus::Sidebar;
        // No border on the sidebar: the floating diff panel's rounded left
        // border is the visual divider between the two panes.
        let inner = area;

        let h = inner.height as usize;
        let n = self.sidebar_rows.len();
        // The scrollbar sits just left of the right border; reserve a content
        // column for it when the list overflows.
        let need_sb = n > h;
        let w = (inner.width as usize).saturating_sub(if need_sb { 1 } else { 0 });
        let max = n.saturating_sub(h);
        let scroll = self.sidebar_scroll.min(max);

        let mut lines: Vec<Line> = Vec::new();
        for idx in scroll..n.min(scroll + h) {
            let is_cursor = focused && idx == self.sidebar_sel;
            match &self.sidebar_rows[idx] {
                SbRow::Dir { name, depth, path } => {
                    let indent = "  ".repeat(depth + 1);
                    let arrow = if self.collapsed.contains(path) {
                        "▶ "
                    } else {
                        "▼ "
                    };
                    let avail = w.saturating_sub(indent.chars().count() + 2);
                    let label = pad_width(&elide_left(name, avail), avail);
                    let bg = if is_cursor {
                        Some(theme().cursor_bg)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    lines.push(Line::from(vec![
                        Span::styled(indent, wbg(Style::default())),
                        Span::styled(arrow, wbg(Style::default().fg(theme().muted))),
                        Span::styled(label, wbg(Style::default().fg(theme().muted))),
                    ]));
                }
                SbRow::File { idx: fi, depth } => {
                    let fi = *fi;
                    let is_cur = fi == self.current_file;
                    let (status, status_color) = self
                        .changeset
                        .files
                        .get(fi)
                        .map(file_status)
                        .unwrap_or(('M', theme().warn));
                    let indent = "  ".repeat(depth + 1);
                    let path = self
                        .changeset
                        .files
                        .get(fi)
                        .map(|f| f.display_path())
                        .unwrap_or_default();
                    // A comment dot just left of the filename: yellow = open,
                    // hollow gray = all resolved, blank = none.
                    let (dot, dot_color) = match file_comment_state(&self.comments, path) {
                        Some(true) => ("● ", theme().warn),
                        Some(false) => ("○ ", theme().muted),
                        None => ("  ", theme().none),
                    };
                    let (adds, dels) = self.file_stats.get(fi).copied().unwrap_or((0, 0));
                    let counts = format!(" +{adds} -{dels}");
                    let avail = w
                        .saturating_sub(indent.chars().count() + 4)
                        .saturating_sub(counts.chars().count());
                    let base = base_of(path);
                    let name = pad_width(&elide_left(base, avail), avail);
                    let bg = if is_cursor {
                        Some(theme().cursor_bg)
                    } else if is_cur {
                        Some(theme().unfocus_bg)
                    } else {
                        None
                    };
                    let wbg = |st: Style| match bg {
                        Some(b) => st.bg(b),
                        None => st,
                    };
                    let name_style = if is_cur {
                        Style::default()
                            .fg(theme().text_strong)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme().text)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(indent, wbg(Style::default())),
                        Span::styled(format!("{status} "), wbg(Style::default().fg(status_color))),
                        Span::styled(dot, wbg(Style::default().fg(dot_color))),
                        Span::styled(name, wbg(name_style)),
                        Span::styled(format!(" +{adds}"), wbg(Style::default().fg(theme().added))),
                        Span::styled(
                            format!(" -{dels}"),
                            wbg(Style::default().fg(theme().removed)),
                        ),
                    ]));
                }
            }
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            inner,
        );
        if need_sb {
            let mut sb = ScrollbarState::new(max + 1)
                .position(scroll)
                .viewport_content_length(h);
            // Render inside `inner` so the thumb lands one column left of the
            // pane border (which stays a clean, continuous resize divider). The
            // track is blank so it doesn't double up against the border.
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some(" "))
                    .thumb_symbol("█")
                    .thumb_style(Style::default().fg(theme().scrollbar_thumb)),
                inner,
                &mut sb,
            );
        }
    }

    /// A vertical scrollbar on the right edge of `area` for the diff pane.
    pub(super) fn render_diff_scrollbar(&self, f: &mut Frame, area: Rect) {
        let (start, end) = self.file_range();
        // Measured in display lines so the thumb tracks wrapped content too.
        let total = self.display_lines(start, end);
        if total <= self.height {
            return;
        }
        let max_top = total - self.height;
        let pos = self.display_lines(start, self.scroll).min(max_top);
        let mut sb = ScrollbarState::new(max_top + 1)
            .position(pos)
            .viewport_content_length(self.height);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("█")
                .thumb_style(Style::default().fg(theme().scrollbar_thumb)),
            area,
            &mut sb,
        );
    }

    pub(super) fn render_unified(&self, f: &mut Frame, area: Rect) {
        let width = area.width as usize;
        let height = area.height as usize;
        let (start, file_end) = self.file_range();
        let top = self.scroll.max(start);
        let mut lines: Vec<Line> = Vec::new();
        let mut idx = top;
        while lines.len() < height && idx < file_end {
            let row = &self.rows[idx];
            let selected = self.in_selection(idx);
            // First visual line of this row (action rows / buttons anchor here).
            let y = area.y + lines.len() as u16;
            let mut row_lines: Vec<Line> = match &row.kind {
                RowKind::Comment(cl) if matches!(cl.kind, CommentKind::Actions) => {
                    let border = if cl.resolved {
                        theme().muted
                    } else {
                        theme().border_unfocus
                    };
                    let btns = self.comment_buttons(cl);
                    vec![self.action_row_line(&btns, border, area.x, y, width, 0, width)]
                }
                RowKind::Composer(c) if matches!(c.kind, ComposerKind::Hint) => {
                    let btns = self.composer_buttons();
                    vec![self.action_row_line(&btns, theme().accent, area.x, y, width, 0, width)]
                }
                _ => self.row_to_lines(row, selected, width),
            };
            if matches!(row.kind, RowKind::Line { .. }) && self.show_add_button(idx) {
                if let Some(first) = row_lines.first_mut() {
                    let spans = std::mem::take(&mut first.spans);
                    *first = self.append_right_button(
                        spans,
                        width,
                        "comment(i)",
                        theme().accent,
                        ButtonAction::AddComment,
                        area.x,
                        y,
                    );
                }
            }
            debug_assert!(
                !self.wrap || row_lines.len() == self.row_h(idx),
                "render produced {} lines for row {idx} but row_h says {}",
                row_lines.len(),
                self.row_h(idx)
            );
            for l in row_lines {
                if lines.len() >= height {
                    break;
                }
                lines.push(l);
            }
            idx += 1;
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            area,
        );
    }

    /// Whether the floating `comment(i)` button should appear on row `idx`: the
    /// diff pane is focused, no composer is open, and `idx` is the *last* row of
    /// the current selection. A new thread (and its composer) renders after the
    /// last line of its range, so the button sits there too — right where the
    /// comment box will open — not on a different end of a range selection.
    pub(super) fn show_add_button(&self, idx: usize) -> bool {
        self.composer.is_none()
            && self.effective_focus() == Focus::Diff
            && idx == self.selection_bounds().1
    }

    pub(super) fn render_split(&self, f: &mut Frame, area: Rect) {
        let total = area.width as usize;
        let divider = SPLIT_DIVIDER;
        let side_w = Self::split_side_w(total);
        let (start, file_end) = self.file_range();
        let top = self.scroll.max(start);
        let height = area.height as usize;
        let dw = divider.chars().count();
        let mut lines: Vec<Line> = Vec::new();
        let mut idx = top;
        while lines.len() < height && idx < file_end {
            let row = &self.split_rows[idx];
            let selected = self.in_selection(idx);
            let y = area.y + lines.len() as u16;
            let mut row_lines: Vec<Line> = match &row.kind {
                SplitRowKind::Comment { side, line: cl }
                    if matches!(cl.kind, CommentKind::Actions) =>
                {
                    let border = if cl.resolved {
                        theme().muted
                    } else {
                        theme().border_unfocus
                    };
                    let btns = self.comment_buttons(cl);
                    let left = if matches!(side, Side::Old) {
                        0
                    } else {
                        side_w + dw
                    };
                    vec![self.action_row_line(&btns, border, area.x, y, total, left, side_w)]
                }
                SplitRowKind::Composer { side, line: cl }
                    if matches!(cl.kind, ComposerKind::Hint) =>
                {
                    let btns = self.composer_buttons();
                    let left = if matches!(side, Side::Old) {
                        0
                    } else {
                        side_w + dw
                    };
                    vec![self.action_row_line(
                        &btns,
                        theme().accent,
                        area.x,
                        y,
                        total,
                        left,
                        side_w,
                    )]
                }
                _ => self.split_row_to_lines(row, selected, side_w, divider),
            };
            if matches!(row.kind, SplitRowKind::Pair { .. }) && self.show_add_button(idx) {
                if let Some(first) = row_lines.first_mut() {
                    let spans = std::mem::take(&mut first.spans);
                    *first = self.append_right_button(
                        spans,
                        total,
                        "comment(i)",
                        theme().accent,
                        ButtonAction::AddComment,
                        area.x,
                        y,
                    );
                }
            }
            debug_assert!(
                !self.wrap || row_lines.len() == self.row_h(idx),
                "render produced {} lines for split row {idx} but row_h says {}",
                row_lines.len(),
                self.row_h(idx)
            );
            for l in row_lines {
                if lines.len() >= height {
                    break;
                }
                lines.push(l);
            }
            idx += 1;
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme().bg)),
            area,
        );
    }
}
