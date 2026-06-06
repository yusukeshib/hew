//! Centralized UI color palette.
//!
//! Every non-syntax color the TUI draws lives here so the look can be tuned in
//! one place. (Code-token colors come separately from the syntect theme in
//! [`crate::ui::highlight`].) Access the active palette through [`theme()`].
//!
//! The master palette ([`MASTER`]) is authored in 24-bit truecolor. At startup
//! [`init_theme`] resolves it against the detected terminal: on a truecolor
//! terminal the RGB values pass through unchanged; otherwise each `Rgb` is
//! downsampled to the nearest xterm-256 index ([`adapt_color`]) so the look
//! degrades gracefully instead of being mangled by tmux's own conversion.
//! Named ANSI colors (e.g. `Red`, `Cyan`) are left alone so they keep tracking
//! the user's terminal/tmux palette.

use ratatui::style::Color;
use std::sync::OnceLock;

/// Semantic colors for the diff viewer's chrome and text.
#[derive(Clone, Copy)]
pub struct Theme {
    /// The global background painted behind every pane.
    pub bg: Color,

    // Diff line backgrounds.
    /// Added line tint.
    pub add_bg: Color,
    /// Removed line tint.
    pub del_bg: Color,
    /// Current (cursor) line when its pane is focused (diff or sidebar).
    pub cursor_bg: Color,
    /// Current/selected line when its pane is *not* focused.
    pub unfocus_bg: Color,
    /// File-header row background.
    pub file_header_bg: Color,
    /// Inline comment box background.
    pub comment_bg: Color,

    // Borders, dividers, scrollbars.
    /// Very dark chrome (split column divider).
    pub subtle: Color,
    /// Scrollbar thumb.
    pub scrollbar_thumb: Color,
    /// Diff panel border while focused.
    pub border_focus: Color,
    /// Diff panel border while unfocused (dim but visible).
    pub border_unfocus: Color,

    // Text / foreground.
    /// Default body text.
    pub text: Color,
    /// Emphasized text (current file, headers, comment-box border).
    pub text_strong: Color,
    /// Muted text (gutter numbers, status line, context sign, resolved dot).
    pub muted: Color,
    /// Faint structural text (directory / hunk headers, sidebar labels).
    pub faint: Color,
    /// Accent (rename status, comment author).
    pub accent: Color,
    /// Attention (modified status, open-comment dot).
    pub warn: Color,
    /// Additions (status letter, `+` sign, counts).
    pub added: Color,
    /// Deletions (status letter, `-` sign, counts).
    pub removed: Color,
    /// Placeholder where no marker is drawn.
    pub none: Color,
}

/// The master palette: a dark theme tuned for low-contrast chrome with a vivid
/// focused cursor line, authored in truecolor. Resolved per-terminal by
/// [`init_theme`]; read the resolved palette via [`theme()`].
static MASTER: Theme = Theme {
    // Dracula palette (https://draculatheme.com): bg #282a36, current-line
    // #44475a, fg #f8f8f2, comment #6272a4, cyan #8be9fd, green #50fa7b, orange
    // #ffb86c, purple #bd93f9, red #ff5555.
    bg: Color::Rgb(40, 42, 54),

    add_bg: Color::Rgb(34, 52, 43),
    del_bg: Color::Rgb(63, 42, 49),
    cursor_bg: Color::Rgb(68, 71, 90),
    unfocus_bg: Color::Rgb(52, 54, 68),
    file_header_bg: Color::Rgb(54, 57, 73),
    comment_bg: Color::Rgb(45, 47, 61),

    subtle: Color::Rgb(54, 57, 73),
    scrollbar_thumb: Color::Rgb(98, 114, 164),
    border_focus: Color::Rgb(189, 147, 249),
    border_unfocus: Color::Rgb(98, 114, 164),

    text: Color::Rgb(248, 248, 242),
    text_strong: Color::Rgb(255, 255, 255),
    muted: Color::Rgb(98, 114, 164),
    faint: Color::Rgb(130, 142, 184),
    accent: Color::Rgb(139, 233, 253),
    warn: Color::Rgb(255, 184, 108),
    added: Color::Rgb(80, 250, 123),
    removed: Color::Rgb(255, 85, 85),
    none: Color::Reset,
};

/// Whether the active terminal was detected as truecolor-capable. Drives the
/// dynamic downsampling of syntect token colors in [`adapt_color`]. Defaults to
/// `true` (no downsampling) until [`init_theme`] runs — so unit tests and any
/// pre-init access keep the authored palette.
static TRUECOLOR: OnceLock<bool> = OnceLock::new();
/// The palette resolved for the active terminal (see [`init_theme`]).
static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Resolve and cache the active palette for the detected terminal. Call once at
/// startup, before the first render. Idempotent: later calls are ignored.
pub fn init_theme(truecolor: bool) {
    let _ = TRUECOLOR.set(truecolor);
    let _ = ACTIVE.set(MASTER.adapt(truecolor));
}

/// The active palette. Falls back to the authored truecolor master if
/// [`init_theme`] hasn't run (e.g. in tests). Crucially this *reads* `ACTIVE`
/// without initializing it — a pre-init call must not lock in `MASTER` and
/// prevent a later [`init_theme(false)`] from installing the 256-color palette.
pub fn theme() -> &'static Theme {
    ACTIVE.get().unwrap_or(&MASTER)
}

/// Adapt a *dynamically produced* color (e.g. a syntect token's RGB) to the
/// active terminal: pass truecolor through, otherwise downsample `Rgb` to the
/// nearest xterm-256 index. Named/indexed colors are returned unchanged.
pub fn adapt_color(c: Color) -> Color {
    down(c, *TRUECOLOR.get().unwrap_or(&true))
}

impl Theme {
    /// Build a copy with every `Rgb` field downsampled for a non-truecolor
    /// terminal (a no-op when `truecolor`). Named colors are preserved.
    fn adapt(&self, truecolor: bool) -> Theme {
        let d = |c: Color| down(c, truecolor);
        Theme {
            bg: d(self.bg),
            add_bg: d(self.add_bg),
            del_bg: d(self.del_bg),
            cursor_bg: d(self.cursor_bg),
            unfocus_bg: d(self.unfocus_bg),
            file_header_bg: d(self.file_header_bg),
            comment_bg: d(self.comment_bg),
            subtle: d(self.subtle),
            scrollbar_thumb: d(self.scrollbar_thumb),
            border_focus: d(self.border_focus),
            border_unfocus: d(self.border_unfocus),
            text: d(self.text),
            text_strong: d(self.text_strong),
            muted: d(self.muted),
            faint: d(self.faint),
            accent: d(self.accent),
            warn: d(self.warn),
            added: d(self.added),
            removed: d(self.removed),
            none: d(self.none),
        }
    }
}

/// Downsample a single `Rgb` color to the nearest xterm-256 index when the
/// terminal isn't truecolor; pass everything else through untouched.
fn down(c: Color, truecolor: bool) -> Color {
    match c {
        Color::Rgb(r, g, b) if !truecolor => Color::Indexed(rgb_to_ansi256(r, g, b)),
        other => other,
    }
}

/// Nearest xterm-256 palette index for a 24-bit color. Considers both the
/// 6×6×6 color cube (indices 16..=231) and the 24-step grayscale ramp
/// (232..=255), returning whichever is closest by squared Euclidean distance.
/// The first 16 slots are skipped: they're terminal-defined, so their RGB is
/// unknown and can't be matched reliably.
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // Sample levels for each cube axis (xterm's standard 6-step ramp).
    const CUBE: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_cube = |v: i32| -> usize {
        CUBE.iter()
            .enumerate()
            .min_by_key(|(_, &lvl)| (v - lvl).abs())
            .map(|(i, _)| i)
            .unwrap()
    };
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let dist = |a: (i32, i32, i32), x: (i32, i32, i32)| {
        let (dr, dg, db) = (a.0 - x.0, a.1 - x.1, a.2 - x.2);
        dr * dr + dg * dg + db * db
    };

    // Best match within the color cube.
    let (ci, cj, ck) = (nearest_cube(r), nearest_cube(g), nearest_cube(b));
    let cube_rgb = (CUBE[ci], CUBE[cj], CUBE[ck]);
    let cube_idx = 16 + 36 * ci + 6 * cj + ck;

    // Best match within the grayscale ramp: gray N (0..=23) is 8 + 10*N. Round
    // to the *nearest* step (+5 before the floor-divide) rather than truncating,
    // so e.g. avg 17 picks N=1 (18) not N=0 (8).
    let avg = (r + g + b) / 3;
    let gray_n = (((avg - 8).max(0) + 5) / 10).clamp(0, 23);
    let gray_v = 8 + 10 * gray_n;
    let gray_idx = 232 + gray_n as usize;

    if dist((r, g, b), cube_rgb) <= dist((r, g, b), (gray_v, gray_v, gray_v)) {
        cube_idx as u8
    } else {
        gray_idx as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_passes_rgb_through_unchanged() {
        assert_eq!(down(Color::Rgb(20, 42, 24), true), Color::Rgb(20, 42, 24));
        // Named colors are never touched, in either mode.
        assert_eq!(down(Color::Red, false), Color::Red);
        assert_eq!(down(Color::Reset, false), Color::Reset);
    }

    #[test]
    fn non_truecolor_downsamples_rgb_to_indexed() {
        // Pure colors land on their cube corners.
        assert_eq!(down(Color::Rgb(0, 0, 0), false), Color::Indexed(16));
        assert_eq!(down(Color::Rgb(255, 255, 255), false), Color::Indexed(231));
        assert_eq!(down(Color::Rgb(255, 0, 0), false), Color::Indexed(196));
        assert_eq!(down(Color::Rgb(0, 255, 0), false), Color::Indexed(46));
        assert_eq!(down(Color::Rgb(0, 0, 255), false), Color::Indexed(21));
    }

    #[test]
    fn mid_gray_prefers_the_grayscale_ramp() {
        // A neutral gray is closer to the 24-step ramp than any cube corner.
        let idx = match down(Color::Rgb(128, 128, 128), false) {
            Color::Indexed(i) => i,
            other => panic!("expected indexed, got {other:?}"),
        };
        assert!(
            (232..=255).contains(&idx),
            "expected grayscale ramp, got {idx}"
        );
    }

    #[test]
    fn grayscale_ramp_rounds_to_nearest_step() {
        // avg 17 is closer to gray step 1 (value 18 -> index 233) than step 0
        // (value 8 -> index 232); flooring would wrongly pick 232.
        assert_eq!(rgb_to_ansi256(17, 17, 17), 233);
        // avg 12 -> step 0 (8) is nearer than step 1 (18).
        assert_eq!(rgb_to_ansi256(12, 12, 12), 232);
    }

    #[test]
    fn theme_falls_back_to_master_without_locking() {
        // No test calls `init_theme`, so `ACTIVE` stays empty and `theme()`
        // must hand back the `MASTER` reference itself (not a cached copy) —
        // proving a pre-init read can't lock in a palette and block a later
        // `init_theme(false)` from installing the 256-color fallback.
        assert!(std::ptr::eq(theme(), &MASTER));
        assert_eq!(theme().added, MASTER.added);
    }

    #[test]
    fn adapt_downsamples_rgb_fields_but_keeps_named() {
        let dark = MASTER.adapt(false);
        // Rgb fields become Indexed...
        assert!(matches!(dark.cursor_bg, Color::Indexed(_)));
        assert!(matches!(dark.removed, Color::Indexed(_)));
        // ...while a non-Rgb color (Reset) is preserved.
        assert_eq!(dark.none, Color::Reset);
        // Truecolor adapt is a faithful pass-through.
        let bright = MASTER.adapt(true);
        assert_eq!(bright.cursor_bg, MASTER.cursor_bg);
    }
}
