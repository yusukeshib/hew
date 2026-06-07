use super::*;

#[test]
fn delete_targets_session_comments_only() {
    // Both the root and the reply here come from the input sidecar.
    let (mut app, tid, base_reply_id) = app_with_thread(3);

    // Cursor on an input comment: `D` is a no-op.
    app.selected = comment_head(&app, &base_reply_id);
    app.delete_current_comment();
    assert_eq!(app.status, "can't delete a comment from the input");
    assert_eq!(
        app.comments.threads[0].comments.len(),
        2,
        "an input comment must survive D"
    );

    // Add a reply this session (to the same base thread), then delete it.
    app.selected = comment_head(&app, &base_reply_id);
    app.open_reply();
    app.on_key_compose(KeyCode::Char('y'), KeyModifiers::NONE);
    app.submit_compose();
    assert_eq!(app.comments.threads[0].comments.len(), 3);
    let new_reply_id = app.comments.threads[0].comments[2].id.clone();

    app.selected = comment_head(&app, &new_reply_id);
    app.delete_current_comment();
    assert_eq!(app.status, "deleted comment");
    assert_eq!(
        app.comments.threads[0].comments.len(),
        2,
        "only the session reply is removed"
    );
    assert!(
        app.comments.threads.iter().any(|t| t.id == tid),
        "the thread (and its input comments) survives"
    );
}

#[test]
fn deleting_a_session_thread_last_comment_drops_the_thread() {
    // A wholly in-session thread: deleting its only comment removes it.
    let (mut app, _tid, _reply) = app_with_thread(3);
    goto(&mut app, Side::New, 1);
    app.open_new_thread();
    app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
    app.submit_compose();
    let new_tid = app
        .comments
        .threads
        .iter()
        .find(|t| t.range.contains(1) && t.side == Side::New)
        .expect("new thread")
        .id
        .clone();
    let cid = app
        .comments
        .threads
        .iter()
        .find(|t| t.id == new_tid)
        .unwrap()
        .comments[0]
        .id
        .clone();
    app.selected = comment_head(&app, &cid);
    app.delete_current_comment();
    assert!(
        !app.comments.threads.iter().any(|t| t.id == new_tid),
        "emptying a thread drops it"
    );
}

#[test]
fn comments_are_navigable_stops() {
    let (mut app, _tid, reply_id) = app_with_thread(3);
    // Land on the diff line the thread anchors to, then walk down: we must
    // eventually stop on each comment message (a stop that is not a line).
    goto(&mut app, Side::New, 3);
    let mut comment_stops = 0;
    for _ in 0..40 {
        app.move_by(1, 1);
        if app.comment_unit_at(app.selected).is_some() {
            comment_stops += 1;
        }
    }
    assert!(
        comment_stops >= 2,
        "navigation should stop on each comment message (got {comment_stops})"
    );
    // And the reply message is reachable as its own stop.
    let head = comment_head(&app, &reply_id);
    assert!(app.is_stop_at(head));
}

#[test]
fn paste_inserts_into_composer_and_is_ignored_otherwise() {
    // Outside the composer a paste is a no-op (not replayed as commands).
    let mut app = app_with(DIFF);
    app.on_paste("qqq deletes nothing".into());
    assert!(!app.quit);
    assert!(app.composer.is_none());

    // Inside the composer the whole multi-line paste lands in one shot,
    // with CRLF/CR normalized to `\n`.
    open_composer(&mut app);
    app.on_paste("first line\r\nsecond line\rthird".into());
    assert_eq!(
        app.composer.as_ref().unwrap().textarea.lines().join("\n"),
        "first line\nsecond line\nthird"
    );
    assert!(app.composer.is_some(), "paste must not submit");
}

#[test]
fn resolved_thread_comment_shows_focus_border() {
    // Regression: a resolved thread's box was always drawn with the muted
    // border, even when the cursor was on it — so an individual comment in a
    // resolved thread gave no visual "selected" feedback. Focus must win.
    let (mut app, tid, reply_id) = app_with_thread(3);
    app.comments.toggle_resolved(&tid);
    app.rebuild_rows();
    assert!(app.comments.threads[0].resolved);

    // The reply's individual comment is still a reachable stop...
    let head = comment_head(&app, &reply_id);
    assert!(app.is_stop_at(head));

    // ...and when focused, its box border is the focus color, not muted.
    let cl = app.comment_at(head).unwrap().clone();
    let focused = app.comment_line_to_line(&cl, true, 40);
    let unfocused = app.comment_line_to_line(&cl, false, 40);
    let border_fg = |line: &ratatui::text::Line| {
        // The box border span (`╭`/`│`/`╰`) carries the border color.
        line.spans
            .iter()
            .find(|s| s.content.chars().any(|c| "╭╮╰╯│".contains(c)))
            .and_then(|s| s.style.fg)
    };
    assert_eq!(border_fg(&focused), Some(theme().border_focus));
    assert_eq!(border_fg(&unfocused), Some(theme().muted));
}

#[test]
fn split_view_wraps_comment_body_into_the_half_column() {
    // Regression: comment bodies were wrapped to the full diff width but
    // rendered into a half-width column in split view, so every line got
    // clipped on the right. `sync_comment_wrap` must wrap to the split
    // column width.
    let cs = parse_report(DIFF).0;
    let mut store = CommentStore::default();
    store.add_thread(
        "f.rs".into(),
        Side::New,
        LineRange { start: 2, end: 2 },
        Some("you".into()),
        "The labor market has shifted into a higher gear, powering through \
             an energy shock and immigration restrictions to pull more people."
            .into(),
    );
    let mut app = App::with_comments(cs, store);
    app.view = View::Split;
    app.wrap = false;

    // Mirror render_split's column math for a known inner width.
    let inner: u16 = 90;
    app.sync_comment_wrap(inner);
    // Worst case (scrollbar present) side column, as render computes it.
    let side_w = (inner as usize).saturating_sub(1 + str_width(SPLIT_DIVIDER)) / 2;
    let inner_w = side_w - 2; // borders
    let indent = 3; // columns reserved for the body indent in comment_wrap

    // Every wrapped body fragment must fit the rendered half-column with its
    // indent — i.e. it is never clipped by `take_width`.
    let mut body_rows = 0;
    for i in 0..app.split_rows.len() {
        if let SplitRowKind::Comment { line, .. } = &app.split_rows[i].kind {
            if let CommentKind::Body(b) = &line.kind {
                body_rows += 1;
                assert!(
                    str_width(b) + indent <= inner_w,
                    "body fragment {:?} ({}+{}) exceeds split inner width {}",
                    b,
                    str_width(b),
                    indent,
                    inner_w
                );
            }
        }
    }
    assert!(body_rows >= 2, "long body should wrap to several rows");
}

#[test]
fn focusing_a_comment_selects_the_whole_message_and_its_thread() {
    let (mut app, tid, reply_id) = app_with_thread(3);
    let head = comment_head(&app, &reply_id);
    app.selected = head;

    // The focused-thread action target is the comment's thread.
    assert_eq!(app.focused_thread_id(), Some(tid.clone()));
    assert_eq!(app.focused_comment(), Some((tid, reply_id)));

    // Every row of the message (and only those) is in the selection.
    let (lo, hi) = app.comment_unit_span(head).unwrap();
    assert!(hi >= lo);
    for i in lo..=hi {
        assert!(
            app.in_selection(i),
            "row {i} of the message should highlight"
        );
    }
    assert!(!app.in_selection(lo.saturating_sub(1)) || lo == 0);
    assert!(!app.in_selection(hi + 1));
}

#[test]
fn comment_selection_survives_view_toggle() {
    let (mut app, tid, reply_id) = app_with_thread(3);
    app.selected = comment_head(&app, &reply_id);
    app.toggle_view(); // unified <-> split
    assert_eq!(
        app.focused_comment(),
        Some((tid, reply_id)),
        "the same comment should stay focused across a view switch"
    );
}
