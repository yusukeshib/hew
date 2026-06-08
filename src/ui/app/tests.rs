use super::*;
use crate::diff::parse::parse_report;

// Four additions framed by two context lines, so the new side carries
// lines 1..=6 and there are six selectable rows in a known order.

mod comments;
mod composer;
mod mouse;
mod nav;
mod wrap;

const DIFF: &str = "\
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,6 @@
 a
+b
+c
+d
+e
 f
";

// Two files, to exercise per-file navigation.
const TWO_FILES: &str = "\
--- a/one.rs
+++ b/one.rs
@@ -1 +1,2 @@
 x
+y
--- a/two.rs
+++ b/two.rs
@@ -1 +1,2 @@
 p
+q
";

fn app_with(diff: &str) -> App {
    let cs = parse_report(diff).0;
    let mut app = App::with_comments(cs, CommentStore::default());
    app.height = 4; // deterministic viewport for scroll math
                    // Pin the legacy 1:1 geometry for tests that assert exact row/line
                    // positions; the wrap-specific tests opt back in explicitly.
    app.wrap = false;
    app
}

/// Move the cursor onto the row anchored at `(current_file, side, line)`.
fn goto(app: &mut App, side: Side, line: u32) {
    let (s, e) = app.file_range();
    for i in s..e {
        if app.anchor_at(i) == Some((app.current_file, side, line)) {
            app.selected = i;
            return;
        }
    }
    panic!("no selectable row for {side:?} line {line}");
}

/// Build an app with one new-side thread (two messages) anchored to `line`,
/// rendered inline.
fn app_with_thread(line: u32) -> (App, String, String) {
    let cs = parse_report(DIFF).0;
    let mut store = CommentStore::default();
    let tid = store.add_thread(
        "f.rs".into(),
        Side::New,
        LineRange {
            start: line,
            end: line,
        },
        Some("a".into()),
        "root message".into(),
    );
    store.reply(&tid, Some("b".into()), "a reply".into());
    let reply_id = store.threads[0].comments[1].id.clone();
    let mut app = App::with_comments(cs, store);
    app.height = 40; // tall enough to hold the whole thread
    app.wrap = false;
    (app, tid, reply_id)
}

/// First active row that is a content line of comment `comment_id`.
fn comment_head(app: &App, comment_id: &str) -> usize {
    (0..app.active_len())
        .find(|&i| {
            app.is_stop_at(i)
                && app.comment_unit_at(i).map(|(_, c)| c).as_deref() == Some(comment_id)
        })
        .expect("comment head row")
}

/// Open a new-thread composer anchored on a known diff line.
fn open_composer(app: &mut App) {
    goto(app, Side::New, 3);
    app.open_new_thread();
    assert!(app.composer.is_some(), "composer should be open");
}

/// The current composer text (no caret), for assertions.
fn composer_text(app: &App) -> String {
    app.composer.as_ref().unwrap().textarea.lines().join("\n")
}

// Render into a TestBackend and return the app (button hit rects are
// recorded during the draw).
fn render(app: &mut App, w: u16, h: u16) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
}

fn click(app: &mut App, r: Rect) {
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
}

// ---- soft-wrap ----

// A single addition with a long code body, so wrapping it produces several
// display lines while the row count stays tiny.
const LONG_DIFF: &str = "\
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,3 @@
 a
+let total = alpha + beta + gamma + delta + epsilon + zeta + eta + theta + iota;
 f
";
