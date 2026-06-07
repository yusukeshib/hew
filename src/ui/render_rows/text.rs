//! Text measurement, width-wrapping, and line sanitization helpers.

use super::*;

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Display width of a single char (control chars treated as 0; they're stripped
/// before rendering anyway).
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Take the longest prefix of `s` whose display width does not exceed `max`,
/// returning the prefix and its actual width. A wide glyph straddling the
/// boundary is dropped (so the result never overflows `max`).
pub fn take_width(s: &str, max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    (out, w)
}

/// Format a `SystemTime` as a UTC `YYYY-MM-DD HH:MM` timestamp (no external
/// date crate).
pub(super) fn fmt_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm) = (tod / 3600, (tod % 3600) / 60);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// Greedy word-wrap to `width` display columns, hard-splitting over-long words.
/// All measurements are in terminal cells (wide glyphs count as 2).
pub(super) fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut w = 0usize;
    // Append `word` to the current line, hard-splitting it across lines when it
    // overflows. Only break before a glyph when the current line is non-empty,
    // so a single glyph wider than `width` (e.g. a CJK char at width 1) lands on
    // its own line instead of emitting a spurious empty line ahead of it.
    let push_overlong = |word: &str, out: &mut Vec<String>, line: &mut String, w: &mut usize| {
        for ch in word.chars() {
            let cw = char_width(ch);
            if *w > 0 && *w + cw > width {
                out.push(std::mem::take(line));
                *w = 0;
            }
            line.push(ch);
            *w += cw;
        }
    };
    let push_word = |word: &str, out: &mut Vec<String>, line: &mut String, w: &mut usize| {
        let ww = str_width(word);
        if *w == 0 {
            push_overlong(word, out, line, w);
        } else if *w + 1 + ww <= width {
            line.push(' ');
            line.push_str(word);
            *w += 1 + ww;
        } else {
            out.push(std::mem::take(line));
            *w = 0;
            push_overlong(word, out, line, w);
        }
    };
    for word in s.split_whitespace() {
        push_word(word, &mut out, &mut line, &mut w);
    }
    out.push(line);
    out
}

/// Width-wrap that preserves every character verbatim — including runs of
/// spaces and leading/trailing whitespace. Used by the comment composer, which
/// is a live text buffer: unlike `wrap_text` (a display formatter that collapses
/// whitespace via `split_whitespace`), the editor must render exactly what the
/// user typed. Breaks greedily at the display-width boundary, preferring the
/// last space on the line so words don't split mid-token when avoidable.
pub(super) fn wrap_preserve(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = char_width(ch);
        if w + cw > width && !line.is_empty() {
            // Try to break at the last space so a word isn't split needlessly.
            if ch != ' ' {
                if let Some(brk) = line.rfind(' ') {
                    // Don't break on a trailing run of spaces (brk at end).
                    let tail: String = line[brk + 1..].to_string();
                    if !tail.is_empty() {
                        // Keep the break space on the first visual line so no
                        // character is dropped — this is a live edit buffer, and
                        // the rendered text (and caret position) must match the
                        // buffer verbatim.
                        line.truncate(brk + 1);
                        out.push(std::mem::take(&mut line));
                        line = tail;
                        w = str_width(&line);
                    } else {
                        out.push(std::mem::take(&mut line));
                        w = 0;
                    }
                } else {
                    out.push(std::mem::take(&mut line));
                    w = 0;
                }
            } else {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
        }
        line.push(ch);
        w += cw;
    }
    out.push(line);
    out
}

/// Make a line safe for a TUI cell grid. ratatui diffs cells between frames, so
/// a stray `\r`, tab, or ANSI escape corrupts the terminal and never self-heals.
/// We expand tabs (4-col stops), drop CR/LF, strip ANSI CSI/OSC sequences, and
/// drop any remaining control characters.
pub fn sanitize_line(s: &str) -> String {
    // Delegates to `sanitize_into`, whose fast path copies a clean printable-
    // ASCII line (the common case for source diffs) in a single allocation
    // instead of rebuilding it char-by-char. Scanning for cleanliness happens
    // once, inside `sanitize_into` — duplicating the check here would re-scan
    // every non-clean line before the (already expensive) char loop.
    let mut out = String::with_capacity(s.len());
    sanitize_into(&mut out, s);
    out
}

/// True when every byte is printable ASCII (`0x20..=0x7e`), so the string needs
/// no sanitization. Single vectorizable byte scan.
#[inline]
pub(super) fn is_clean_ascii(s: &str) -> bool {
    s.bytes().all(|b| b.wrapping_sub(0x20) < 0x5f)
}

/// Sanitize `s` and append the result to `out` (see [`sanitize_line`]). Lets a
/// caller that already needs an owned buffer (e.g. a line prefixed with a diff
/// sign) avoid a second allocation + copy. Tab stops are measured from the start
/// of `s` (column 0), independent of whatever `out` already holds, so a one-char
/// diff-sign prefix doesn't shift tab alignment — identical to sanitizing `s`
/// alone and then prepending the sign.
pub(super) fn sanitize_into(out: &mut String, s: &str) {
    // Fast path: append a clean ASCII line wholesale (single memcpy).
    if is_clean_ascii(s) {
        out.push_str(s);
        return;
    }
    let mut col = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => {
                let n = 4 - (col % 4);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' | '\n' => {}
            '\u{1b}' => match chars.peek() {
                // CSI: ESC [ ... <final 0x40..=0x7e>
                Some('[') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&p) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... (BEL | ESC \)
                Some(']') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if p == '\u{7}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() => {}
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
}
