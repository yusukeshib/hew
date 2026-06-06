//! Lazy, per-line syntax highlighting via syntect (pure-Rust fancy-regex).
//!
//! Highlighting is intentionally line-isolated: each diff line is highlighted on
//! its own (no cross-line state), which keeps it cheap and trivially cacheable
//! for viewport-only rendering. Multi-line constructs (block comments, multiline
//! strings) won't carry state across hunk gaps — an accepted trade-off for a
//! diff viewer that never sees the whole file.

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

pub struct Highlighter {
    ps: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        // two-face ships bat's extended syntax set (TS/TSX, TOML, Dockerfile, …)
        // and themes; `_no_newlines` matches our line-by-line highlighting. The
        // default theme is Dracula.
        let ps = two_face::syntax::extra_no_newlines();
        let theme = two_face::theme::extra()
            .get(EmbeddedThemeName::Dracula)
            .clone();
        Highlighter { ps, theme }
    }

    /// Resolve a syntax by file path (extension or filename), else plain text.
    pub fn syntax_for(&self, path: &str) -> &SyntaxReference {
        let p = std::path::Path::new(path);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        self.ps
            .find_syntax_by_extension(ext)
            .or_else(|| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                self.ps.find_syntax_by_token(name)
            })
            .unwrap_or_else(|| self.ps.find_syntax_plain_text())
    }

    /// Highlight one line into `(fg color, text)` runs.
    pub fn line(&self, syntax: &SyntaxReference, text: &str) -> Vec<(Color, String)> {
        let mut h = HighlightLines::new(syntax, &self.theme);
        match h.highlight_line(text, &self.ps) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(st, s)| {
                    let c = st.foreground;
                    // Downsample to xterm-256 on non-truecolor terminals so the
                    // syntax palette degrades gracefully (see `theme`).
                    let fg = crate::ui::theme::adapt_color(Color::Rgb(c.r, c.g, c.b));
                    (fg, s.to_string())
                })
                .collect(),
            Err(_) => vec![(Color::Gray, text.to_string())],
        }
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_loads_and_highlights() {
        // A theme that yields no colored runs would defeat syntax highlighting.
        let hl = Highlighter::new();
        let syntax = hl.syntax_for("main.rs");
        let runs = hl.line(syntax, "let x = 1;");
        assert!(!runs.is_empty(), "highlighting produced no runs");
        // Dracula is a truecolor theme; with truecolor active (the test default)
        // at least one run should carry a real RGB foreground.
        assert!(
            runs.iter().any(|(c, _)| matches!(c, Color::Rgb(..))),
            "expected an RGB foreground from the Dracula theme"
        );
    }
}
