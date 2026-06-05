pub mod app;
pub mod highlight;
pub mod highlight_cache;
pub mod render_rows;
pub mod sidebar;
pub mod theme;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use std::io::stderr;
use std::sync::Once;

use crate::comments::model::CommentStore;
use crate::diff::model::Changeset;

/// When stdin isn't a TTY (e.g. `git diff | hew`), point fd 0 at a real
/// terminal so crossterm's raw-mode and event reader have something to read
/// keys/mouse from. No-op when stdin is already a terminal.
///
/// We deliberately dup the terminal that's *already inherited* on stdout or
/// stderr rather than `open("/dev/tty")`. On macOS `/dev/tty` is the
/// controlling-terminal cloning device, and registering that descriptor with
/// kqueue (which mio/crossterm do) fails with `EINVAL` — surfacing as
/// "Failed to initialize input reader". The inherited pty slave on fd 1/2
/// registers cleanly. We only fall back to `/dev/tty` when neither stdout nor
/// stderr is a terminal.
fn reattach_stdin_to_tty() -> Result<()> {
    use std::os::fd::IntoRawFd;

    // SAFETY: isatty only inspects the given fd.
    let is_tty = |fd: i32| unsafe { libc::isatty(fd) } == 1;
    if is_tty(libc::STDIN_FILENO) {
        return Ok(());
    }

    // SAFETY: dup2 closes the old fd 0 and aliases it onto `src`; both are
    // valid open descriptors. The aliased terminal stays open via fd 1/2.
    if is_tty(libc::STDOUT_FILENO) || is_tty(libc::STDERR_FILENO) {
        let src = if is_tty(libc::STDOUT_FILENO) {
            libc::STDOUT_FILENO
        } else {
            libc::STDERR_FILENO
        };
        if unsafe { libc::dup2(src, libc::STDIN_FILENO) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("redirecting stdin to the inherited terminal");
        }
        return Ok(());
    }

    // No inherited terminal to borrow; last resort is the controlling tty.
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("opening /dev/tty to read interactive input")?;
    let fd = tty.into_raw_fd();
    // SAFETY: `fd` is a valid descriptor we own; close it after aliasing 0.
    let rc = unsafe { libc::dup2(fd, libc::STDIN_FILENO) };
    let dup_err = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if rc < 0 {
        return Err(dup_err).context("redirecting stdin to /dev/tty");
    }
    Ok(())
}

/// Best-effort restore of the inherited terminal: leave raw mode, exit the
/// alternate screen, and stop mouse capture. Safe to call more than once and
/// from a panic hook, so every exit path (normal, error, panic) lands the user
/// back on a clean prompt. Errors are ignored — we're tearing down regardless.
fn restore_terminal() {
    // Pop the keyboard-enhancement flags first (best-effort; terminals that
    // never received a push simply ignore the pop), then tear down the rest.
    let _ = execute!(stderr(), PopKeyboardEnhancementFlags);
    let _ = disable_raw_mode();
    let _ = execute!(stderr(), LeaveAlternateScreen, DisableMouseCapture);
}

/// Install (once) a panic hook that restores the terminal *before* the default
/// hook prints the panic message. Without this, a panic inside the TUI prints
/// its backtrace into the alternate screen (then loses it on teardown) and
/// leaves the user's terminal in raw mode.
fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            original(info);
        }));
    });
}

/// Set up the terminal, run the app, and restore the terminal afterwards.
pub fn run(changeset: Changeset, comments: CommentStore) -> Result<CommentStore> {
    // An empty changeset has nothing to show.
    if changeset.is_empty() {
        // Diagnostic goes to stderr; stdout stays clean for the review JSON.
        eprintln!("hew: no changes to review");
        return Ok(comments);
    }

    // The patch usually arrives on stdin (`git diff | hew`), which leaves fd 0
    // wired to a pipe rather than a terminal. Reconnect it to the controlling
    // terminal so raw-mode and crossterm's input reader have a real TTY to read
    // key/mouse events from; otherwise the first poll fails with "Failed to
    // initialize input reader".
    reattach_stdin_to_tty()?;

    // Install the panic hook *before* we enter raw mode / the alternate screen
    // below, so a panic at any point after that is guaranteed to restore the
    // terminal first. The hook covers panics; the explicit `restore_terminal()`
    // below covers the normal and error paths (note we do *not* use `?` on
    // teardown — a restore error must not skip the rest of the teardown or
    // swallow the app's own result).
    install_panic_hook();

    // Render to stderr, not stdout: stdout is reserved for the review JSON we
    // action log on exit, so `git diff | hew > actions.json` writes the result
    // to the file while the TUI still draws on the inherited terminal (fzf-style).
    enable_raw_mode()?;
    let mut out = stderr();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    // Enable the keyboard-enhancement protocol so the composer can distinguish
    // Shift+Enter (submit) from a bare Enter (newline) on terminals that
    // support it (kitty/ghostty/wezterm/foot/…). We push it *unconditionally to
    // stderr* — where the TUI renders — rather than gating on
    // `supports_keyboard_enhancement()`, whose probe writes to stdout. stdout
    // is reserved for the action-log JSON (and is often redirected to a file),
    // so the probe never reaches the terminal and would wrongly report "no
    // support". Terminals that don't implement the protocol simply ignore the
    // escape sequence, and Ctrl-S remains as a fallback. The matching pop
    // happens in `restore_terminal`.
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let mut app = app::App::with_comments(changeset, comments);
    let result = app.run(&mut terminal);

    restore_terminal();
    let _ = terminal.show_cursor();
    // Hand the final store back so the caller can diff it against the base.
    result.map(|()| app.into_comments())
}
