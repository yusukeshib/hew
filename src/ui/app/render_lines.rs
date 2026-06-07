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
        for (i, (code_spans, code_w)) in wrapped.into_iter().enumerate() {
            let prefix = if i == 0 {
                format!("{num} ")
            } else {
                " ".repeat(PREFIX)
            };
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme().muted))];
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
        for (i, (code_spans, code_w)) in wrapped.into_iter().enumerate() {
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
            // `code_w` is the visual line's content width, returned by wrap_runs.
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
}
