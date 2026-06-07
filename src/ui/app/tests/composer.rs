use super::*;

#[test]
fn composer_supports_readline_cursor_editing() {
    let mut app = app_with(DIFF);
    open_composer(&mut app);
    // Type "ac", move left (←), insert "b" between them — cursor editing.
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('c'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Left, KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(composer_text(&app), "abc");

    // Ctrl+A jumps to line start; typed text lands there.
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL);
    app.on_key_compose(KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(composer_text(&app), "Xabc");

    // Ctrl+E jumps to line end.
    app.on_key_compose(KeyCode::Char('e'), KeyModifiers::CONTROL);
    app.on_key_compose(KeyCode::Char('Z'), KeyModifiers::NONE);
    assert_eq!(composer_text(&app), "XabcZ");
}

#[test]
fn cursor_move_rebuilds_the_rendered_composer() {
    // Regression: a cursor-only key (←) doesn't change the text, so
    // TextArea::input returns false. The row stream must still be rebuilt,
    // or the drawn caret would stay put while the real cursor moved.
    let mut app = app_with(DIFF);
    app.toggle_view(); // Split -> Unified, so the caret rides a `Row`
    open_composer(&mut app);
    // Tests never draw, so `comment_wrap` would be 0 and wrap each glyph
    // onto its own line; give the body a real width so it stays one line.
    app.comment_wrap = 40;
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Left, KeyModifiers::NONE);
    let body = app
        .rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Composer(ComposerLine {
                kind: ComposerKind::Body(s),
            }) if s.contains(COMPOSER_CARET) => Some(s.clone()),
            _ => None,
        })
        .expect("a composer body row carrying the caret");
    assert_eq!(
        body,
        format!("a{COMPOSER_CARET}b"),
        "the caret marker must follow the cursor"
    );
}

#[test]
fn composer_caret_renders_at_the_cursor() {
    let mut app = app_with(DIFF);
    open_composer(&mut app);
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL); // to start
    let spec = app.composer_spec().expect("composer spec");
    assert_eq!(
        spec.body,
        format!("{COMPOSER_CARET}ab"),
        "caret renders at the cursor, not the end"
    );
}

#[test]
fn composer_body_overlay_fills_exactly_the_inner_width() {
    // Regression: the caret-overlay path must honor the same width contract
    // as `fit` — a body longer than the box is truncated, never overflowed.
    let app = app_with(DIFF);
    let width = 12; // inner_w = width - margin(2) - borders(2) = 8
    let cl = ComposerLine {
        kind: ComposerKind::Body(format!("{COMPOSER_CARET}abcdefghijklmnopqrstuvwxyz")),
    };
    let line = app.composer_line_to_line(&cl, width);
    let total: usize = line.spans.iter().map(|s| str_width(&s.content)).sum();
    assert_eq!(
        total, width,
        "composer body line must fill exactly the row width, even when truncated"
    );
}

#[test]
fn ctrl_d_deletes_forward_and_does_not_cancel() {
    // Ctrl+D is readline delete-forward now, not a cancel chord.
    let mut app = app_with(DIFF);
    open_composer(&mut app);
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::CONTROL); // to start
    app.on_key_compose(KeyCode::Char('d'), KeyModifiers::CONTROL); // delete 'a'
    assert!(app.composer.is_some(), "Ctrl+D must not cancel");
    assert_eq!(composer_text(&app), "b");
}

#[test]
fn composer_keeps_caret_visible_when_taller_than_viewport() {
    // Regression: typing a long comment scrolled the box's *top* into view
    // and pushed the caret (its bottom body line) off-screen below. The
    // viewport must follow the caret instead.
    let mut app = app_with(DIFF);
    app.height = 6; // viewport far shorter than the box
    app.toggle_view(); // default Split -> Unified (refreshes file span)
    assert!(matches!(app.view, View::Unified));
    open_composer(&mut app);
    for _ in 0..200 {
        app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
    }
    app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
    app.ensure_composer_visible();

    let (s, e) = app.file_range();
    // The caret rides the last composer Body row.
    let caret = (s..e)
        .rev()
        .find(|&i| {
            matches!(
                app.rows.get(i).map(|r| &r.kind),
                Some(RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Body(_)
                }))
            )
        })
        .expect("composer body row");
    assert!(
        caret >= app.scroll && caret < app.scroll + app.height,
        "caret row {caret} must stay within view [{}, {})",
        app.scroll,
        app.scroll + app.height
    );
}

#[test]
fn composer_caret_visible_on_a_tiny_viewport() {
    // The caret sits on the last Body row, with Hint+Bottom chrome below it.
    // Anchoring to the box's bottom border (height < 3) would leave the
    // caret off-screen, so the scroll must follow the body row instead.
    let mut app = app_with(DIFF);
    app.height = 2;
    app.toggle_view(); // default Split -> Unified
    open_composer(&mut app);
    for _ in 0..50 {
        app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
    }
    app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
    app.ensure_composer_visible();

    let (s, e) = app.file_range();
    let caret = (s..e)
        .find(|&i| app.is_composer_caret_at(i))
        .expect("composer caret row");
    assert!(
        caret >= app.scroll && caret < app.scroll + app.height,
        "caret row {caret} must stay within view [{}, {}) on a tiny viewport",
        app.scroll,
        app.scroll + app.height
    );
}

#[test]
fn ensure_composer_visible_follows_caret_upward() {
    // Regression: scrolling anchored to the *last* body row, so with the
    // cursor moved up in a box taller than the viewport, a bottom-anchored
    // scroll left the caret off-screen above. The pass must pull the
    // viewport up to the caret row.
    let mut app = app_with(DIFF);
    app.height = 4;
    app.toggle_view(); // Split -> Unified
    open_composer(&mut app);
    app.comment_wrap = 40;
    for _ in 0..40 {
        app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
    }
    app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
    // Cursor to the top of the buffer, then force the viewport to the
    // bottom (as if we'd just been typing at the end) and re-run the pass.
    for _ in 0..45 {
        app.on_key_compose(KeyCode::Up, KeyModifiers::NONE);
    }
    let (_, e) = app.file_range();
    app.scroll = e.saturating_sub(app.height); // bottom-anchored
    app.ensure_composer_visible();

    let (s, e) = app.file_range();
    let caret = (s..e)
        .find(|&i| app.is_composer_caret_at(i))
        .expect("composer caret row");
    assert!(
        caret >= app.scroll && caret < app.scroll + app.height,
        "caret row {caret} must stay within view [{}, {}) when the cursor is up",
        app.scroll,
        app.scroll + app.height
    );
}

#[test]
fn bare_enter_inserts_a_newline_in_the_composer() {
    let mut app = app_with(DIFF);
    open_composer(&mut app);
    app.on_key_compose(KeyCode::Char('a'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Enter, KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('b'), KeyModifiers::NONE);
    // A bare Enter must NOT submit — the composer stays open with a newline.
    assert!(app.composer.is_some(), "bare Enter should not submit");
    assert_eq!(
        app.composer.as_ref().unwrap().textarea.lines().join("\n"),
        "a\nb"
    );
}

#[test]
fn ctrl_enter_submits_the_composer() {
    let (mut app, _tid, _reply) = app_with_thread(3);
    let before = app.comments.threads.len();
    goto(&mut app, Side::New, 1);
    app.open_new_thread();
    app.on_key_compose(KeyCode::Char('h'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('i'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Enter, KeyModifiers::CONTROL);
    // Ctrl+Enter submits: composer closes and a new thread is recorded.
    assert!(app.composer.is_none(), "Ctrl+Enter should submit");
    assert_eq!(app.comments.threads.len(), before + 1);
}

#[test]
fn ctrl_s_submits_the_composer() {
    // The protocol-free primary submit: a C0 control byte that survives
    // tmux/SSH even when the kitty keyboard protocol (and thus Ctrl+Enter)
    // is not forwarded.
    let (mut app, _tid, _reply) = app_with_thread(3);
    let before = app.comments.threads.len();
    goto(&mut app, Side::New, 1);
    app.open_new_thread();
    app.on_key_compose(KeyCode::Char('h'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('i'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(app.composer.is_none(), "Ctrl+S should submit");
    assert_eq!(app.comments.threads.len(), before + 1);
}

#[test]
fn shift_enter_does_not_submit_the_composer() {
    // Regression: Shift+Enter is indistinguishable from a bare Enter under
    // the DISAMBIGUATE_ESCAPE_CODES protocol, so it must behave like one
    // (insert a newline) rather than submit.
    let mut app = app_with(DIFF);
    open_composer(&mut app);
    app.on_key_compose(KeyCode::Char('x'), KeyModifiers::NONE);
    app.on_key_compose(KeyCode::Enter, KeyModifiers::SHIFT);
    assert!(app.composer.is_some(), "Shift+Enter must not submit");
    assert_eq!(
        app.composer.as_ref().unwrap().textarea.lines().join("\n"),
        "x\n"
    );
}
