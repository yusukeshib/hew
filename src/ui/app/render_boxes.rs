//! Comment/composer box and action-button line builders.

use super::*;

impl App {
    /// Buttons for a thread box's action row: reply, resolve/unresolve, and
    /// (only when the focused comment is a session-added one in this thread)
    /// delete. `(label-with-hotkey, action, label text color)` — the chip's
    /// background is a uniform subtle surface; this color is its foreground.
    pub(super) fn comment_buttons(&self, cl: &CommentLine) -> Vec<(String, ButtonAction, Color)> {
        let tid = cl.thread_id.clone();
        let mut v = vec![(
            "reply(r)".to_string(),
            ButtonAction::Reply(tid.clone()),
            theme().accent,
        )];
        if cl.resolved {
            v.push((
                "unresolve(R)".to_string(),
                ButtonAction::ToggleResolve(tid.clone()),
                theme().warn,
            ));
        } else {
            v.push((
                "resolve(R)".to_string(),
                ButtonAction::ToggleResolve(tid.clone()),
                theme().added,
            ));
        }
        if let Some((ftid, fcid)) = self.focused_comment() {
            if ftid == tid && !self.base_comment_ids.contains(&fcid) {
                v.push((
                    "delete(D)".to_string(),
                    ButtonAction::Delete(tid, fcid),
                    theme().removed,
                ));
            }
        }
        v
    }

    /// Buttons for the open composer's action row: submit and cancel.
    pub(super) fn composer_buttons(&self) -> Vec<(String, ButtonAction, Color)> {
        vec![
            (
                "submit(ctrl+s)".to_string(),
                ButtonAction::Submit,
                theme().accent,
            ),
            (
                "cancel(esc)".to_string(),
                ButtonAction::Cancel,
                theme().muted,
            ),
        ]
    }

    /// Build one full-width action-button row inside a box. The box (margin +
    /// `│ … │` + button chips) is `box_w` cells wide and starts `left_pad`
    /// cells into the row (for split-view side placement); the rest is padded
    /// to `width`. Each chip is a subtle raised surface with the action's color
    /// as its label text, and its on-screen rect is recorded (at `x0` + offset,
    /// row `y`) for click hit-testing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn action_row_line(
        &self,
        btns: &[(String, ButtonAction, Color)],
        border: Color,
        x0: u16,
        y: u16,
        width: usize,
        left_pad: usize,
        box_w: usize,
    ) -> Line<'static> {
        const MARGIN: usize = 2;
        let bstyle = Style::default().fg(border);
        if box_w <= MARGIN + 2 {
            return Line::from(Span::raw(" ".repeat(width)));
        }
        let inner_w = box_w - MARGIN - 2;
        let mut spans: Vec<Span<'static>> = Vec::new();
        if left_pad > 0 {
            spans.push(Span::raw(" ".repeat(left_pad)));
        }
        spans.push(Span::raw(" ".repeat(MARGIN)));
        spans.push(Span::styled("│".to_string(), bstyle));
        // Inner content: ` label ` chips packed left-to-right. Each chip is a
        // subtle raised surface with the action's color as the *text* (a soft,
        // toolbar-like button rather than a loud solid fill); the chip padding
        // and distinct text colors keep them readable as separate buttons even
        // packed tight, which matters in split view's narrow side column. Each
        // chip's screen rect is recorded for click hit-testing.
        let mut col = 0usize;
        let base_x = x0 + (left_pad + MARGIN + 1) as u16;
        let mut inner: Vec<Span<'static>> = Vec::new();
        for (i, (label, action, color)) in btns.iter().enumerate() {
            let chip = format!(" {label} ");
            let w = str_width(&chip);
            // A one-cell (box-background) gap before each chip after the first,
            // so the raised surfaces read as separate buttons. The gap is part
            // of the fit test, so it never dangles and drops the next chip.
            let gap = usize::from(i > 0);
            if col + gap + w > inner_w {
                break;
            }
            if gap > 0 {
                inner.push(Span::raw(" "));
                col += 1;
            }
            self.button_hits.borrow_mut().push((
                Rect {
                    x: base_x + col as u16,
                    y,
                    width: w as u16,
                    height: 1,
                },
                action.clone(),
            ));
            inner.push(Span::styled(
                chip,
                Style::default()
                    .bg(theme().subtle)
                    .fg(*color)
                    .add_modifier(Modifier::BOLD),
            ));
            col += w;
        }
        if col < inner_w {
            inner.push(Span::raw(" ".repeat(inner_w - col)));
        }
        spans.extend(inner);
        spans.push(Span::styled("│".to_string(), bstyle));
        let used = left_pad + box_w;
        if used < width {
            spans.push(Span::raw(" ".repeat(width - used)));
        }
        Line::from(spans)
    }

    /// Overlay a right-aligned button chip on an already-built diff line: clip
    /// the line's content to leave room, then append ` label `. Used to float a
    /// `comment(i)` button at the end of the focused diff line (GitHub-style).
    /// Records the chip's screen rect (at `x0` + offset, row `y`) for clicks.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn append_right_button(
        &self,
        spans: Vec<Span<'static>>,
        width: usize,
        label: &str,
        color: Color,
        action: ButtonAction,
        x0: u16,
        y: u16,
    ) -> Line<'static> {
        let chip = format!(" {label} ");
        let cw = str_width(&chip);
        if width <= cw {
            return Line::from(spans);
        }
        let keep = width - cw;
        // Clip the existing spans to `keep` display cells (preserving styles,
        // so the line's selection tint carries up to the button).
        let mut out: Vec<Span<'static>> = Vec::new();
        let mut acc = 0usize;
        for s in spans {
            if acc >= keep {
                break;
            }
            let sw = str_width(&s.content);
            if acc + sw <= keep {
                acc += sw;
                out.push(s);
            } else {
                let (t, tw) = take_width(&s.content, keep - acc);
                out.push(Span::styled(t, s.style));
                acc += tw;
                break;
            }
        }
        if acc < keep {
            out.push(Span::raw(" ".repeat(keep - acc)));
        }
        self.button_hits.borrow_mut().push((
            Rect {
                x: x0 + keep as u16,
                y,
                width: cw as u16,
                height: 1,
            },
            action,
        ));
        out.push(Span::styled(
            chip,
            Style::default()
                .bg(theme().subtle)
                .fg(color)
                .add_modifier(Modifier::BOLD),
        ));
        Line::from(out)
    }

    /// Render one inline comment line as part of a rounded box spanning `width`
    /// (2-column left margin + `╭─╮`/`│ │`/`╰─╯` frame). `focused` is set for
    /// the rows of the message the cursor is on.
    pub(super) fn comment_line_to_line(
        &self,
        cl: &CommentLine,
        focused: bool,
        width: usize,
    ) -> Line<'static> {
        const MARGIN: usize = 2;
        // Border brightness signals focus: the focused message keeps the bright
        // (theme) border so the cursor is visible even on a resolved thread
        // (whose box is otherwise dimmed). Only an *unfocused* resolved thread
        // reads as fully settled. Without the focus check first, landing on a
        // resolved comment gave no visual feedback at all.
        let border_col = if focused {
            theme().border_focus
        } else if cl.resolved {
            theme().muted
        } else {
            theme().border_unfocus
        };
        let bstyle = Style::default().fg(border_col);
        // Box occupies cols [MARGIN, width); inner_w is the span between borders.
        let inner_w = width.saturating_sub(MARGIN + 2);
        if width <= MARGIN + 2 {
            return Line::from(Span::raw(" ".repeat(width)));
        }
        let margin = Span::raw(" ".repeat(MARGIN));
        match &cl.kind {
            CommentKind::Top => Line::from(vec![
                margin,
                Span::styled(format!("╭{}╮", "─".repeat(inner_w)), bstyle),
            ]),
            CommentKind::Bottom => Line::from(vec![
                margin,
                Span::styled(format!("╰{}╯", "─".repeat(inner_w)), bstyle),
            ]),
            // The action row is normally drawn by `action_row_line` straight
            // from the render loop (it needs the screen position to record
            // click rects); this is a blank-box fallback if it's ever reached.
            CommentKind::Actions => Line::from(vec![
                margin,
                Span::styled("│".to_string(), bstyle),
                Span::raw(" ".repeat(inner_w)),
                Span::styled("│".to_string(), bstyle),
            ]),
            CommentKind::Author { name, date } => {
                // Name on the left, date flush right (1-col gutter before the
                // border). Spans below total exactly `inner_w`.
                let left = format!(" @{name}");
                let llen = str_width(&left);
                let dlen = str_width(date);
                let name_col = if cl.resolved {
                    theme().muted
                } else {
                    theme().warn
                };
                let name_style = Style::default().fg(name_col).add_modifier(Modifier::BOLD);
                let inner = if llen + dlen + 2 <= inner_w {
                    vec![
                        Span::styled(left, name_style),
                        Span::raw(" ".repeat(inner_w - llen - dlen - 1)),
                        Span::styled(date.clone(), Style::default().fg(theme().muted)),
                        Span::raw(" "),
                    ]
                } else {
                    let (l, lw) = take_width(&left, inner_w);
                    let pad = inner_w - lw;
                    vec![Span::styled(l, name_style), Span::raw(" ".repeat(pad))]
                };
                let mut spans = vec![margin, Span::styled("│".to_string(), bstyle)];
                spans.extend(inner);
                spans.push(Span::styled("│".to_string(), bstyle));
                Line::from(spans)
            }
            _ => {
                let (content, color, bold) = match &cl.kind {
                    CommentKind::Head { replies } => (
                        format!(
                            " ▾ {} · {} message{}",
                            if cl.resolved { "resolved" } else { "open" },
                            replies,
                            if *replies == 1 { "" } else { "s" }
                        ),
                        if cl.resolved {
                            theme().muted
                        } else {
                            theme().accent
                        },
                        true,
                    ),
                    CommentKind::Body(b) => (
                        format!(" {b}"),
                        if cl.resolved {
                            theme().muted
                        } else {
                            theme().text
                        },
                        false,
                    ),
                    _ => (String::new(), theme().text, false), // Gap
                };
                let mut cstyle = Style::default().fg(color);
                if bold {
                    cstyle = cstyle.add_modifier(Modifier::BOLD);
                }
                let clen = str_width(&content);
                let content = if clen > inner_w {
                    take_width(&content, inner_w).0
                } else {
                    format!("{content}{}", " ".repeat(inner_w - clen))
                };
                Line::from(vec![
                    margin,
                    Span::styled("│".to_string(), bstyle),
                    Span::styled(content, cstyle),
                    Span::styled("│".to_string(), bstyle),
                ])
            }
        }
    }

    /// Render one inline composer line as part of a rounded accent box spanning
    /// `width` (2-col left margin + framed title / live body / key hint).
    pub(super) fn composer_line_to_line(&self, cl: &ComposerLine, width: usize) -> Line<'static> {
        const MARGIN: usize = 2;
        let bstyle = Style::default().fg(theme().accent);
        let inner_w = width.saturating_sub(MARGIN + 2);
        if width <= MARGIN + 2 {
            return Line::from(Span::raw(" ".repeat(width)));
        }
        let margin = Span::raw(" ".repeat(MARGIN));
        // Pad/truncate `s` to exactly `inner_w` display cells.
        let fit = |s: &str| -> String {
            let w = str_width(s);
            if w > inner_w {
                take_width(s, inner_w).0
            } else {
                format!("{s}{}", " ".repeat(inner_w - w))
            }
        };
        match &cl.kind {
            ComposerKind::Top { title } => {
                let t = take_width(title, inner_w).0;
                let dashes = inner_w.saturating_sub(str_width(&t));
                Line::from(vec![
                    margin,
                    Span::styled("╭".to_string(), bstyle),
                    Span::styled(
                        t,
                        Style::default()
                            .fg(theme().accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("─".repeat(dashes), bstyle),
                    Span::styled("╮".to_string(), bstyle),
                ])
            }
            ComposerKind::Bottom => Line::from(vec![
                margin,
                Span::styled(format!("╰{}╯", "─".repeat(inner_w)), bstyle),
            ]),
            ComposerKind::Body(b) => {
                let text_style = Style::default().fg(theme().text);
                let mut spans = vec![margin, Span::styled("│".to_string(), bstyle)];
                // The caret is an *overlay*, not a character: render the cell at
                // the cursor with a reversed (block) style instead of splicing a
                // glyph in, so the surrounding text never shifts.
                if let Some(idx) = b.find(COMPOSER_CARET) {
                    let pre = &b[..idx];
                    let after = &b[idx + COMPOSER_CARET.len_utf8()..];
                    // The character sitting under the cursor (or a space at EOL).
                    let mut chars = after.chars();
                    let under = chars.next();
                    let tail: String = chars.collect();
                    let mut cursor_cell =
                        under.map(|c| c.to_string()).unwrap_or_else(|| " ".into());
                    // Lay out exactly `inner_w` cells, honoring the same
                    // pad/truncate contract as `fit`: reserve the cursor cell
                    // first (so the caret is never the thing that gets clipped),
                    // then fit the lead text and tail around it. A wide cursor
                    // glyph wider than the whole box degrades to a space.
                    if str_width(&cursor_cell) > inner_w {
                        cursor_cell = " ".into();
                    }
                    let cursor_w = str_width(&cursor_cell);
                    let (lead, lead_w) = take_width(&format!(" {pre}"), inner_w - cursor_w);
                    let (tail, tail_w) = take_width(&tail, inner_w - lead_w - cursor_w);
                    let pad = inner_w - lead_w - cursor_w - tail_w;
                    spans.push(Span::styled(lead, text_style));
                    spans.push(Span::styled(
                        cursor_cell,
                        text_style.add_modifier(Modifier::REVERSED),
                    ));
                    spans.push(Span::styled(
                        format!("{tail}{}", " ".repeat(pad)),
                        text_style,
                    ));
                } else {
                    spans.push(Span::styled(fit(&format!(" {b}")), text_style));
                }
                spans.push(Span::styled("│".to_string(), bstyle));
                Line::from(spans)
            }
            ComposerKind::Hint => Line::from(vec![
                margin,
                Span::styled("│".to_string(), bstyle),
                Span::styled(
                    fit(" ctrl+s: submit · enter: newline · esc: cancel"),
                    Style::default().fg(theme().muted),
                ),
                Span::styled("│".to_string(), bstyle),
            ]),
        }
    }
}
