//! Centralized UI color palette.
//!
//! Every non-syntax color the TUI draws lives here so the look can be tuned in
//! one place. (Code-token colors come separately from the syntect theme in
//! [`crate::ui::highlight`].) Access the active palette through [`THEME`].

use ratatui::style::Color;

/// Semantic colors for the diff viewer's chrome and text.
pub struct Theme {
    // Diff line backgrounds.
    /// Added line tint.
    pub add_bg: Color,
    /// Removed line tint.
    pub del_bg: Color,
    /// Focused selection background (sidebar row / generic selection).
    pub sel_bg: Color,
    /// Current (cursor) line when the diff pane is focused.
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

/// The active palette: a dark theme tuned for low-contrast chrome with a
/// vivid focused cursor line.
pub const THEME: Theme = Theme {
    add_bg: Color::Rgb(20, 42, 24),
    del_bg: Color::Rgb(48, 24, 26),
    sel_bg: Color::Rgb(96, 104, 128),
    cursor_bg: Color::Rgb(38, 116, 180),
    unfocus_bg: Color::Rgb(40, 42, 48),
    file_header_bg: Color::Rgb(40, 44, 52),
    comment_bg: Color::Rgb(28, 30, 34),

    subtle: Color::Rgb(38, 40, 46),
    scrollbar_thumb: Color::Rgb(58, 62, 70),
    border_focus: Color::White,
    border_unfocus: Color::Rgb(78, 84, 96),

    text: Color::Gray,
    text_strong: Color::White,
    muted: Color::DarkGray,
    faint: Color::Rgb(106, 115, 130),
    accent: Color::Cyan,
    warn: Color::Yellow,
    added: Color::Green,
    removed: Color::Red,
    none: Color::Reset,
};
