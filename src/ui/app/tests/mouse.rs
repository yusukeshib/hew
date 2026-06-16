use super::*;

#[test]
fn comment_box_records_reply_and_resolve_buttons() {
    let (mut app, tid, _rid) = app_with_thread(2);
    render(&mut app, 160, 40);
    let hits = app.button_hits.borrow();
    assert!(
        hits.iter()
            .any(|(_, a)| matches!(a, ButtonAction::Reply(t) if *t == tid)),
        "reply button should be recorded"
    );
    assert!(
        hits.iter()
            .any(|(_, a)| matches!(a, ButtonAction::ToggleResolve(t) if *t == tid)),
        "resolve button should be recorded"
    );
}

#[test]
fn clicking_the_reply_button_opens_the_composer() {
    let (mut app, tid, _rid) = app_with_thread(2);
    render(&mut app, 160, 40);
    let rect = app
        .button_hits
        .borrow()
        .iter()
        .find(|(_, a)| matches!(a, ButtonAction::Reply(t) if *t == tid))
        .map(|(r, _)| *r)
        .expect("reply button recorded");
    click(&mut app, rect);
    assert!(app.composer.is_some(), "clicking reply opens the composer");
}

#[test]
fn clicking_the_resolve_button_toggles_the_thread() {
    let (mut app, _tid, _rid) = app_with_thread(2);
    render(&mut app, 160, 40);
    let before = app.comments.threads[0].resolved;
    let rect = app
        .button_hits
        .borrow()
        .iter()
        .find(|(_, a)| matches!(a, ButtonAction::ToggleResolve(_)))
        .map(|(r, _)| *r)
        .expect("resolve button recorded");
    click(&mut app, rect);
    assert_ne!(
        before, app.comments.threads[0].resolved,
        "clicking resolve toggles the thread"
    );
}

// Index of the (single) composer row in the active list.
fn composer_row(app: &App) -> usize {
    (0..app.active_len())
        .find(|&i| app.is_composer_at(i))
        .expect("composer row")
}

#[test]
fn clicking_resolve_anchors_the_cursor_to_the_clicked_thread() {
    // Regression (bug1): with the cursor parked at the file top (e.g. after a wheel
    // scroll, which leaves the selection untouched), clicking a thread's
    // resolve button used to re-anchor the rebuild to the parked cursor and
    // snap the viewport to the top. The click must move the cursor to the
    // resolved thread so it stays on screen.
    let (mut app, tid, _rid) = app_with_thread(2);
    render(&mut app, 160, 40);
    app.selected = app.first_selectable().unwrap(); // park at the top
    let rect = app
        .button_hits
        .borrow()
        .iter()
        .find(|(_, a)| matches!(a, ButtonAction::ToggleResolve(_)))
        .map(|(r, _)| *r)
        .expect("resolve button recorded");
    click(&mut app, rect);
    assert_eq!(
        app.focused_thread_id().as_deref(),
        Some(tid.as_str()),
        "cursor should land on the resolved thread, not the parked top row"
    );
}

#[test]
fn clicking_reply_keeps_the_composer_in_view() {
    // Regression (bug2): with a small viewport scrolled down to a thread while
    // the cursor was parked at the file top, the reply rebuild re-anchored to
    // the parked cursor and the composer could land outside the viewport. The
    // click must anchor the cursor to the replied thread so the composer is
    // shown.
    let (mut app, tid, _rid) = app_with_thread(2);
    render(&mut app, 160, 40);
    app.height = 4; // small viewport so scroll position matters
    app.selected = app.first_selectable().unwrap(); // park at the top
    let rect = app
        .button_hits
        .borrow()
        .iter()
        .find(|(_, a)| matches!(a, ButtonAction::Reply(t) if *t == tid))
        .map(|(r, _)| *r)
        .expect("reply button recorded");
    click(&mut app, rect);
    assert!(app.composer.is_some(), "reply opens the composer");
    assert_eq!(
        app.focused_thread_id().as_deref(),
        Some(tid.as_str()),
        "cursor should anchor to the replied thread"
    );
    let row = composer_row(&app);
    assert!(
        row >= app.scroll && row < app.scroll + app.height,
        "composer row {row} should be within the viewport [{}, {})",
        app.scroll,
        app.scroll + app.height
    );
}

#[test]
fn range_comment_box_renders_after_the_last_line() {
    // A New-side range 2..=4 should inject its box right after the row for
    // line 4 (GitHub-style: below the last line), not after line 2.
    let cs = parse_report(DIFF).0;
    let mut store = CommentStore::default();
    store.add_thread(
        "f.rs".into(),
        Side::New,
        LineRange { start: 2, end: 4 },
        Some("a".into()),
        "msg".into(),
    );
    let app = App::with_comments(cs, store);
    let line4 = app
        .rows
        .iter()
        .position(|r| {
            matches!(
                &r.kind,
                RowKind::Line {
                    new_line: Some(4),
                    ..
                }
            )
        })
        .unwrap();
    let top = app
        .rows
        .iter()
        .position(
            |r| matches!(&r.kind, RowKind::Comment(cl) if matches!(cl.kind, CommentKind::Top)),
        )
        .unwrap();
    assert_eq!(
        top,
        line4 + 1,
        "box should immediately follow the last range line"
    );
}

#[test]
fn cursor_diff_line_shows_a_clickable_add_comment_button() {
    let mut app = app_with(DIFF);
    app.focus = Focus::Diff;
    goto(&mut app, Side::New, 2);
    render(&mut app, 120, 40);
    let rect = app
        .button_hits
        .borrow()
        .iter()
        .find(|(_, a)| matches!(a, ButtonAction::AddComment))
        .map(|(r, _)| *r)
        .expect("add-comment button recorded on the cursor line");
    click(&mut app, rect);
    assert!(
        app.composer.is_some(),
        "clicking comment(i) opens a new-thread composer"
    );
}

#[test]
fn composer_records_submit_and_cancel_buttons() {
    let (mut app, _tid, _rid) = app_with_thread(2);
    app.focus = Focus::Diff;
    app.jump_comment(1);
    app.open_reply();
    render(&mut app, 160, 40);
    let hits = app.button_hits.borrow();
    assert!(hits.iter().any(|(_, a)| matches!(a, ButtonAction::Submit)));
    assert!(hits.iter().any(|(_, a)| matches!(a, ButtonAction::Cancel)));
}
