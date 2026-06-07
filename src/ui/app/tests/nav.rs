use super::*;

#[test]
fn precomputed_file_spans_match_a_full_scan() {
    // The O(1) per-file-switch span lookup must agree with a brute-force
    // scan of the active row list, in both layouts.
    let mut app = app_with(TWO_FILES);
    for toggled in [false, true] {
        if toggled {
            app.toggle_view();
        }
        let len = app.active_len();
        for fi in 0..app.changeset.files.len() {
            app.current_file = fi;
            app.recompute_file_span();
            let (mut s, mut e) = (len, len);
            for i in 0..len {
                if app.row_file_idx(i) == Some(fi) {
                    if s == len {
                        s = i;
                    }
                    e = i + 1;
                }
            }
            assert_eq!(
                app.file_span,
                (s, e),
                "file {fi} span mismatch (toggled={toggled})"
            );
        }
    }
}

#[test]
fn navigation_clamps_within_the_file() {
    let mut app = app_with(DIFF);
    let first = app.first_selectable().unwrap();
    let last = app.last_selectable().unwrap();
    app.selected = first;

    // Up past the top is a no-op.
    app.move_by(-1, 5);
    assert_eq!(app.selected, first);

    // Down past the bottom clamps to the last selectable row.
    app.move_by(1, 100);
    assert_eq!(app.selected, last);
}

#[test]
fn ensure_visible_keeps_cursor_in_viewport() {
    let mut app = app_with(DIFF);
    let (start, end) = app.file_range();
    app.selected = app.last_selectable().unwrap();
    app.ensure_visible();
    let h = app.height;
    assert!(app.scroll <= app.selected, "cursor above viewport");
    assert!(app.selected < app.scroll + h, "cursor below viewport");
    // Scroll never leaves the file's row slice.
    assert!(app.scroll >= start && app.scroll < end);
}

#[test]
fn visual_selection_spans_a_multi_line_range() {
    let mut app = app_with(DIFF);
    goto(&mut app, Side::New, 2);
    app.toggle_visual();
    assert!(app.visual && app.sel_anchor.is_some());
    goto(&mut app, Side::New, 5);

    // The selection covers new-side lines 2..=5 on a single side.
    let (fi, side, lo, hi) = app.selection_range().unwrap();
    assert_eq!((fi, side, lo, hi), (app.current_file, Side::New, 2, 5));

    // Leaving visual drops the anchor.
    app.toggle_visual();
    assert!(!app.visual && app.sel_anchor.is_none());
}

#[test]
fn shift_arrows_extend_a_line_selection() {
    // Shift+Down/Up builds a multi-line selection without entering `v`
    // visual mode, anchoring at the starting line. `visual` stays false so
    // a later unmodified move collapses the range.
    let mut app = app_with(DIFF);
    goto(&mut app, Side::New, 2);
    assert!(!app.visual && app.sel_anchor.is_none());

    app.on_key_diff(KeyCode::Down, false, true);
    app.on_key_diff(KeyCode::Down, false, true);
    assert!(!app.visual && app.sel_anchor.is_some());
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 4)
    );

    // Shrinking back up narrows the range.
    app.on_key_diff(KeyCode::Up, false, true);
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 3)
    );
}

#[test]
fn unmodified_move_collapses_a_shift_selection() {
    // Regression: Shift+arrow used to flip on persistent visual mode, so the
    // multi-select stayed active after releasing Shift (terminals can't
    // report Shift key-up). A plain j/k must collapse the range back to a
    // single line.
    let mut app = app_with(DIFF);
    goto(&mut app, Side::New, 2);
    app.on_key_diff(KeyCode::Down, false, true);
    app.on_key_diff(KeyCode::Down, false, true);
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 4),
        "shift extended the range"
    );

    // A plain (no-shift) Down collapses to the single cursor line.
    app.on_key_diff(KeyCode::Down, false, false);
    assert!(app.sel_anchor.is_none(), "plain move must drop the anchor");
    let (_, side, lo, hi) = app.selection_range().unwrap();
    assert_eq!(
        (side, lo, hi),
        (Side::New, 5, 5),
        "range collapsed to one line"
    );

    // A fresh Shift+arrow starts a brand-new range from here.
    app.on_key_diff(KeyCode::Down, false, true);
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 5, 6)
    );
}

#[test]
fn shift_arrows_skip_comment_rows_and_keep_a_valid_line_range() {
    // Regression: extend_selection used to step to the next *stop*, which
    // includes comment/collapsed rows — landing the cursor off a diff line
    // so selection_range() went None. It must skip over an inline thread's
    // rows and keep selecting diff lines.
    let (mut app, _tid, _reply) = app_with_thread(3);
    goto(&mut app, Side::New, 2);

    // Walk down across the line-3 thread's inline rows; every step must stay
    // on a diff line with a valid line range.
    for _ in 0..3 {
        app.on_key_diff(KeyCode::Down, false, true);
        assert!(
            app.is_selectable_at(app.selected),
            "cursor landed on a non-diff row"
        );
        assert!(
            app.selection_range().is_some(),
            "selection range went None mid-extend"
        );
    }
    // The range spans diff lines (start stays at the anchor, end advanced).
    let (_, side, lo, hi) = app.selection_range().unwrap();
    assert_eq!((side, lo), (Side::New, 2));
    assert!(hi > lo, "selection should have grown past the anchor line");
}

#[test]
fn toggling_view_preserves_a_multi_line_selection() {
    // Regression: switching unified<->split used to collapse a visual
    // selection down to the cursor line. The whole range must survive.
    let mut app = app_with(DIFF);
    goto(&mut app, Side::New, 2);
    app.toggle_visual();
    goto(&mut app, Side::New, 5);
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 5)
    );

    app.toggle_view(); // -> split
    assert!(app.visual && app.sel_anchor.is_some());
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 5),
        "selection collapsed after toggling to split"
    );

    app.toggle_view(); // -> back to unified
    assert_eq!(
        app.selection_range().unwrap(),
        (app.current_file, Side::New, 2, 5),
        "selection collapsed after toggling back to unified"
    );
}

#[test]
fn selection_range_is_single_line_without_an_anchor() {
    let mut app = app_with(DIFF);
    goto(&mut app, Side::New, 3);
    let (_, side, lo, hi) = app.selection_range().unwrap();
    assert_eq!((side, lo, hi), (Side::New, 3, 3));
}

#[test]
fn jumping_files_moves_the_cursor_into_the_new_file() {
    let mut app = app_with(TWO_FILES);
    assert_eq!(app.current_file, 0);
    app.jump_file(1);
    assert_eq!(app.current_file, 1);
    // The cursor lands on a selectable row belonging to file 1.
    assert!(app.is_selectable_at(app.selected));
    assert_eq!(app.row_file_idx(app.selected), Some(1));
    // ...and back.
    app.jump_file(-1);
    assert_eq!(app.current_file, 0);
    assert_eq!(app.row_file_idx(app.selected), Some(0));
}
