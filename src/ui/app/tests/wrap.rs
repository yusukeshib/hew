use super::*;

#[test]
fn wrap_count_matches_wrap_runs_for_any_run_split() {
    // The height oracle (`wrap_count`) must agree with the renderer
    // (`wrap_runs`) regardless of how the text is partitioned into colored
    // runs — wrapping breaks only on cumulative display width.
    let red = Color::Red;
    let blue = Color::Blue;
    let long = "x".repeat(50);
    let cases = [
        "",
        "abc",
        "hello world this is a longer sentence to wrap",
        "日本語のテキストを折り返す", // wide glyphs
        "a日b本c語d",
        long.as_str(),
    ];
    for text in cases {
        for budget in [1usize, 2, 3, 4, 7, 10, 13, 80] {
            // Split `text` into two runs at every byte boundary that lands
            // on a char boundary, to exercise run-independence.
            let mut splits = vec![0usize, text.len()];
            for (i, _) in text.char_indices() {
                splits.push(i);
            }
            for &cut in &splits {
                if !text.is_char_boundary(cut) {
                    continue;
                }
                let runs = vec![
                    (red, text[..cut].to_string()),
                    (blue, text[cut..].to_string()),
                ];
                let got = wrap_runs(&runs, budget, None).len();
                assert_eq!(
                    got,
                    wrap_count(text, budget),
                    "text={text:?} budget={budget} cut={cut}"
                );
            }
        }
    }
}

#[test]
fn wrap_off_keeps_one_display_line_per_row() {
    // Regression guard: with wrap off the geometry is the original 1:1
    // model (every row is height 1), so display-line math == row count.
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    render(&mut app, 40, 20);
    assert!(!app.wrap);
    let (s, e) = app.file_range();
    assert_eq!(app.display_lines(s, e), e - s);
    assert!((s..e).all(|i| app.row_h(i) == 1));
}

#[test]
fn long_line_wraps_into_several_display_lines() {
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    app.toggle_wrap();
    assert!(app.wrap);
    // A narrow viewport forces the long addition to wrap.
    render(&mut app, 40, 20);
    let (s, e) = app.file_range();
    // The long addition is the only row that should exceed one line.
    let tall = (s..e).filter(|&i| app.row_h(i) > 1).count();
    assert_eq!(tall, 1, "exactly the long code line wraps");
    assert!(
        app.display_lines(s, e) > e - s,
        "wrapping adds display lines over the row count"
    );
}

#[test]
fn click_below_a_wrapped_line_maps_to_the_next_row() {
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    app.toggle_wrap();
    render(&mut app, 40, 20);
    let (s, e) = app.file_range();
    // Find the wrapped (tall) code row and the next selectable row.
    let tall = (s..e).find(|&i| app.row_h(i) > 1).expect("a wrapped row");
    let next = (tall + 1..e)
        .find(|&i| app.is_selectable_at(i))
        .expect("a row after the wrapped one");
    app.scroll = s;
    // The wrapped row occupies `row_h(tall)` display lines starting at the
    // top of the viewport; clicking just past them must land on `next`,
    // not on the wrapped row's last visual line.
    let mut off = 0usize;
    for i in s..tall {
        off += app.row_h(i);
    }
    off += app.row_h(tall); // first display line of `next`
    let area = app.diff_area;
    let row = area.y + off as u16;
    click(
        &mut app,
        Rect {
            x: area.x + 8,
            y: row,
            width: 1,
            height: 1,
        },
    );
    assert_eq!(app.selected, next, "click maps through the wrapped row");
}

#[test]
fn wrapping_a_real_patch_keeps_heights_and_render_in_sync() {
    // Exercises the `debug_assert!` in render (row_h must equal the lines
    // actually produced) over a real, highlighted patch in both views at a
    // range of widths — the tightest guard against oracle/renderer drift.
    use crate::loader::load_patch;
    let cs = load_patch(Some(Path::new("examples/rust-long-en.patch"))).unwrap();
    let mut app = App::with_comments(cs, CommentStore::default());
    app.wrap = true;
    app.geom.dirty = true;
    for view in [View::Unified, View::Split] {
        if app.view != view {
            app.toggle_view();
        }
        for w in [50u16, 80, 120, 200] {
            render(&mut app, w, 30);
            // Scroll through the whole file so every row is rendered.
            for _ in 0..40 {
                app.move_by(1, app.height.max(1));
                render(&mut app, w, 30);
            }
        }
    }
}

#[test]
fn display_lines_prefix_sum_matches_a_direct_walk() {
    // The O(1) prefix-sum query must equal the naive per-row sum it
    // replaces, for every sub-range of the file.
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    app.toggle_wrap();
    render(&mut app, 40, 20);
    let (s, e) = app.file_range();
    assert_eq!(app.geom.offsets.len(), app.active_len() + 1);
    for a in s..=e {
        for b in a..=e {
            let walk: usize = (a..b).map(|i| app.row_h(i)).sum();
            assert_eq!(app.display_lines(a, b), walk, "range {a}..{b}");
        }
    }
}

#[test]
fn widening_reclamps_scroll_off_the_end() {
    // Regression: after a resize that shrinks wrap heights, a scroll value
    // valid for the narrow geometry must be clamped so the viewport never
    // paints empty space past EOF.
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    app.toggle_wrap();
    // Narrow + short viewport so the long line wraps and content overflows;
    // scroll to the bottom.
    render(&mut app, 30, 4);
    app.scroll_view(100);
    render(&mut app, 30, 4);
    // Now widen a lot: the long line no longer wraps, so far less content.
    render(&mut app, 400, 4);
    let (s, e) = app.file_range();
    assert!(
        app.scroll <= app.max_scroll_row(s, e, app.height.max(1)),
        "scroll {} must be clamped to max {}",
        app.scroll,
        app.max_scroll_row(s, e, app.height.max(1))
    );
}

#[test]
fn toggling_wrap_recomputes_heights_and_holds_the_cursor() {
    let mut app = app_with(LONG_DIFF);
    app.view = View::Unified;
    app.rebuild_file_spans();
    app.recompute_file_span();
    render(&mut app, 40, 6);
    let before = app.selected;
    app.toggle_wrap();
    render(&mut app, 40, 6);
    // Heights now reflect wrapping, and the cursor stayed put and in view.
    assert!(!app.geom.heights.is_empty());
    assert_eq!(app.selected, before);
    let (s, _) = app.file_range();
    assert!(app.scroll >= s);
}
