//! Key and mouse input handling, sidebar interaction, scrolling.

use super::*;

impl App {
    /// Mouse: wheel scrolls the pane under the pointer; left-click selects;
    /// dragging the divider resizes the sidebar.
    /// The button whose recorded screen rect contains `(col, row)`, if any.
    pub(super) fn button_at(&self, col: u16, row: u16) -> Option<ButtonAction> {
        self.button_hits
            .borrow()
            .iter()
            .find(|(r, _)| hit(*r, col, row))
            .map(|(_, a)| a.clone())
    }

    /// Run a clicked button's action.
    pub(super) fn dispatch_button(&mut self, action: ButtonAction) {
        match action {
            ButtonAction::AddComment => self.open_new_thread(),
            ButtonAction::Submit => self.submit_compose(),
            ButtonAction::Cancel => self.cancel_compose(),
            ButtonAction::Reply(tid) => {
                self.select_thread(&tid);
                self.open_reply_to(tid);
            }
            ButtonAction::ToggleResolve(tid) => {
                self.select_thread(&tid);
                self.toggle_resolved_thread(tid);
            }
            ButtonAction::Delete(tid, cid) => {
                self.select_thread(&tid);
                self.delete_comment(tid, cid);
            }
        }
    }

    pub(super) fn on_mouse(&mut self, me: MouseEvent) {
        // Clickable buttons win over everything else (and work while the
        // composer is open, since its submit/cancel live in the box).
        if let MouseEventKind::Down(MouseButton::Left) = me.kind {
            if let Some(action) = self.button_at(me.column, me.row) {
                self.dispatch_button(action);
                return;
            }
        }
        // The composer modal swallows all other mouse input.
        if self.composer.is_some() {
            return;
        }
        let (col, row) = (me.column, me.row);
        let on_divider = self.divider_col() == Some(col);
        let over_sidebar = self.sidebar_area.width > 0 && hit(self.sidebar_area, col, row);
        match me.kind {
            MouseEventKind::Up(_) => {
                self.resizing = false;
                self.sb_drag = None;
            }
            // Scrollbar thumb drag (start + continue).
            MouseEventKind::Down(MouseButton::Left) if hit(self.diff_sb, col, row) => {
                self.sb_drag = Some(Focus::Diff);
                self.drag_diff_sb(row);
            }
            MouseEventKind::Down(MouseButton::Left) if hit(self.sidebar_sb, col, row) => {
                self.sb_drag = Some(Focus::Sidebar);
                self.drag_sidebar_sb(row);
            }
            // The pane border (rightmost column) is the resize divider.
            MouseEventKind::Down(MouseButton::Left) if on_divider => self.resizing = true,
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Diff) => {
                self.drag_diff_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sb_drag == Some(Focus::Sidebar) => {
                self.drag_sidebar_sb(row)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing => self.resize_to(col),
            // Wheel scrolls the pane under the pointer. Over the sidebar it
            // moves the list only — selection/focus are left untouched.
            MouseEventKind::ScrollDown => {
                if over_sidebar {
                    self.scroll_sidebar(3);
                } else {
                    self.focus = Focus::Diff;
                    self.scroll_view(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if over_sidebar {
                    self.scroll_sidebar(-3);
                } else {
                    self.focus = Focus::Diff;
                    self.scroll_view(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if over_sidebar {
                    self.click_sidebar(row);
                } else if hit(self.diff_area, col, row) {
                    self.click_diff(row, true);
                }
            }
            // Drag in the diff extends the line selection.
            MouseEventKind::Drag(MouseButton::Left) if hit(self.diff_area, col, row) => {
                self.click_diff(row, false);
            }
            _ => {}
        }
    }

    /// The code text of row `idx` (sign stripped / new side), if it's a line.
    pub(super) fn line_text(&self, idx: usize) -> Option<String> {
        match self.view {
            View::Unified => match self.rows.get(idx)?.kind {
                RowKind::Line { .. } => {
                    let t = &self.rows[idx].text;
                    Some(t.get(1..).unwrap_or("").to_string())
                }
                _ => None,
            },
            View::Split => match &self.split_rows.get(idx)?.kind {
                SplitRowKind::Pair { left, right } => {
                    right.as_ref().or(left.as_ref()).map(|c| c.text.clone())
                }
                _ => None,
            },
        }
    }

    /// Copy the selection to the system clipboard (via OSC 52 next frame): the
    /// focused comment's body when a comment is selected, else the diff lines.
    pub(super) fn copy_selection(&mut self) {
        if let Some((thread_id, comment_id)) = self.focused_comment() {
            if let Some(body) = self
                .comments
                .threads
                .iter()
                .find(|t| t.id == thread_id)
                .and_then(|t| {
                    t.comments
                        .iter()
                        .find(|c| c.id == comment_id)
                        .map(|c| c.body.clone())
                })
            {
                self.status = "copied comment".into();
                self.pending_copy = Some(body);
            }
            return;
        }
        let (lo, hi) = self.selection_bounds();
        let lines: Vec<String> = (lo..=hi).filter_map(|i| self.line_text(i)).collect();
        if lines.is_empty() {
            return;
        }
        self.status = format!("copied {} line(s)", lines.len());
        self.pending_copy = Some(lines.join("\n"));
    }

    /// Map a scrollbar drag at terminal `row` to a scroll position.
    pub(super) fn drag_diff_sb(&mut self, row: u16) {
        let (start, end) = self.file_range();
        let total = end - start;
        let pos = sb_thumb_pos(
            self.diff_sb.y,
            self.diff_sb.height as usize,
            total,
            self.height,
            row,
        );
        self.scroll = start + pos;
    }

    pub(super) fn drag_sidebar_sb(&mut self, row: u16) {
        let h = self.sidebar_sb.height as usize;
        self.sidebar_scroll = sb_thumb_pos(self.sidebar_sb.y, h, self.sidebar_rows.len(), h, row);
    }

    /// Scroll the file list independently of the selection.
    pub(super) fn scroll_sidebar(&mut self, delta: isize) {
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        self.sidebar_scroll =
            (self.sidebar_scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Scroll the list so the sidebar cursor row is visible.
    pub(super) fn reveal_sidebar(&mut self) {
        let h = self.sidebar_area.height as usize;
        if h == 0 {
            return;
        }
        let r = self
            .sidebar_sel
            .min(self.sidebar_rows.len().saturating_sub(1));
        // Include the row just above (dir header / parent file) when present.
        let target = r.saturating_sub(1);
        if target < self.sidebar_scroll {
            self.sidebar_scroll = target;
        } else if r >= self.sidebar_scroll + h {
            self.sidebar_scroll = r + 1 - h;
        }
    }

    /// Rebuild the sidebar tree from the current collapse set.
    pub(super) fn rebuild_sidebar(&mut self) {
        let (sr, map) = build_sidebar_rows(&self.changeset, &self.collapsed);
        self.sidebar_rows = sr;
        self.file_to_sbrow = map;
        // The row count shrank/grew; keep the scroll within bounds so clicks and
        // rendering agree.
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        self.sidebar_scroll = self.sidebar_scroll.min(max);
    }

    /// Expand every ancestor directory of `fi` so its row is visible.
    pub(super) fn reveal_file_in_tree(&mut self, fi: usize) {
        let Some(f) = self.changeset.files.get(fi) else {
            return;
        };
        let dir = dir_of(f.display_path());
        if dir.is_empty() {
            return;
        }
        let segs: Vec<&str> = dir.split('/').collect();
        let mut changed = false;
        for d in 0..segs.len() {
            if self.collapsed.remove(&segs[..=d].join("/")) {
                changed = true;
            }
        }
        if changed {
            self.rebuild_sidebar();
        }
    }

    /// Open or close directory `path`, keeping the cursor on its row.
    pub(super) fn set_dir_collapsed(&mut self, path: String, collapsed: bool) {
        let changed = if collapsed {
            self.collapsed.insert(path.clone())
        } else {
            self.collapsed.remove(&path)
        };
        if !changed {
            return;
        }
        self.rebuild_sidebar();
        if let Some(r) = self
            .sidebar_rows
            .iter()
            .position(|row| matches!(row, SbRow::Dir { path: p, .. } if *p == path))
        {
            self.sidebar_sel = r;
        }
        self.sidebar_sel = self
            .sidebar_sel
            .min(self.sidebar_rows.len().saturating_sub(1));
        self.reveal_sidebar();
    }

    /// Toggle the directory under the cursor (no-op on file rows).
    pub(super) fn toggle_dir(&mut self, path: String) {
        let collapsed = self.collapsed.contains(&path);
        self.set_dir_collapsed(path, !collapsed);
    }

    /// Collapse (`collapse = true`) or expand the directory under the cursor.
    /// Collapsing while on a file row closes its containing folder and
    /// moves the cursor onto that folder.
    pub(super) fn fold_dir(&mut self, collapse: bool) {
        match self.sidebar_rows.get(self.sidebar_sel) {
            Some(SbRow::Dir { path, .. }) => {
                let path = path.clone();
                // ← on an already-closed folder closes its container instead.
                if collapse && self.collapsed.contains(&path) {
                    let parent = dir_of(&path);
                    if !parent.is_empty() {
                        self.set_dir_collapsed(parent.to_string(), true);
                    }
                } else {
                    self.set_dir_collapsed(path, collapse);
                }
            }
            Some(SbRow::File { idx, .. }) if collapse => {
                let fi = *idx;
                if let Some(parent) = self.parent_dir_of_file(fi) {
                    self.set_dir_collapsed(parent, true);
                }
            }
            _ => {}
        }
    }

    /// The immediate containing directory of file `fi`, if it lives in one.
    pub(super) fn parent_dir_of_file(&self, fi: usize) -> Option<String> {
        let f = self.changeset.files.get(fi)?;
        let dir = dir_of(f.display_path());
        (!dir.is_empty()).then(|| dir.to_string())
    }

    /// Toggle the directory under the cursor open/closed.
    pub(super) fn fold_dir_toggle(&mut self) {
        if let Some(SbRow::Dir { path, .. }) = self.sidebar_rows.get(self.sidebar_sel) {
            let path = path.clone();
            self.toggle_dir(path);
        }
    }

    /// Move the sidebar cursor to the next/prev row and act on it. Every tree
    /// row (dir, file) is a valid landing spot, so this is a simple clamped step.
    pub(super) fn move_sidebar(&mut self, dir: isize) {
        let n = self.sidebar_rows.len();
        let next = self.sidebar_sel as isize + dir;
        if next < 0 || next as usize >= n {
            return;
        }
        self.sidebar_sel = next as usize;
        self.activate_sidebar();
    }

    /// Jump the sidebar cursor to the first/last row.
    pub(super) fn sidebar_edge(&mut self, last: bool) {
        let n = self.sidebar_rows.len();
        if n == 0 {
            return;
        }
        self.sidebar_sel = if last { n - 1 } else { 0 };
        self.activate_sidebar();
    }

    /// Apply the row under the sidebar cursor: switch to the file it names.
    pub(super) fn activate_sidebar(&mut self) {
        // Directory rows are just a cursor resting spot during navigation;
        // they toggle only on explicit activation.
        if let Some(SbRow::File { idx, .. }) = self.sidebar_rows.get(self.sidebar_sel) {
            let fi = *idx;
            if fi != self.current_file {
                self.set_current_file(fi);
            }
            self.reveal_sidebar();
        }
    }

    pub(super) fn click_sidebar(&mut self, row: u16) {
        let off = row.saturating_sub(self.sidebar_area.y) as usize;
        // Mirror render's clamp so clicks map to the row actually drawn.
        let h = self.sidebar_area.height as usize;
        let max = self.sidebar_rows.len().saturating_sub(h);
        let scroll = self.sidebar_scroll.min(max);
        let idx = scroll + off;
        match self.sidebar_rows.get(idx) {
            Some(SbRow::Dir { path, .. }) => {
                let path = path.clone();
                self.focus = Focus::Sidebar;
                self.sidebar_sel = idx;
                self.toggle_dir(path);
            }
            Some(SbRow::File { idx: fi, .. }) => {
                let fi = *fi;
                self.focus = Focus::Sidebar;
                self.set_current_file(fi);
            }
            None => {}
        }
    }

    /// Place the cursor at the clicked diff row. `anchor` starts a new
    /// selection there; otherwise the existing anchor is kept (drag extend).
    pub(super) fn click_diff(&mut self, row: u16, anchor: bool) {
        self.focus = Focus::Diff;
        let (start, end) = self.file_range();
        let top = self.scroll.max(start);
        // Walk wrapped row heights from the top of the viewport to map the
        // clicked display offset onto a logical row. With wrap off every height
        // is 1, so this reduces to `top + offset`.
        let off = row.saturating_sub(self.diff_area.y) as usize;
        let mut acc = 0usize;
        let mut idx = top;
        while idx < end {
            let h = self.row_h(idx);
            if off < acc + h {
                break;
            }
            acc += h;
            idx += 1;
        }
        let idx = idx.clamp(start, end.saturating_sub(1).max(start));
        if let Some(i) = self.stop_for(idx) {
            self.selected = i;
            // A drag-range only makes sense between diff lines; landing on a
            // comment selects that one message and drops any range anchor.
            if self.is_selectable_at(i) {
                if anchor {
                    self.sel_anchor = Some(i);
                }
            } else {
                self.sel_anchor = None;
            }
            self.ensure_visible();
        }
    }

    pub(super) fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // While composing, the modal owns every keystroke.
        if self.composer.is_some() {
            return self.on_key_compose(code, mods);
        }
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        // Global keys, independent of the focused pane.
        match code {
            // Quit only on q / Ctrl-C (never Esc; Ctrl-D is half-page down).
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Char('c') if ctrl => return self.quit = true,
            KeyCode::Tab | KeyCode::Char('s') => return self.toggle_view(),
            KeyCode::Char('b') if ctrl => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar {
                    self.focus = Focus::Diff;
                }
                return;
            }
            KeyCode::Char('l') if ctrl => return self.needs_clear = true,
            // Toggle soft-wrap of long diff lines.
            KeyCode::Char('w') => return self.toggle_wrap(),
            _ => {}
        }
        match self.effective_focus() {
            Focus::Sidebar => self.on_key_sidebar(code),
            Focus::Diff => self.on_key_diff(code, ctrl, mods.contains(KeyModifiers::SHIFT)),
        }
    }

    /// Navigation when the file sidebar (left pane) is focused.
    pub(super) fn on_key_sidebar(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_sidebar(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sidebar(-1),
            KeyCode::Char('g') | KeyCode::Home => self.sidebar_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.sidebar_edge(true),
            // Left/Right (or h/l): toggle the folder open state on a dir row.
            KeyCode::Left | KeyCode::Char('h') => self.fold_dir(true),
            KeyCode::Right | KeyCode::Char('l') => self.fold_dir(false),
            KeyCode::Char(' ') | KeyCode::Char('o') => self.fold_dir_toggle(),
            // Enter: move focus to the right (diff) pane.
            KeyCode::Enter => self.focus = Focus::Diff,
            _ => {}
        }
    }

    /// Navigation when the diff pane is focused.
    pub(super) fn on_key_diff(&mut self, code: KeyCode, ctrl: bool, shift: bool) {
        let page = self.height.max(1);
        let half = (self.height / 2).max(1);
        // Shift+Up/Down extends a line selection (an alternative to `v` visual
        // mode). Modified arrow keys ride the standard CSI cursor encoding, so
        // they survive tmux/SSH without the kitty protocol — unlike Shift+Enter.
        if shift {
            match code {
                KeyCode::Down => return self.extend_selection(1),
                KeyCode::Up => return self.extend_selection(-1),
                _ => {}
            }
        }
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1, 1),

            // Half-page down / up: Ctrl-D / Ctrl-U.
            KeyCode::Char('d') if ctrl => self.move_by(1, half),
            KeyCode::Char('u') if ctrl => self.move_by(-1, half),

            // Full page: Space / Ctrl-F / PageDown forward, b / PageUp back.
            KeyCode::Char(' ') | KeyCode::Char('f') | KeyCode::PageDown => self.move_by(1, page),
            KeyCode::Char('b') | KeyCode::PageUp => self.move_by(-1, page),

            // One-line viewport scroll, cursor stays in view: Ctrl-E / Ctrl-Y (less/vim).
            KeyCode::Char('e') if ctrl => self.scroll_view(1),
            KeyCode::Char('y') if ctrl => self.scroll_view(-1),

            // Top / bottom.
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.visual {
                    self.sel_anchor = None;
                }
                self.selected = self.first_selectable().unwrap_or(0);
                self.ensure_visible();
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.visual {
                    self.sel_anchor = None;
                }
                self.selected = self.last_selectable().unwrap_or(0);
                self.ensure_visible();
            }

            // Jump between comment threads.
            KeyCode::Char('n') => self.jump_comment(1),
            KeyCode::Char('N') => self.jump_comment(-1),

            // Visual line-select: anchor a comment to a multi-line range.
            KeyCode::Char('v') => self.toggle_visual(),

            // Compose a new thread (i) or reply to the thread here (r).
            KeyCode::Char('i') => self.open_new_thread(),
            KeyCode::Char('r') => self.open_reply(),

            // Resolve/unresolve (R) the thread on this line, or delete (D) the
            // focused comment.
            KeyCode::Char('R') => self.resolve_current_thread(),
            KeyCode::Char('D') => self.delete_current_comment(),

            // Jump between files.
            KeyCode::Char(']') => self.jump_file(1),
            KeyCode::Char('[') => self.jump_file(-1),

            // Copy the selected line(s).
            KeyCode::Char('y') => self.copy_selection(),
            // Esc: drop any drag selection and hand focus back to the sidebar
            // (never quits).
            KeyCode::Esc => {
                self.sel_anchor = None;
                if self.visual {
                    // First Esc just leaves visual mode (keeps focus here).
                    self.visual = false;
                    self.status = "visual off".into();
                } else if self.sidebar_available() {
                    self.focus = Focus::Sidebar;
                }
            }
            _ => {}
        }
    }
}
