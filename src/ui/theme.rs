//! Centralized UI color palette.
//!
//! The whole UI palette is *derived from the active syntect theme* (see
//! [`Theme::from_syntect`]): the chrome (sidebar, borders, headers, cursor
//! line, status) and background are computed from the theme's background,
//! foreground, selection, and a few scope colors. So switching the default
//! theme is a one-line change in [`crate::ui::highlight`] — the chrome follows
//! automatically, no hand-authored parallel palette to keep in sync.
//!
//! At startup [`init_theme`] derives the palette and resolves it against the
//! detected terminal: on truecolor the RGB values pass through; otherwise each
//! `Rgb` is downsampled to the nearest xterm-256 index ([`adapt_color`]) so the
//! look degrades gracefully instead of being mangled by tmux's own conversion.
//! Access the active palette through [`theme()`].

use ratatui::style::Color;
use std::sync::OnceLock;
use syntect::highlighting::{Color as SynColor, Highlighter as SynHighlighter, Theme as SynTheme};
use syntect::parsing::Scope;

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

/// Fallback palette returned by [`theme`] before [`init_theme`] installs the
/// derived one (e.g. any code path — including tests — that reads the palette
/// pre-init). A neutral dark theme; the real palette is derived from the active
/// syntect theme at startup.
static FALLBACK: Theme = Theme {
    bg: Color::Rgb(26, 27, 38),
    add_bg: Color::Rgb(32, 44, 38),
    del_bg: Color::Rgb(55, 32, 42),
    cursor_bg: Color::Rgb(54, 74, 124),
    unfocus_bg: Color::Rgb(41, 46, 66),
    file_header_bg: Color::Rgb(41, 46, 66),
    comment_bg: Color::Rgb(31, 35, 53),
    subtle: Color::Rgb(41, 46, 66),
    scrollbar_thumb: Color::Rgb(86, 95, 137),
    border_focus: Color::Rgb(122, 162, 247),
    border_unfocus: Color::Rgb(86, 95, 137),
    text: Color::Rgb(169, 177, 214),
    text_strong: Color::Rgb(192, 202, 245),
    muted: Color::Rgb(86, 95, 137),
    faint: Color::Rgb(115, 122, 162),
    accent: Color::Rgb(125, 207, 255),
    warn: Color::Rgb(224, 175, 104),
    added: Color::Rgb(158, 206, 106),
    removed: Color::Rgb(247, 118, 142),
    none: Color::Reset,
};

/// Whether the active terminal was detected as truecolor-capable. Drives the
/// dynamic downsampling of syntect token colors in [`adapt_color`]. Defaults to
/// `true` (no downsampling) until [`init_theme`] runs — so unit tests and any
/// pre-init access keep authored colors.
static TRUECOLOR: OnceLock<bool> = OnceLock::new();
/// The palette resolved for the active terminal (see [`init_theme`]).
static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Derive the chrome palette from the active `syntect` theme, then resolve it
/// for the detected terminal and cache it. Call once at startup, before the
/// first render. Idempotent: later calls are ignored.
pub fn init_theme(syntect_theme: &SynTheme, truecolor: bool) {
    let derived = Theme::from_syntect(syntect_theme);
    let _ = TRUECOLOR.set(truecolor);
    let _ = ACTIVE.set(derived.adapt(truecolor));
}

/// The active palette. Falls back to [`FALLBACK`] if [`init_theme`] hasn't run
/// (e.g. in tests). Crucially this *reads* `ACTIVE` without initializing it — a
/// pre-init call must not lock in `FALLBACK` and prevent a later [`init_theme`]
/// from installing the derived palette.
pub fn theme() -> &'static Theme {
    ACTIVE.get().unwrap_or(&FALLBACK)
}

/// Adapt a *dynamically produced* color (e.g. a syntect token's RGB) to the
/// active terminal: pass truecolor through, otherwise downsample `Rgb` to the
/// nearest xterm-256 index. Named/indexed colors are returned unchanged.
pub fn adapt_color(c: Color) -> Color {
    down(c, *TRUECOLOR.get().unwrap_or(&true))
}

/// Linearly blend two RGB triples (`t` in 0..=1: 0 = `a`, 1 = `b`).
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// Composite `c` (with its alpha) over an opaque `bg`. Theme `selection` /
/// `line_highlight` colors are often semi-transparent tints meant to sit over
/// the background.
fn over(bg: (u8, u8, u8), c: SynColor) -> (u8, u8, u8) {
    mix(bg, (c.r, c.g, c.b), c.a as f32 / 255.0)
}

/// Squared distance between two RGB triples (perceptual-ish, unweighted).
fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

impl Theme {
    /// Derive the full chrome palette from a `syntect` theme: background and
    /// foreground come from the theme settings; selection/line-highlight drive
    /// the cursor line; a handful of scope colors (comment, keyword, string,
    /// markup.inserted/deleted) drive the accents and diff tints. Everything
    /// else is a tint of bg toward fg, so any theme yields a coherent UI.
    pub fn from_syntect(syn: &SynTheme) -> Theme {
        let s = &syn.settings;
        let rgb = |c: SynColor| (c.r, c.g, c.b);
        let bg = s.background.map(rgb).unwrap_or((26, 27, 38));
        let fg = s.foreground.map(rgb).unwrap_or((192, 202, 245));

        // Resolve a scope's foreground; `None` when the theme doesn't give it a
        // color distinct from the default foreground (so we can fall back).
        let hl = SynHighlighter::new(syn);
        let scope = |name: &str| -> Option<(u8, u8, u8)> {
            let sc = Scope::new(name).ok()?;
            let c = hl.style_for_stack(&[sc]).foreground;
            let t = (c.r, c.g, c.b);
            (t != fg).then_some(t)
        };

        let comment = scope("comment").unwrap_or_else(|| mix(bg, fg, 0.45));
        let accent = s.accent.map(rgb).or_else(|| scope("keyword")).unwrap_or(fg);
        let warn = scope("string")
            .or_else(|| scope("constant.numeric"))
            .unwrap_or((224, 175, 104));
        let added = scope("markup.inserted")
            .or_else(|| scope("diff.inserted"))
            .unwrap_or((158, 206, 106));
        let removed = scope("markup.deleted")
            .or_else(|| scope("diff.deleted"))
            .unwrap_or((247, 118, 142));

        // Focused current line: the theme's line-highlight/selection over bg.
        // If that's too close to bg to notice, fall back to a clear bg->fg mix.
        let sel = s
            .line_highlight
            .or(s.selection)
            .map(|c| over(bg, c))
            .unwrap_or_else(|| mix(bg, fg, 0.22));
        // Min squared per-channel distance for the focused line to read as
        // distinct from bg; below it we fall back to a clear bg->fg mix.
        const CURSOR_BG_MIN_DIST2: i32 = 30 * 30;
        let cursor_bg = if dist2(sel, bg) < CURSOR_BG_MIN_DIST2 {
            mix(bg, fg, 0.22)
        } else {
            sel
        };

        let t = |x: (u8, u8, u8)| Color::Rgb(x.0, x.1, x.2);
        Theme {
            bg: t(bg),
            add_bg: t(mix(bg, added, 0.22)),
            del_bg: t(mix(bg, removed, 0.22)),
            cursor_bg: t(cursor_bg),
            unfocus_bg: t(mix(bg, fg, 0.10)),
            file_header_bg: t(mix(bg, fg, 0.12)),
            comment_bg: t(mix(bg, fg, 0.05)),
            subtle: t(mix(bg, fg, 0.12)),
            scrollbar_thumb: t(comment),
            border_focus: t(accent),
            // Unfocused borders sit well below the bright accent focus border so
            // a selected comment box (or the focused diff panel) reads clearly
            // against unselected ones: a dim neutral grey, not the lighter
            // comment hue (which was too close to the accent at a glance).
            border_unfocus: t(mix(bg, fg, 0.28)),
            text: t(mix(fg, bg, 0.12)),
            text_strong: t(fg),
            // Secondary / "disabled" UI text (line numbers, status, dates) is a
            // neutral bg→fg grey — deliberately decoupled from the (often
            // colored) comment scope so disabled text never inherits a hue.
            muted: t(mix(bg, fg, 0.45)),
            faint: t(mix(bg, fg, 0.32)),
            accent: t(accent),
            warn: t(warn),
            added: t(added),
            removed: t(removed),
            none: Color::Reset,
        }
    }

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
    fn theme_falls_back_without_locking() {
        // No test calls `init_theme`, so `ACTIVE` stays empty and `theme()`
        // must hand back the `FALLBACK` reference itself (not a cached copy) —
        // proving a pre-init read can't lock in a palette and block a later
        // `init_theme` from installing the derived one.
        assert!(std::ptr::eq(theme(), &FALLBACK));
        assert_eq!(theme().added, FALLBACK.added);
    }

    #[test]
    fn adapt_downsamples_rgb_fields_but_keeps_non_rgb() {
        let dark = FALLBACK.adapt(false);
        // Rgb fields become Indexed...
        assert!(matches!(dark.cursor_bg, Color::Indexed(_)));
        assert!(matches!(dark.removed, Color::Indexed(_)));
        // ...while a non-Rgb color (Reset) is passed through unchanged.
        assert_eq!(dark.none, Color::Reset);
        // Truecolor adapt is a faithful pass-through.
        let bright = FALLBACK.adapt(true);
        assert_eq!(bright.cursor_bg, FALLBACK.cursor_bg);
    }

    #[test]
    fn derives_chrome_from_syntect_theme() {
        // The whole chrome palette is derived from the active syntect theme:
        // bg mirrors the theme background, and diff/accent colors are real
        // (distinct from bg).
        let syn = crate::ui::highlight::default_theme();
        let t = Theme::from_syntect(&syn);
        if let Some(bg) = syn.settings.background {
            assert_eq!(
                t.bg,
                Color::Rgb(bg.r, bg.g, bg.b),
                "bg must mirror the theme"
            );
        }
        assert_ne!(t.added, t.bg, "added must be a visible color");
        assert_ne!(t.removed, t.bg, "removed must be a visible color");
        assert_ne!(t.cursor_bg, t.bg, "focused current line must stand out");
    }
}
