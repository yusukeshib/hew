use super::text::is_clean_ascii;
use super::*;
use crate::comments::model::{CommentStore, LineRange};
use crate::diff::parse::parse_report;
use crate::loader::{load_comments, load_patch};
use std::path::{Path, PathBuf};

// A two-line change: old line 2 (`b`) deleted, new line 2 (`B`) added.
const SIMPLE_DIFF: &str = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@ fn main
 a
-b
+B
 c
";

fn store_with(side: Side, line: u32) -> CommentStore {
    let mut store = CommentStore::default();
    store.add_thread(
        PathBuf::from("foo.txt"),
        side,
        LineRange {
            start: line,
            end: line,
        },
        Some("me".into()),
        "a comment".into(),
    );
    store
}

#[test]
fn split_comment_renders_under_anchored_side() {
    let cs = parse_report(SIMPLE_DIFF).0;

    // Old-side thread (anchored to the deleted line 2) is tagged Old.
    let old = store_with(Side::Old, 2);
    let rows = build_split_rows(&cs, &old, 80, None);
    assert!(
        rows.iter().any(|r| matches!(
            r.kind,
            SplitRowKind::Comment {
                side: Side::Old,
                ..
            }
        )),
        "old-side comment should be tagged Side::Old"
    );
    assert!(
        !rows.iter().any(|r| matches!(
            r.kind,
            SplitRowKind::Comment {
                side: Side::New,
                ..
            }
        )),
        "old-side comment must not be tagged Side::New"
    );

    // New-side thread (anchored to the added line 2) is tagged New.
    let new = store_with(Side::New, 2);
    let rows = build_split_rows(&cs, &new, 80, None);
    assert!(
        rows.iter().any(|r| matches!(
            r.kind,
            SplitRowKind::Comment {
                side: Side::New,
                ..
            }
        )),
        "new-side comment should be tagged Side::New"
    );
}

#[test]
fn thread_anchored_outside_any_hunk_is_still_emitted() {
    // A comment on a line the diff never shows (new-side line 99, far past
    // the only hunk) used to be counted by the sidebar yet never rendered.
    // It must still appear, appended after the file's hunks, in both views.
    let cs = parse_report(SIMPLE_DIFF).0;
    let orphan = store_with(Side::New, 99);

    let unified = build_rows(&cs, &orphan, 80, None);
    assert!(
        unified
            .iter()
            .any(|r| matches!(r.kind, RowKind::Comment(_))),
        "an out-of-hunk thread must still render in the unified view"
    );

    let split = build_split_rows(&cs, &orphan, 80, None);
    assert!(
        split
            .iter()
            .any(|r| matches!(r.kind, SplitRowKind::Comment { .. })),
        "an out-of-hunk thread must still render in the split view"
    );
}

#[test]
fn in_hunk_thread_is_not_double_emitted_by_orphan_pass() {
    // Dedup: a thread shown inline must not be re-emitted as an orphan.
    let cs = parse_report(SIMPLE_DIFF).0;
    let inhunk = store_with(Side::New, 2); // anchored to the added line 2
    let rows = build_rows(&cs, &inhunk, 80, None);
    let box_tops = rows
        .iter()
        .filter(|r| matches!(&r.kind, RowKind::Comment(cl) if matches!(cl.kind, CommentKind::Top)))
        .count();
    assert_eq!(box_tops, 1, "thread emitted exactly once");
}

#[test]
fn expands_tabs_and_strips_controls() {
    assert_eq!(sanitize_line("\tx"), "    x");
    assert_eq!(sanitize_line("a\tb"), "a   b"); // tab to next 4-col stop
    assert_eq!(sanitize_line("end\r"), "end");
    assert_eq!(sanitize_line("a\u{0}b"), "ab");
    // ANSI CSI color sequence is removed, payload kept.
    assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m"), "red");
}

#[test]
fn sanitize_fast_path_matches_clean_ascii_verbatim() {
    // The printable-ASCII fast path must return the input untouched (it's
    // the common case and skips the char-by-char loop entirely).
    for s in [
        "let x = 1;",
        "  indented",
        "a + b - c",
        "~tilde~ {braces}",
        "",
    ] {
        assert!(is_clean_ascii(s), "{s:?} should be detected as clean ASCII");
        assert_eq!(sanitize_line(s), s);
    }
    // Boundary bytes that must NOT take the fast path: DEL (0x7f), a control
    // char (< 0x20), and any non-ASCII (>= 0x80).
    assert!(!is_clean_ascii("a\u{7f}b"));
    assert!(!is_clean_ascii("a\tb"));
    assert!(!is_clean_ascii("\u{1b}[0m"));
    assert!(!is_clean_ascii("caf\u{e9}"));
}

#[test]
fn sanitize_into_prefix_does_not_shift_tab_stops() {
    // `build_rows` prepends a diff sign then sanitizes into the same buffer;
    // tab stops must be measured from the code text (col 0), so the result
    // matches sign + independently-sanitized text. The space prefix here is
    // a 1-col sign, but the tab still expands to the 4-col stop of the code.
    let mut buf = String::from(" ");
    sanitize_into(&mut buf, "a\tb");
    assert_eq!(buf, format!(" {}", sanitize_line("a\tb")));
    assert_eq!(buf, " a   b");
}

#[test]
fn display_width_counts_wide_glyphs() {
    assert_eq!(str_width("abc"), 3);
    assert_eq!(str_width("日本語"), 6); // 3 wide CJK glyphs = 6 cells
    assert_eq!(str_width("a日b"), 4);
}

#[test]
fn take_width_never_overflows_on_wide_glyphs() {
    // Budget 3 over "a日本": fits 'a'(1)+'日'(2)=3; '本' would overflow.
    let (s, w) = take_width("a日本", 3);
    assert_eq!(s, "a日");
    assert_eq!(w, 3);
    // Odd budget straddling a wide glyph drops it (no half-cell).
    let (s, w) = take_width("日本", 1);
    assert_eq!(s, "");
    assert_eq!(w, 0);
}

#[test]
fn wrap_text_wraps_on_display_width() {
    // Three wide glyphs (6 cells) wrap at width 4 (2 glyphs per line).
    let lines = wrap_text("日本語", 4);
    assert!(lines.iter().all(|l| str_width(l) <= 4));
    assert_eq!(lines.concat(), "日本語");
}

#[test]
fn wrap_text_no_empty_lines_for_unsplittable_glyphs() {
    // A glyph wider than the width can't be split; it must land on its own
    // line with no spurious empty line ahead of it.
    let lines = wrap_text("日本", 1);
    assert_eq!(lines, vec!["日".to_string(), "本".to_string()]);
    assert!(lines.iter().all(|l| !l.is_empty()));
}

#[test]
fn wrap_preserve_keeps_runs_of_spaces() {
    // Regression: the composer is a live buffer, so consecutive spaces must
    // survive verbatim (wrap_text collapses them via split_whitespace).
    let lines = wrap_preserve("a    b", 80);
    assert_eq!(lines, vec!["a    b".to_string()]);
}

#[test]
fn wrap_preserve_breaks_at_word_boundary() {
    // Greedy break prefers the last space so a word isn't split mid-token.
    // The break space stays on the first line so the buffer is preserved
    // verbatim (concatenating the visual lines reproduces the input).
    let lines = wrap_preserve("hello world", 7);
    assert_eq!(lines, vec!["hello ".to_string(), "world".to_string()]);
    assert_eq!(lines.concat(), "hello world");
}

#[test]
fn wrap_preserve_hard_splits_an_overlong_token() {
    // A token longer than the width has no space to break on, so it is split.
    let lines = wrap_preserve("abcdef", 3);
    assert_eq!(lines, vec!["abc".to_string(), "def".to_string()]);
}

#[test]
fn injects_inline_thread_rows() {
    let cs = load_patch(Some(Path::new("examples/rust-long-en.patch"))).unwrap();
    let comments = load_comments(Path::new("examples/rust-long-en.comments.json")).unwrap();
    let base = build_rows(&cs, &CommentStore::default(), 80, None);
    let rows = build_rows(&cs, &comments, 80, None);
    // Threads inject extra (comment) rows over a no-comment baseline.
    assert!(rows.len() > base.len());
    // Those rows are comment rows: non-selectable and anchorless.
    let comment_rows = rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Comment(_)))
        .count();
    assert!(comment_rows > 0);
    // Each thread renders exactly one header, regardless of multi-line
    // anchor ranges (no per-line duplication).
    let heads = rows
        .iter()
        .filter(|r| {
            matches!(
                &r.kind,
                RowKind::Comment(CommentLine {
                    kind: CommentKind::Head { .. },
                    ..
                })
            )
        })
        .count();
    assert_eq!(heads, comments.threads.len());
    assert!(rows
        .iter()
        .all(|r| !matches!(r.kind, RowKind::Comment(_))
            || (!r.is_selectable() && r.anchor().is_none())));
}

#[test]
fn new_thread_composer_injects_inline_box() {
    let cs = parse_report(SIMPLE_DIFF).0;
    let store = CommentStore::default();
    let spec = ComposerSpec {
        anchor: ComposerAnchor::NewThread {
            file_idx: 0,
            side: Side::New,
            line: 2,
        },
        title: " new comment ".into(),
        body: "hi".into(),
    };
    // Without a composer, no composer rows; with one, a box appears.
    assert!(!build_rows(&cs, &store, 80, None)
        .iter()
        .any(|r| matches!(r.kind, RowKind::Composer(_))));
    let rows = build_rows(&cs, &store, 80, Some(&spec));
    // Exactly one top + one bottom border (a single box), and it carries
    // the live body text. Composer rows are never selectable.
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(
                r.kind,
                RowKind::Composer(ComposerLine {
                    kind: ComposerKind::Top { .. }
                })
            ))
            .count(),
        1
    );
    assert!(rows
            .iter()
            .any(|r| matches!(&r.kind, RowKind::Composer(ComposerLine { kind: ComposerKind::Body(b) }) if b.contains("hi"))));
    assert!(rows
        .iter()
        .all(|r| !matches!(r.kind, RowKind::Composer(_)) || !r.is_selectable()));
}

#[test]
fn reply_composer_injects_under_its_thread() {
    let cs = parse_report(SIMPLE_DIFF).0;
    let store = store_with(Side::New, 2);
    let thread_id = store.threads[0].id.clone();
    let spec = ComposerSpec {
        anchor: ComposerAnchor::Reply { thread_id },
        title: " reply ".into(),
        body: "ok".into(),
    };
    let rows = build_rows(&cs, &store, 80, Some(&spec));
    // The reply box renders after the thread's bottom border.
    let bottom = rows.iter().position(|r| {
        matches!(
            &r.kind,
            RowKind::Comment(CommentLine {
                kind: CommentKind::Bottom,
                ..
            })
        )
    });
    let comp_top = rows.iter().position(|r| {
        matches!(
            r.kind,
            RowKind::Composer(ComposerLine {
                kind: ComposerKind::Top { .. }
            })
        )
    });
    assert!(bottom.is_some() && comp_top.is_some());
    assert!(
        comp_top > bottom,
        "reply composer must sit below its thread"
    );
}
