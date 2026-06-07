//! Per-row line builders (diff lines, comment/composer boxes, buttons).

use super::*;

impl App {
    /// One logical split row expanded into its display lines. A single line
    /// unless `wrap` is on and a `Pair` has code wide enough to wrap on either
    /// side; the pair's height is the taller of the two columns, with the
    /// shorter column blank-padded so the divider stays aligned.
    pub(super) fn split_row_to_lines(
        &self,
        row: &SplitRow,
        selected: bool,
        side_w: usize,
        divider: &str,
    ) -> Vec<Line<'static>> {
        match &row.kind {
            SplitRowKind::Pair { left, right } if self.wrap => {
                let lr = self.side_line_rows(left.as_ref(), row.file_idx, side_w, selected);
                let rr = self.side_line_rows(right.as_ref(), row.file_idx, side_w, selected);
                let n = lr.len().max(rr.len());
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let mut spans = lr
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| self.blank_side(left.as_ref(), side_w, selected));
                    spans.push(Span::styled(
                        divider.to_string(),
                        Style::default().fg(theme().subtle),
                    ));
                    spans.extend(
                        rr.get(i)
                            .cloned()
                            .unwrap_or_else(|| self.blank_side(right.as_ref(), side_w, selected)),
                    );
                    out.push(Line::from(spans));
                }
                out
            }
            _ => vec![self.split_row_to_line(row, selected, side_w, divider)],
        }
    }

    /// A blank `width`-wide column for continuation rows of the shorter side of
    /// a wrapped pair, tinted with that side's background so it reads as part
    /// of the same cell.
    pub(super) fn blank_side(
        &self,
        cell: Option<&SideCell>,
        width: usize,
        selected: bool,
    ) -> Vec<Span<'static>> {
        let st = match cell {
            None => Style::default().bg(theme().comment_bg),
            Some(c) => {
                let bg = if selected {
                    Some(self.diff_cursor_bg())
                } else {
                    match c.kind {
                        LineKind::Addition => Some(theme().add_bg),
                        LineKind::Deletion => Some(theme().del_bg),
                        LineKind::Context => None,
                    }
                };
                bg.map_or(Style::default(), |b| Style::default().bg(b))
            }
        };
        vec![Span::styled(" ".repeat(width), st)]
    }

    pub(super) fn split_row_to_line(
        &self,
        row: &SplitRow,
        selected: bool,
        side_w: usize,
        divider: &str,
    ) -> Line<'static> {
        match &row.kind {
            SplitRowKind::FileHeader => Line::from(Span::styled(
                format!("▌ {}", row.text),
                Style::default()
                    .fg(theme().text_strong)
                    .bg(theme().file_header_bg)
                    .add_modifier(Modifier::BOLD),
            )),
            SplitRowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default()
                    .fg(theme().faint)
                    .add_modifier(Modifier::ITALIC),
            )),
            SplitRowKind::Pair { left, right } => {
                let mut spans = self.side_spans(left.as_ref(), row.file_idx, side_w, selected);
                spans.push(Span::styled(
                    divider.to_string(),
                    Style::default().fg(theme().subtle),
                ));
                spans.extend(self.side_spans(right.as_ref(), row.file_idx, side_w, selected));
                Line::from(spans)
            }
            SplitRowKind::Comment { side, line: cl } => {
                // Render the thread under the column it is anchored to: old
                // (deletion) comments on the left, new (addition) comments on
                // the right. The opposite column is left blank.
                let body = self.comment_line_to_line(cl, selected, side_w).spans;
                match side {
                    Side::Old => {
                        let mut spans = body;
                        // Pad out to the divider + right column so the row
                        // fills its width.
                        spans.push(Span::styled(
                            " ".repeat(divider.chars().count() + side_w),
                            Style::default(),
                        ));
                        Line::from(spans)
                    }
                    Side::New => {
                        let pad = side_w + divider.chars().count();
                        let mut spans = vec![Span::styled(" ".repeat(pad), Style::default())];
                        spans.extend(body);
                        Line::from(spans)
                    }
                }
            }
            SplitRowKind::Composer { side, line: cl } => {
                // Same side-aware placement as comment rows: render under the
                // anchored column, blank the other.
                let body = self.composer_line_to_line(cl, side_w).spans;
                match side {
                    Side::Old => {
                        let mut spans = body;
                        spans.push(Span::styled(
                            " ".repeat(divider.chars().count() + side_w),
                            Style::default(),
                        ));
                        Line::from(spans)
                    }
                    Side::New => {
                        let pad = side_w + divider.chars().count();
                        let mut spans = vec![Span::styled(" ".repeat(pad), Style::default())];
                        spans.extend(body);
                        Line::from(spans)
                    }
                }
            }
        }
    }

    /// Render one side (old/new) of a split pair into spans of width `width`.
    pub(super) fn side_spans(
        &self,
        cell: Option<&SideCell>,
        file_idx: usize,
        width: usize,
        selected: bool,
    ) -> Vec<Span<'static>> {
        const PREFIX: usize = 5; // line number(4) + space(1)
        match cell {
            None => vec![Span::styled(
                " ".repeat(width),
                Style::default().bg(theme().comment_bg),
            )],
            Some(c) => {
                let num = c
                    .line
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let bg = if selected {
                    Some(self.diff_cursor_bg())
                } else {
                    match c.kind {
                        LineKind::Addition => Some(theme().add_bg),
                        LineKind::Deletion => Some(theme().del_bg),
                        LineKind::Context => None,
                    }
                };
                let mut spans = vec![Span::styled(
                    format!("{num} "),
                    Style::default().fg(theme().muted),
                )];
                spans.extend(self.styled_fit(file_idx, &c.text, width.saturating_sub(PREFIX), bg));
                spans
            }
        }
    }

    /// Soft-wrap one side of a split pair into `>= 1` rows, each exactly `width`
    /// cells wide (prefix + code + tint fill). The first row carries the line
    /// number; continuation rows pad the 5-col prefix. Mirrors `side_spans` for
    /// the non-wrapping single-row case.
    pub(super) fn side_line_rows(
        &self,
        cell: Option<&SideCell>,
        file_idx: usize,
        width: usize,
        selected: bool,
    ) -> Vec<Vec<Span<'static>>> {
        const PREFIX: usize = 5;
        let Some(c) = cell else {
            return vec![vec![Span::styled(
                " ".repeat(width),
                Style::default().bg(theme().comment_bg),
            )]];
        };
        let num = c
            .line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".into());
        let bg = if selected {
            Some(self.diff_cursor_bg())
        } else {
            match c.kind {
                LineKind::Addition => Some(theme().add_bg),
                LineKind::Deletion => Some(theme().del_bg),
                LineKind::Context => None,
            }
        };
        let budget = width.saturating_sub(PREFIX);
        let runs = self.hl.runs(file_idx, &c.text);
        let wrapped = wrap_runs(&runs, budget, bg);
        let mut out = Vec::with_capacity(wrapped.len());
        for (i, code_spans) in wrapped.into_iter().enumerate() {
            let prefix = if i == 0 {
                format!("{num} ")
            } else {
                " ".repeat(PREFIX)
            };
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme().muted))];
            let code_w: usize = code_spans.iter().map(|s| str_width(&s.content)).sum();
            spans.extend(code_spans);
            let used = PREFIX + code_w;
            // Pad to the full column width so the divider/right side align.
            // `styled_fit` always pads (even with no bg), so match that to keep
            // the column a fixed width regardless of tint.
            if used < width {
                let mut st = Style::default();
                if let Some(b) = bg {
                    st = st.bg(b);
                }
                spans.push(Span::styled(" ".repeat(width - used), st));
            }
            out.push(spans);
        }
        out
    }

    /// One logical unified row expanded into its display lines. A single line
    /// unless `wrap` is on and the row is a long code line, in which case the
    /// code is soft-wrapped with continuation lines indented under it.
    pub(super) fn row_to_lines(
        &self,
        row: &Row,
        selected: bool,
        width: usize,
    ) -> Vec<Line<'static>> {
        match &row.kind {
            RowKind::Line {
                kind,
                old_line,
                new_line,
            } if self.wrap => {
                self.wrapped_code_lines(row, *kind, *old_line, *new_line, selected, width)
            }
            _ => vec![self.row_to_line(row, selected, width)],
        }
    }

    /// Soft-wrap a unified code line. The first display line carries the line
    /// numbers + sign; continuation lines pad that 13-col prefix so the code
    /// stays aligned, and the diff background tint spans every line.
    pub(super) fn wrapped_code_lines(
        &self,
        row: &Row,
        kind: LineKind,
        old_line: Option<u32>,
        new_line: Option<u32>,
        selected: bool,
        width: usize,
    ) -> Vec<Line<'static>> {
        let num = format!(
            "{:>5} {:>5} ",
            old_line.map(|n| n.to_string()).unwrap_or_default(),
            new_line.map(|n| n.to_string()).unwrap_or_default(),
        );
        let (sign, code) = row.text.split_at(1);
        let bg = if selected {
            Some(self.diff_cursor_bg())
        } else {
            match kind {
                LineKind::Addition => Some(theme().add_bg),
                LineKind::Deletion => Some(theme().del_bg),
                LineKind::Context => None,
            }
        };
        let sign_color = match kind {
            LineKind::Addition => theme().added,
            LineKind::Deletion => theme().removed,
            LineKind::Context => theme().muted,
        };
        let with_bg = |st: Style| match bg {
            Some(b) => st.bg(b),
            None => st,
        };
        let budget = width.saturating_sub(Self::UNI_PREFIX);
        let runs = self.hl.runs(row.file_idx, code);
        let wrapped = wrap_runs(&runs, budget, bg);
        let mut out = Vec::with_capacity(wrapped.len());
        for (i, code_spans) in wrapped.into_iter().enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if i == 0 {
                spans.push(Span::styled(
                    num.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                spans.push(Span::styled(
                    sign.to_string(),
                    with_bg(Style::default().fg(sign_color)),
                ));
            } else {
                spans.push(Span::styled(
                    " ".repeat(Self::UNI_PREFIX),
                    with_bg(Style::default()),
                ));
            }
            let code_w: usize = code_spans.iter().map(|s| str_width(&s.content)).sum();
            spans.extend(code_spans);
            let used = Self::UNI_PREFIX + code_w;
            // Fill the rest so the tint / selection spans the whole line
            // (only when there is a background to extend, matching `row_to_line`).
            if bg.is_some() && used < width {
                spans.push(Span::styled(
                    " ".repeat(width - used),
                    with_bg(Style::default()),
                ));
            }
            out.push(Line::from(spans));
        }
        out
    }

    pub(super) fn row_to_line(&self, row: &Row, selected: bool, width: usize) -> Line<'static> {
        match &row.kind {
            RowKind::FileHeader => {
                let st = Style::default()
                    .fg(theme().text_strong)
                    .bg(theme().file_header_bg)
                    .add_modifier(Modifier::BOLD);
                let text = format!("▌ {}", row.text);
                let pad = width.saturating_sub(str_width(&text));
                Line::from(vec![
                    Span::styled(text, st),
                    Span::styled(" ".repeat(pad), st),
                ])
            }
            RowKind::HunkHeader => Line::from(Span::styled(
                row.text.clone(),
                Style::default()
                    .fg(theme().faint)
                    .add_modifier(Modifier::ITALIC),
            )),
            RowKind::Line {
                kind,
                old_line,
                new_line,
            } => {
                let num = format!(
                    "{:>5} {:>5} ",
                    old_line.map(|n| n.to_string()).unwrap_or_default(),
                    new_line.map(|n| n.to_string()).unwrap_or_default(),
                );
                let (sign, code) = row.text.split_at(1);
                let bg = if selected {
                    Some(self.diff_cursor_bg())
                } else {
                    match kind {
                        LineKind::Addition => Some(theme().add_bg),
                        LineKind::Deletion => Some(theme().del_bg),
                        LineKind::Context => None,
                    }
                };
                let sign_color = match kind {
                    LineKind::Addition => theme().added,
                    LineKind::Deletion => theme().removed,
                    LineKind::Context => theme().muted,
                };
                let with_bg = |st: Style| match bg {
                    Some(b) => st.bg(b),
                    None => st,
                };
                let mut used = str_width(&num) + 1;
                let mut spans = vec![
                    Span::styled(num, with_bg(Style::default().fg(theme().muted))),
                    Span::styled(sign.to_string(), with_bg(Style::default().fg(sign_color))),
                ];
                // Highlighted code, with the diff background tint behind it.
                let hl = self.hl.runs(row.file_idx, code);
                for (c, s) in hl.iter() {
                    used += str_width(s);
                    spans.push(Span::styled(s.clone(), with_bg(Style::default().fg(*c))));
                }
                // Fill the rest so the tint / selection spans the whole line.
                if bg.is_some() && used < width {
                    spans.push(Span::styled(
                        " ".repeat(width - used),
                        with_bg(Style::default()),
                    ));
                }
                Line::from(spans)
            }
            RowKind::Comment(cl) => self.comment_line_to_line(cl, selected, width),
            RowKind::Composer(cl) => self.composer_line_to_line(cl, width),
        }
    }

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
