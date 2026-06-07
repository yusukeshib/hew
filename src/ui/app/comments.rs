//! Inline composer and comment-thread operations.

use super::*;

impl App {
    /// Rebuild the diff row lists from the changeset + inline comment threads,
    /// keeping the cursor on the same (file, side, line) anchor.
    pub(super) fn rebuild_rows(&mut self) {
        let key = self.sel_key();
        let cur_file = self.current_file;
        // Rebuild only the view on screen; mark the other stale so toggle_view
        // rebuilds it on demand. Halves per-keystroke composer cost — both lists
        // derive from the same comments/width, so the lazy rebuild is lossless.
        self.rebuild_active_view();
        // Rows changed; recompute every file's span, then the current one,
        // before first_selectable/ensure_visible read it.
        self.rebuild_file_spans();
        self.recompute_file_span();
        let target = key.as_ref().and_then(|k| self.find_sel_key(k));
        self.selected = target
            .or_else(|| self.first_selectable())
            .unwrap_or(0)
            .min(self.active_len().saturating_sub(1));
        self.current_file = self.row_file_idx(self.selected).unwrap_or(cur_file);
        self.recompute_file_span();
        self.geom.dirty = true;
        self.ensure_visible();
    }

    /// Rebuild the active view's row list from the current comments/composer,
    /// marking the *inactive* view stale (it is reconstructed lazily by
    /// [`Self::ensure_active_view_built`] when `toggle_view` switches to it).
    fn rebuild_active_view(&mut self) {
        self.build_view(self.view);
        match self.view {
            View::Unified => self.split_dirty = true,
            View::Split => self.unified_dirty = true,
        }
    }

    /// Ensure the active view's row list is current, rebuilding it when a prior
    /// edit left it stale. A cheap no-op when it's already fresh. Call before
    /// any code reads the active row list after a view switch.
    pub(super) fn ensure_active_view_built(&mut self) {
        let stale = match self.view {
            View::Unified => self.unified_dirty,
            View::Split => self.split_dirty,
        };
        if stale {
            self.build_view(self.view);
        }
    }

    /// Build one view's row list from the current state, clearing its stale flag.
    fn build_view(&mut self, view: View) {
        let composer = self.composer_spec();
        match view {
            View::Unified => {
                self.rows = build_rows(
                    &self.changeset,
                    &self.comments,
                    self.comment_wrap,
                    composer.as_ref(),
                );
                self.unified_dirty = false;
            }
            View::Split => {
                self.split_rows = build_split_rows(
                    &self.changeset,
                    &self.comments,
                    self.comment_wrap,
                    composer.as_ref(),
                );
                self.split_dirty = false;
            }
        }
    }

    /// Translate the live composer into a row-stream injection spec (where the
    /// box renders inline + its title), or `None` when no composer is open.
    pub(super) fn composer_spec(&self) -> Option<ComposerSpec> {
        let c = self.composer.as_ref()?;
        let (anchor, title) = match &c.target {
            ComposeTarget::NewThread {
                file_idx,
                side,
                start: _,
                end,
            } => {
                // The box renders right under the selected line(s), so the
                // file/range is obvious from context — keep the title bare.
                (
                    ComposerAnchor::NewThread {
                        file_idx: *file_idx,
                        side: *side,
                        // Anchor the composer after the *last* line of the
                        // range (GitHub-style), matching where the resulting
                        // thread box renders (see `last_anchor_lines`).
                        line: *end,
                    },
                    " new comment ".into(),
                )
            }
            ComposeTarget::Reply { thread_id } => (
                ComposerAnchor::Reply {
                    thread_id: thread_id.clone(),
                },
                " reply ".into(),
            ),
        };
        Some(ComposerSpec {
            anchor,
            title,
            body: body_with_caret(&c.textarea),
        })
    }

    /// Whether the active row at `i` is a line of the inline composer box.
    pub(super) fn is_composer_at(&self, i: usize) -> bool {
        match self.view {
            View::Unified => matches!(
                self.rows.get(i).map(|r| &r.kind),
                Some(RowKind::Composer(_))
            ),
            View::Split => matches!(
                self.split_rows.get(i).map(|r| &r.kind),
                Some(SplitRowKind::Composer { .. })
            ),
        }
    }

    /// Whether the active row at `i` is the composer body row that carries the
    /// caret glyph. With cursor movement the caret can sit on any wrapped body
    /// line (not just the last), so scrolling keys off the glyph itself.
    pub(super) fn is_composer_caret_at(&self, i: usize) -> bool {
        let body = match self.view {
            View::Unified => match self.rows.get(i).map(|r| &r.kind) {
                Some(RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Body(s),
                })) => s,
                _ => return false,
            },
            View::Split => match self.split_rows.get(i).map(|r| &r.kind) {
                Some(SplitRowKind::Composer {
                    line:
                        ComposerLine {
                            kind: ComposerKind::Body(s),
                        },
                    ..
                }) => s,
                _ => return false,
            },
        };
        body.contains(COMPOSER_CARET)
    }

    /// Scroll so the (contiguous) inline composer box is in view, anchored to
    /// the body row carrying the caret. The cursor can be on any line now, so
    /// when the box is taller than the viewport we keep the caret row on screen
    /// rather than pinning the top or the bottom.
    pub(super) fn ensure_composer_visible(&mut self) {
        let (s, e) = self.file_range();
        let Some(first) = (s..e).find(|&i| self.is_composer_at(i)) else {
            return;
        };
        let last = (first..e)
            .take_while(|&i| self.is_composer_at(i))
            .last()
            .unwrap_or(first);
        // Anchor scroll to the row carrying the caret glyph (the cursor line),
        // wherever it is in the box — falling back to the last row if the glyph
        // somehow isn't found, so we never leave the box fully off-screen.
        let caret = (first..=last)
            .find(|&i| self.is_composer_caret_at(i))
            .unwrap_or(last);
        let height = self.height.max(1);
        // Caret below the fold: scroll down so it's the last visible row.
        if caret >= self.scroll + height {
            self.scroll = (caret + 1).saturating_sub(height).max(s);
        }
        // Caret above the viewport (cursor moved up in a tall box): scroll up to
        // it.
        if caret < self.scroll {
            self.scroll = caret.max(s);
        }
        // When the whole box fits, prefer showing its top.
        let fits = last - first < height;
        if first < self.scroll && fits {
            self.scroll = first.max(s);
        }
    }

    /// Open the composer for a new thread anchored to the current selection —
    /// the cursor line, or a multi-line range from visual mode (`v`) or a mouse
    /// drag (see [`Self::selection_range`]).
    pub(super) fn open_new_thread(&mut self) {
        let Some((file_idx, side, start, end)) = self.selection_range() else {
            self.status = "put the cursor on a diff line first".into();
            return;
        };
        // A drag could be in flight when the composer opens via the keyboard;
        // clear it so the swallowed mouse-up can't leave us stuck mid-drag.
        self.resizing = false;
        self.sb_drag = None;
        self.composer = Some(Composer {
            target: ComposeTarget::NewThread {
                file_idx,
                side,
                start,
                end,
            },
            textarea: TextArea::default(),
        });
        self.status = "new comment — enter for newline, ctrl+s to submit, esc to cancel".into();
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Open the composer to reply to the focused thread (the comment the cursor
    /// is on, or the thread anchored to the focused diff line).
    pub(super) fn open_reply(&mut self) {
        let Some(id) = self.focused_thread_id() else {
            self.status = "no comment thread here".into();
            return;
        };
        self.open_reply_to(id);
    }

    /// Open the composer to reply to a specific thread (used by the reply
    /// button, which acts on the clicked box regardless of cursor position).
    pub(super) fn open_reply_to(&mut self, thread_id: String) {
        self.resizing = false;
        self.sb_drag = None;
        self.composer = Some(Composer {
            target: ComposeTarget::Reply { thread_id },
            textarea: TextArea::default(),
        });
        self.status = "reply — enter for newline, ctrl+s to submit, esc to cancel".into();
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Cancel the open composer (button equivalent of Esc).
    pub(super) fn cancel_compose(&mut self) {
        self.composer = None;
        self.visual = false;
        self.sel_anchor = None;
        self.status = "cancelled".into();
        self.rebuild_rows();
    }

    /// Insert pasted text in one shot (bracketed paste). Only meaningful while
    /// the composer is open; elsewhere a paste is ignored rather than being
    /// replayed as commands. A single rebuild keeps a multi-paragraph paste from
    /// triggering one full row rebuild per character.
    pub(super) fn on_paste(&mut self, text: String) {
        let Some(c) = self.composer.as_mut() else {
            return;
        };
        // Normalize newlines; `insert_str` splits on `\n` into the buffer at the
        // cursor (which then sits after the inserted text).
        c.textarea
            .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
        self.rebuild_rows();
        self.ensure_composer_visible();
    }

    /// Keystrokes while the composer modal is open. hew's own chords (submit /
    /// cancel) are handled here; everything else is forwarded to the `TextArea`
    /// model, which provides readline/emacs editing (Ctrl+A/E/B/F/K/U/W,
    /// Alt+B/F, arrows, ↑/↓ line moves, Ctrl+D delete-forward, undo, …).
    pub(super) fn on_key_compose(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            // Esc or Ctrl-C cancels without saving. (Ctrl+D is left to the
            // editor as delete-forward, per readline.)
            KeyCode::Esc => {
                self.composer = None;
                self.visual = false;
                self.sel_anchor = None;
                self.status = "cancelled".into();
            }
            KeyCode::Char('c') if ctrl => {
                self.composer = None;
                self.visual = false;
                self.sel_anchor = None;
                self.status = "cancelled".into();
            }
            // Ctrl+S is the primary submit: it's a C0 control byte, so it
            // survives tmux/SSH without any keyboard-protocol negotiation
            // (raw mode clears IXON, so there's no XOFF freeze). Ctrl+Enter is
            // kept as a GitHub-style alias for terminals that forward the kitty
            // keyboard-enhancement protocol (DISAMBIGUATE_ESCAPE_CODES, enabled
            // in `run`); under tmux that protocol is usually swallowed, which is
            // why a protocol-free fallback exists at all. A bare Enter inserts a
            // newline (handled by the editor). Shift+Enter is intentionally not
            // used: the protocol reports ctrl+key but not plain shift+key, so it
            // would be indistinguishable from a bare Enter.
            KeyCode::Char('s') if ctrl => self.submit_compose(),
            KeyCode::Enter if ctrl => self.submit_compose(),
            // Line start / end. tui-textarea binds these to Ctrl+A / Ctrl+E,
            // but its match arms require a lowercase `Char('a')` with no Alt;
            // some terminals (kitty keyboard protocol) report the chord with
            // the Shift bit set or as uppercase, which slips through and does
            // nothing. Drive the move ourselves so the binding is deterministic
            // regardless of how the terminal encodes it.
            KeyCode::Char('a') | KeyCode::Char('A') if ctrl => {
                if let Some(c) = self.composer.as_mut() {
                    c.textarea.move_cursor(tui_textarea::CursorMove::Head);
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') if ctrl => {
                if let Some(c) = self.composer.as_mut() {
                    c.textarea.move_cursor(tui_textarea::CursorMove::End);
                }
            }
            // Everything else: hand the key to the edit model. We always rebuild
            // afterward (below) rather than keying off `input()`'s return value:
            // it reports whether the *text* changed, so cursor-only moves (←/→,
            // Ctrl+A/E, ↑/↓, …) return false even though the caret moved and the
            // row stream needs to redraw it.
            _ => {
                let Some(c) = self.composer.as_mut() else {
                    return;
                };
                c.textarea.input(KeyEvent::new(code, mods));
            }
        }
        // The composer is part of the row stream now, so any state change
        // (typed text, cancel) must rebuild rows to reflect it. `submit_compose`
        // already rebuilds; a redundant rebuild here is cheap and harmless.
        self.rebuild_rows();
        if self.composer.is_some() {
            self.ensure_composer_visible();
        }
    }

    /// Commit the composer's text as a new thread or a reply.
    pub(super) fn submit_compose(&mut self) {
        let Some(c) = self.composer.take() else {
            return;
        };
        let body = c.textarea.lines().join("\n").trim().to_string();
        if body.is_empty() {
            self.status = "empty comment discarded".into();
            return;
        }
        match c.target {
            ComposeTarget::NewThread {
                file_idx,
                side,
                start,
                end,
            } => {
                let Some(file) = self.changeset.files.get(file_idx) else {
                    // Defensive: the anchor's file index should always be valid
                    // (the changeset is fixed for the session).
                    self.status = "comment discarded — unknown file".into();
                    return;
                };
                let path = PathBuf::from(file.display_path());
                self.comments.add_thread(
                    path,
                    side,
                    LineRange { start, end },
                    Some("you".into()),
                    body,
                );
                // Leaving the composer also leaves visual mode.
                self.visual = false;
                self.sel_anchor = None;
                self.status = "added comment".into();
            }
            ComposeTarget::Reply { thread_id } => {
                if self.comments.reply(&thread_id, Some("you".into()), body) {
                    self.status = "added reply".into();
                } else {
                    self.status = "thread no longer exists".into();
                }
            }
        }
        self.rebuild_rows();
    }

    /// The id of the first comment thread anchored to the selected line, if any.
    pub(super) fn current_thread_id(&self) -> Option<String> {
        let (fi, side, line) = self.anchor_at(self.selected)?;
        let file = self.changeset.files.get(fi)?;
        let path = Path::new(file.display_path());
        self.comments
            .threads
            .iter()
            .find(|t| t.file.as_path() == path && t.side == side && t.range.contains(line))
            .map(|t| t.id.clone())
    }

    /// Toggle the resolved state of the focused thread.
    pub(super) fn resolve_current_thread(&mut self) {
        let Some(id) = self.focused_thread_id() else {
            self.status = "no comment thread here".into();
            return;
        };
        self.toggle_resolved_thread(id);
    }

    /// Toggle resolved on a specific thread (used by the resolve button).
    pub(super) fn toggle_resolved_thread(&mut self, id: String) {
        match self.comments.toggle_resolved(&id) {
            Some(true) => self.status = "resolved thread".into(),
            Some(false) => self.status = "unresolved thread".into(),
            None => return,
        }
        self.rebuild_rows();
    }

    /// Delete the focused *comment* — but only if it was added in this session.
    /// The unit of deletion is a single comment (e.g. a reply you wrote), never
    /// a whole thread; removing a thread's last comment drops the thread.
    /// Comments loaded from the input sidecar are immutable, so `D` on one is a
    /// no-op — which is what keeps the action log free of delete actions.
    pub(super) fn delete_current_comment(&mut self) {
        let Some((thread_id, comment_id)) = self.focused_comment() else {
            self.status = "put the cursor on a comment to delete".into();
            return;
        };
        self.delete_comment(thread_id, comment_id);
    }

    /// Delete a specific session-added comment (used by the delete button).
    pub(super) fn delete_comment(&mut self, thread_id: String, comment_id: String) {
        if self.base_comment_ids.contains(&comment_id) {
            self.status = "can't delete a comment from the input".into();
            return;
        }
        if self.comments.remove_comment(&thread_id, &comment_id) {
            self.status = "deleted comment".into();
            self.rebuild_rows();
        }
    }
}
