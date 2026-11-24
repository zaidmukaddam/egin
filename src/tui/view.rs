//! Drawing. Top to bottom: the location bar, the transcript (or the welcome, a picker, the viewer or the shortcuts),
//! the turn row while one runs, the composer or the card that took its place, and the shortcut hints.

use super::{
    blocks,
    input::{Popup, COMMANDS},
    k, secs,
    text::{markdown, pad, trunc, width, wrap, wrap_chars},
    theme::th,
    Card, Focus, Item, Kind, Overlay, Ui,
};
use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};
use unicode_width::UnicodeWidthStr;

const TIPS: &[&str] = &[
    "Tab moves to the transcript: j/k pick a block, e folds it, enter opens it, y copies it.",
    "Type @ to mention a file from the workspace.",
    "Start with ! to run a shell command in the agent's sandbox.",
    "Enter while a turn runs queues your message for the agent's next step; ctrl+enter sends it now.",
    "ctrl+s sets a draft aside, and brings it back.",
    "/resume picks up an earlier session where it stopped.",
];

/// The input laid out in rows of `w` columns, and the cursor's (row, column).
pub fn input_rows(s: &str, cursor: usize, w: usize) -> (Vec<String>, (usize, usize)) {
    use unicode_width::UnicodeWidthChar;
    let w = w.max(2);
    let mut rows = vec![String::new()];
    let (mut col, mut pos) = (0, (0, 0));
    for (i, ch) in s.char_indices() {
        if ch != '\n' && col + ch.width().unwrap_or(0) > w {
            rows.push(String::new());
            col = 0;
        }
        if i == cursor {
            pos = (rows.len() - 1, col);
        }
        if ch == '\n' {
            rows.push(String::new());
            col = 0;
            continue;
        }
        rows.last_mut().unwrap().push(ch);
        col += ch.width().unwrap_or(0);
    }
    if cursor >= s.len() {
        if col >= w {
            rows.push(String::new());
            col = 0;
        }
        pos = (rows.len() - 1, col);
    }
    (rows, pos)
}

fn hints(spans: &mut Vec<Span<'static>>, keys: &[(&str, &str)]) {
    let t = th();
    for (i, (key, label)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", Style::new().fg(t.faint)));
        }
        spans.push(Span::styled(key.to_string(), Style::new().fg(t.text)));
        spans.push(Span::styled(format!(" {label}"), Style::new().fg(t.dim)));
    }
}

impl Ui {
    fn welcome_only(&self) -> bool {
        matches!(self.items.first(), Some(Item { kind: Kind::Welcome, .. }))
    }

    pub(crate) fn draw(&mut self, f: &mut Frame) {
        let t = th();
        let full = f.area();
        f.render_widget(Block::new().style(Style::new().bg(t.bg)), full);
        let compact = full.height <= 20;
        let area = full.inner(Margin { horizontal: 2, vertical: u16::from(!compact) });
        let cw = area.width as usize;
        self.hits = Default::default();
        self.logo_at = None;

        let card = self.card_lines(cw.saturating_sub(3));
        let text_w = cw.saturating_sub(6);
        let (rows, (crow, ccol)) = input_rows(&self.input, self.cursor, text_w);
        let shown = rows.len().min(8);
        let first = (crow + 1).saturating_sub(shown);
        let bottom_h = if card.is_empty() { shown as u16 + 2 } else { card.len() as u16 + 2 };
        let status_h = u16::from(self.turn.is_some());
        let [top, _, body, _, status, bottom, keys] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(!compact)),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(status_h),
            Constraint::Length(bottom_h),
            Constraint::Length(1),
        ])
        .areas(area);
        self.hits.body = body;

        self.draw_top(f, top);
        match &self.overlay {
            Some(Overlay::Viewer(_)) => self.draw_viewer(f, body),
            Some(Overlay::Help(_)) => self.draw_help(f, body),
            Some(Overlay::Picker(_)) => self.draw_picker(f, body),
            None if self.welcome_only() => self.draw_welcome(f, body),
            None => self.draw_transcript(f, body),
        }
        if status_h > 0 {
            self.draw_status(f, status);
        }
        if !card.is_empty() {
            self.draw_card(f, bottom, card);
        } else {
            self.draw_composer(f, bottom, &rows, first, shown, (crow, ccol));
        }
        if let Some(p) = self.popup() {
            self.draw_popup(f, body, &p);
        }
        self.draw_keys(f, keys);
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let t = th();
        let name = self.agent.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let path = self.agent.dir.display().to_string();
        let path = if !home.is_empty() && path.starts_with(&home) { format!("~{}", &path[home.len()..]) } else { path };
        let sep = || Span::styled("  │  ", Style::new().fg(t.faint));
        let mut right: Vec<Span> = vec![];
        if self.yolo {
            right.push(Span::styled("always-approve", Style::new().fg(t.warn)));
        }
        if self.ctx > 0 {
            if !right.is_empty() {
                right.push(sep());
            }
            match self.window {
                Some(w) => {
                    let pct = self.ctx as f64 / w as f64;
                    let color = if pct >= 0.95 {
                        t.err
                    } else if pct >= 0.75 {
                        t.warn
                    } else {
                        t.accent
                    };
                    let filled = ((pct * 8.0).ceil() as usize).min(8);
                    right.push(Span::styled("▰".repeat(filled), Style::new().fg(color)));
                    right.push(Span::styled("▱".repeat(8 - filled), Style::new().fg(t.faint)));
                    right.push(Span::styled(format!(" {} / {}", k(self.ctx), k(w)), Style::new().fg(if pct >= 0.75 { color } else { t.muted })));
                }
                None => right.push(Span::styled(format!("{} ctx", k(self.ctx)), Style::new().fg(t.dim))),
            }
        }
        if self.cost > 0.0 {
            if !right.is_empty() {
                right.push(sep());
            }
            right.push(Span::styled(format!("${:.4}", self.cost), Style::new().fg(t.dim)));
        }
        let rw = width(&right);
        let mut left =
            vec![Span::styled("egin", Style::new().fg(t.fg).add_modifier(Modifier::BOLD)), Span::raw("  "), Span::styled(name, Style::new().fg(t.muted))];
        if let Some(b) = &self.branch {
            left.push(Span::styled(format!("  ⎇ {b}"), Style::new().fg(t.dim)));
        }
        let used = width(&left);
        left.push(Span::styled(format!("  {}", trunc(&path, (area.width as usize).saturating_sub(used + rw + 6))), Style::new().fg(t.dim)));
        f.render_widget(Paragraph::new(Line::from(left)), area);
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }

    fn draw_welcome(&mut self, f: &mut Frame, body: Rect) {
        let t = th();
        let menu: [(&str, &str, &'static str); 4] =
            [("Resume a session", "ctrl+r", "resume"), ("Switch model", "/model", "model"), ("Keys and commands", "?", "help"), ("Quit", "ctrl+d", "quit")];
        let logo_rows = self.logo.as_ref().map_or(1, |l| l.rows as usize);
        let bw = (self.logo.as_ref().map_or(0, |l| l.cols as usize)).max(52).min(body.width as usize);
        let c = &self.agent.cfg;
        let mut lines: Vec<Line<'static>> = vec![];
        match &self.logo {
            Some(_) => lines.extend(std::iter::repeat_n(Line::default(), logo_rows)),
            None => lines.push(Line::from(vec![
                Span::styled("EGI", Style::new().fg(t.fg).add_modifier(Modifier::BOLD)),
                Span::styled("N", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            ])),
        }
        lines.push(Line::default());
        let info = format!("v{} · {} · sandbox {}", env!("CARGO_PKG_VERSION"), c.model, c.sandbox);
        lines.extend(wrap(&[], &[], &[Span::styled(info, Style::new().fg(t.muted))], bw));
        for chosen in &self.agent.chosen {
            lines.extend(wrap(&[], &[], &[Span::styled(chosen.clone(), Style::new().fg(t.dim))], bw));
        }
        lines.push(Line::default());
        let menu_at = lines.len();
        for (label, key, _) in &menu {
            let gap = bw.saturating_sub(label.width() + key.width());
            lines.push(Line::from(vec![
                Span::styled(label.to_string(), Style::new().fg(t.fg).add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(gap)),
                Span::styled(key.to_string(), Style::new().fg(t.muted)),
            ]));
        }
        lines.push(Line::default());
        let tip = TIPS[(self.sid.bytes().map(usize::from).sum::<usize>()) % TIPS.len()];
        lines.extend(wrap(&[Span::styled("Tip  ", Style::new().fg(t.accent))], &[Span::raw("     ")], &[Span::styled(tip, Style::new().fg(t.dim))], bw));
        // Notes and errors from before the first message (a failed command, a model switch) sit under it.
        for it in self.items.iter().skip(1) {
            lines.push(Line::default());
            lines.extend(self.item_lines(it, bw));
        }
        let h = lines.len() as u16;
        let x = body.x + (body.width.saturating_sub(bw as u16)) / 2;
        let y = body.y + body.height.saturating_sub(h) / 3;
        let area = Rect { x, y, width: bw as u16, height: h.min(body.bottom().saturating_sub(y)) };
        if let Some(l) = &self.logo {
            if area.height as usize > logo_rows && (l.cols as usize) <= bw {
                self.logo_at = Some((x, y));
            }
        }
        for (i, (_, _, action)) in menu.iter().enumerate() {
            let row = y + (menu_at + i) as u16;
            if row < body.bottom() {
                self.hits.menu.push((Rect { x, y: row, width: bw as u16, height: 1 }, action));
            }
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_transcript(&mut self, f: &mut Frame, area: Rect) {
        let t = th();
        let w = area.width.saturating_sub(2) as usize;
        let laid = self.layout(w);
        let total = laid.lines.len();
        let h = area.height as usize;
        // Scrolled up: keep the same lines in view while new ones arrive below.
        if self.scroll > 0 && total > self.last_total {
            self.scroll += total - self.last_total;
        }
        self.last_total = total;
        let max = total.saturating_sub(h);
        self.scroll = self.scroll.min(max);
        let start = max - self.scroll;
        self.hits.start = start;
        for &(key, a, b) in &laid.order {
            if b >= start && a < start + h {
                let y0 = area.y + (a.max(start) - start) as u16;
                let y1 = area.y + (b.min(start + h - 1) - start) as u16;
                self.hits.units.push((y0, y1, key));
            }
        }
        self.hits.order = laid.order.clone();
        // A message of yours scrolled off the top stays pinned there while its turn is in view.
        let sticky = laid.turn.get(start).copied().flatten().filter(|(_, first)| start > *first + 1);
        let view: Vec<Line> = laid.lines.into_iter().skip(start).take(h).collect();
        let text_area = Rect { width: area.width.saturating_sub(1), ..area };
        f.render_widget(Paragraph::new(view), text_area);
        if let Some((u, _)) = sticky {
            if let Some(Item { kind: Kind::User(text), .. }) = self.items.get(u) {
                let band = Style::new().bg(t.band);
                let line = pad(
                    Line::from(vec![
                        Span::styled("  ❯ ", band.fg(t.accent)),
                        Span::styled(trunc(text.lines().next().unwrap_or(""), w.saturating_sub(8)), band.fg(t.text)),
                    ]),
                    text_area.width as usize,
                    band,
                );
                f.render_widget(Paragraph::new(line), Rect { height: 1, ..text_area });
            }
        }
        if total > h {
            let mut state = ScrollbarState::new(max).position(start).viewport_content_length(h);
            let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("▐")
                .thumb_style(Style::new().fg(if self.scroll > 0 { t.muted } else { t.faint }));
            f.render_stateful_widget(bar, area, &mut state);
        }
    }

    fn draw_status(&mut self, f: &mut Frame, area: Rect) {
        let t = th();
        let Some(turn) = &self.turn else { return };
        let spin = blocks::SPINNER[(self.born.elapsed().as_millis() / 80) as usize % blocks::SPINNER.len()];
        let waiting = self.card.is_some();
        let phase = if self.cancelling { "Cancelling".to_string() } else { turn.phase.clone() };
        let mut left = vec![
            Span::styled(format!(" {} ", if waiting { "◆" } else { spin }), Style::new().fg(if waiting { t.warn } else { t.accent })),
            Span::styled(format!("{phase}…"), Style::new().fg(t.text)),
        ];
        if area.width > 60 {
            left.push(Span::styled(format!(" {}", secs(turn.phase_at.elapsed())), Style::new().fg(t.dim)));
        }
        let queued = self.items.iter().filter(|i| matches!(i.kind, Kind::Queued(_))).count();
        if queued > 0 {
            left.push(Span::styled(format!(" · {queued} queued, ctrl+enter to send now"), Style::new().fg(t.dim)));
        }
        let mut right = vec![Span::styled(secs(turn.started.elapsed()), Style::new().fg(t.muted))];
        if turn.out_tok > 0 {
            right.push(Span::styled(format!(" ⇣{}", k(turn.out_tok)), Style::new().fg(t.dim)));
        }
        right.push(Span::raw("  "));
        right.push(Span::styled("[stop]", Style::new().fg(t.err)));
        self.hits.stop = Some(Rect { x: area.right().saturating_sub(6), y: area.y, width: 6, height: 1 });
        f.render_widget(Paragraph::new(Line::from(left)), area);
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }

    fn draw_composer(&mut self, f: &mut Frame, area: Rect, rows: &[String], first: usize, shown: usize, (crow, ccol): (usize, usize)) {
        let t = th();
        let focused = self.focus == Focus::Prompt && self.overlay.is_none();
        self.hits.composer = Some(area);
        let mode = if self.yolo {
            Span::styled("always-approve ", Style::new().fg(t.warn))
        } else {
            Span::styled(format!("approvals {} ", self.agent.cfg.approval), Style::new().fg(t.dim))
        };
        let caption = Line::from(vec![Span::styled(format!(" {} · ", self.agent.cfg.model), Style::new().fg(t.muted)), mode]).right_aligned();
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if focused { t.border_on } else { t.border }))
            .title_bottom(caption);
        if self.shell_mode {
            block = block.title_top(Line::from(Span::styled(" shell · runs in the agent's sandbox ", Style::new().fg(t.warn))));
        }
        let inner = block.inner(area);
        f.render_widget(block, area);
        let (glyph, color) = if self.shell_mode { (" ! ", t.warn) } else { (" ❯ ", t.accent) };
        let lead = || Span::styled(glyph, Style::new().fg(if focused { color } else { t.dim }).add_modifier(Modifier::BOLD));
        let mut lines = vec![];
        if self.input.is_empty() {
            let hint = if self.shell_mode {
                "A shell command, like ls or git status"
            } else if self.turn.is_some() {
                "Add to the task; it reaches the agent at its next step"
            } else if self.focus == Focus::Scrollback {
                "Tab or i to type here"
            } else {
                "Ask egin to build, fix or explain anything"
            };
            lines.push(Line::from(vec![lead(), Span::styled(hint, Style::new().fg(t.dim))]));
        } else {
            for (i, r) in rows.iter().enumerate().skip(first).take(shown) {
                let l = if i == 0 { lead() } else { Span::raw("   ") };
                // Paste chips read as chips.
                let mut spans = vec![l];
                let mut rest = r.as_str();
                while let Some((tag, pos)) = self.pastes.iter().find_map(|(tag, _)| rest.find(tag.as_str()).map(|p| (tag, p))) {
                    spans.push(Span::styled(rest[..pos].to_string(), Style::new().fg(t.fg)));
                    spans.push(Span::styled(tag.clone(), Style::new().fg(t.muted).bg(t.inline)));
                    rest = &rest[pos + tag.len()..];
                }
                spans.push(Span::styled(rest.to_string(), Style::new().fg(t.fg)));
                lines.push(Line::from(spans));
            }
        }
        f.render_widget(Paragraph::new(lines), inner);
        if focused {
            f.set_cursor_position((inner.x + 3 + ccol as u16, inner.y + (crow - first) as u16));
        }
    }

    /// The card's rows (no chrome); empty when no card waits.
    fn card_lines(&self, w: usize) -> Vec<Line<'static>> {
        let t = th();
        let band = Style::new().bg(t.band);
        let option = |n: usize, selected: bool, label: Vec<Span<'static>>| {
            let bg = if selected { t.hover } else { t.band };
            let mut spans = vec![
                Span::styled(format!("{n} "), Style::new().fg(t.accent).bg(bg)),
                Span::styled(if selected { "(●) " } else { "(○) " }, Style::new().fg(if selected { t.fg } else { t.dim }).bg(bg)),
            ];
            spans.extend(label.into_iter().map(|s| Span::styled(s.content.to_string(), s.style.bg(bg))));
            pad(Line::from(spans), w, Style::new().bg(bg))
        };
        let text = |s: &str, sel: bool| {
            Span::styled(s.to_string(), Style::new().fg(if sel { t.fg } else { t.text }).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }))
        };
        match &self.card {
            None => vec![],
            Some(Card::Approve { tool, args, from, pick, feedback, .. }) => {
                let title = match tool.as_str() {
                    "bash" => "Allow command?".to_string(),
                    "edit_file" | "write_file" => {
                        format!("Allow {} to {}?", if tool == "edit_file" { "an edit" } else { "writing" }, args["path"].as_str().unwrap_or("a file"))
                    }
                    "web_fetch" => "Allow fetching this page?".to_string(),
                    _ => format!("Allow {tool}?"),
                };
                let mut head = vec![Span::styled(title, band.fg(t.fg).add_modifier(Modifier::BOLD))];
                if let Some(f) = from {
                    head.push(Span::styled(format!("  from {f}"), band.fg(t.dim)));
                }
                let mut lines = vec![pad(Line::from(head), w, band)];
                let what = match tool.as_str() {
                    "bash" => format!("$ {}", args["command"].as_str().unwrap_or("")),
                    "edit_file" => {
                        let (o, n) = (args["old"].as_str().unwrap_or("").lines().count(), args["new"].as_str().unwrap_or("").lines().count());
                        format!("−{o} +{n} lines")
                    }
                    "write_file" => super::n_lines(args["content"].as_str().unwrap_or("").lines().count()),
                    _ => blocks::detail(tool, args),
                };
                let mut body = wrap_chars(&what, w.saturating_sub(2), band.fg(t.text));
                let more = body.len().saturating_sub(6);
                body.truncate(6);
                for l in body {
                    lines.push(pad(Line::from([vec![Span::styled("  ", band)], l.spans].concat()), w, band));
                }
                if more > 0 {
                    lines.push(pad(Line::from(Span::styled(format!("  … +{}", super::n_lines(more)), band.fg(t.dim))), w, band));
                }
                lines.push(pad(Line::default(), w, band));
                lines.push(option(1, *pick == 0, vec![text("Yes, proceed", *pick == 0)]));
                lines.push(option(2, *pick == 1, vec![text(&format!("Always allow {tool} this session"), *pick == 1)]));
                let reject = if feedback.is_empty() {
                    vec![Span::styled("No, reject (type to add feedback)", Style::new().fg(if *pick == 2 { t.text } else { t.dim }))]
                } else {
                    vec![Span::styled("❯ ", Style::new().fg(t.accent)), Span::styled(format!("{feedback}▏"), Style::new().fg(t.fg))]
                };
                lines.push(option(3, *pick == 2, reject));
                lines
            }
            Some(Card::Ask { question, options, pick, other, .. }) => {
                let mut lines: Vec<Line<'static>> = wrap(&[], &[], &[Span::styled(question.clone(), band.fg(t.fg).add_modifier(Modifier::BOLD))], w)
                    .into_iter()
                    .map(|l| pad(l, w, band))
                    .collect();
                lines.push(pad(Line::default(), w, band));
                for (i, o) in options.iter().enumerate() {
                    lines.push(option(i + 1, *pick == i, vec![text(o, *pick == i)]));
                }
                let n = options.len();
                let typed = if other.is_empty() {
                    vec![Span::styled("Type your answer here", Style::new().fg(t.dim))]
                } else {
                    vec![Span::styled("❯ ", Style::new().fg(t.accent)), Span::styled(format!("{other}▏"), Style::new().fg(t.fg))]
                };
                lines.push(option(n + 1, *pick == n, typed));
                lines
            }
        }
    }

    /// A card: a band with an accent bar down its left, in the composer's place.
    fn draw_card(&self, f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
        let t = th();
        let accent = if matches!(self.card, Some(Card::Approve { .. })) { t.warn } else { t.accent };
        f.render_widget(Block::new().style(Style::new().bg(t.band)), area);
        let bar: Vec<Line> = (0..area.height).map(|_| Line::from(Span::styled("┃", Style::new().fg(accent).bg(t.band)))).collect();
        f.render_widget(Paragraph::new(bar), Rect { width: 1, ..area });
        let inner = Rect { x: area.x + 3, y: area.y + 1, width: area.width.saturating_sub(3), height: area.height.saturating_sub(2) };
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_popup(&self, f: &mut Frame, body: Rect, p: &Popup) {
        let t = th();
        let rows: Vec<(String, String)> = match p {
            Popup::Commands(v) => v.iter().map(|(c, d)| (c.to_string(), d.to_string())).collect(),
            Popup::Files(v) => v.iter().map(|f| (format!("@{f}"), String::new())).collect(),
            Popup::Models(v) => v
                .iter()
                .map(|id| {
                    let d = self.models.iter().find(|(m, _)| m == id).map(|(_, i)| format!("{} ctx", k(i.context))).unwrap_or_default();
                    (id.clone(), d)
                })
                .collect(),
        };
        let h = (rows.len() as u16).min(body.height);
        let area = Rect { x: body.x, y: body.bottom().saturating_sub(h), width: body.width.saturating_sub(1), height: h };
        f.render_widget(Clear, area);
        let pick = self.pick.min(rows.len().saturating_sub(1));
        let label_w = rows.iter().map(|r| r.0.width()).max().unwrap_or(0).min(area.width as usize / 2) + 3;
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .map(|(i, (label, desc))| {
                let bg = if i == pick { t.hover } else { t.band };
                let style = Style::new().bg(bg);
                let name = if i == pick { style.fg(t.fg).add_modifier(Modifier::BOLD) } else { style.fg(t.text) };
                let label = trunc(label, label_w - 1);
                pad(
                    Line::from(vec![
                        Span::styled("  ", style),
                        Span::styled(format!("{label:<label_w$}"), name),
                        Span::styled(trunc(desc, (area.width as usize).saturating_sub(label_w + 4)), style.fg(t.dim)),
                    ]),
                    area.width as usize,
                    style,
                )
            })
            .collect();
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_picker(&self, f: &mut Frame, body: Rect) {
        let t = th();
        let Some(Overlay::Picker(p)) = &self.overlay else { return };
        let shown = p.shown();
        let w = body.width.saturating_sub(1) as usize;
        let mut lines = vec![
            Line::from(Span::styled(p.title, Style::new().fg(t.fg).add_modifier(Modifier::BOLD))),
            Line::from(vec![
                Span::styled("❯ ", Style::new().fg(t.accent)),
                if p.filter.is_empty() {
                    Span::styled("type to filter", Style::new().fg(t.dim))
                } else {
                    Span::styled(format!("{}▏", p.filter), Style::new().fg(t.fg))
                },
                Span::styled(format!("   {} of {}", shown.len(), p.rows.len()), Style::new().fg(t.faint)),
            ]),
            Line::default(),
        ];
        let room = (body.height as usize).saturating_sub(lines.len());
        let top = p.pick.saturating_sub(room.saturating_sub(1));
        let label_w = (w / 2).min(60);
        for (i, (label, detail, _)) in shown.iter().enumerate().skip(top).take(room) {
            let sel = i == p.pick;
            let bg = if sel { t.hover } else { t.bg };
            let style = Style::new().bg(bg);
            lines.push(pad(
                Line::from(vec![
                    Span::styled(if sel { "▎ " } else { "  " }, style.fg(t.accent)),
                    Span::styled(
                        format!("{:<label_w$}", trunc(label, label_w - 2)),
                        style.fg(if sel { t.fg } else { t.text }).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
                    ),
                    Span::styled(trunc(detail, w.saturating_sub(label_w + 4)), style.fg(t.dim)),
                ]),
                w,
                style,
            ));
        }
        f.render_widget(Paragraph::new(lines), body);
    }

    fn draw_viewer(&mut self, f: &mut Frame, body: Rect) {
        let t = th();
        let w = body.width.saturating_sub(2) as usize;
        let Some(Overlay::Viewer(v)) = &mut self.overlay else { return };
        let lines: Vec<Line<'static>> = if v.markdown {
            markdown(&v.text, w)
        } else {
            let n = v.text.lines().count().max(1);
            let nw = n.to_string().len();
            v.text
                .lines()
                .enumerate()
                .flat_map(|(i, l)| {
                    let rows = wrap_chars(l, w.saturating_sub(nw + 2), Style::new().fg(t.text));
                    rows.into_iter().enumerate().map(move |(j, r)| {
                        let num = if j == 0 { format!("{:>nw$}  ", i + 1) } else { " ".repeat(nw + 2) };
                        Line::from([vec![Span::styled(num, Style::new().fg(t.faint))], r.spans].concat())
                    })
                })
                .collect()
        };
        let h = body.height.saturating_sub(2) as usize;
        v.scroll = v.scroll.min(lines.len().saturating_sub(h));
        let pos = format!("{}–{} of {}", v.scroll + 1, (v.scroll + h).min(lines.len()), lines.len());
        let title = trunc(&v.title, w.saturating_sub(pos.width() + 2));
        let head = Line::from(vec![
            Span::styled(title.clone(), Style::new().fg(t.fg).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(w.saturating_sub(title.width() + pos.width()))),
            Span::styled(pos, Style::new().fg(t.dim)),
        ]);
        let mut out = vec![head, Line::from(Span::styled("─".repeat(w), Style::new().fg(t.faint)))];
        out.extend(lines.into_iter().skip(v.scroll).take(h));
        f.render_widget(Paragraph::new(out), body);
    }

    fn draw_help(&self, f: &mut Frame, body: Rect) {
        let t = th();
        let Some(Overlay::Help(scroll)) = &self.overlay else { return };
        let sections: &[(&str, &[(&str, &str)])] = &[
            (
                "Prompt",
                &[
                    ("enter", "send (while a turn runs: queue it for the agent's next step)"),
                    ("shift+enter · alt+enter · ctrl+j", "new line (a trailing \\ works too)"),
                    ("ctrl+enter", "send now: stop the turn and run this instead"),
                    ("/", "commands"),
                    ("@", "mention a file from the workspace"),
                    ("!", "run a shell command in the agent's sandbox"),
                    ("↑ ↓", "earlier messages"),
                    ("ctrl+s", "set the draft aside, or bring it back"),
                    ("esc esc", "clear the draft (it is stashed)"),
                ],
            ),
            (
                "Transcript (tab)",
                &[
                    ("j k · ↑ ↓", "pick a block"),
                    ("e · click", "fold or unfold it"),
                    ("← →", "fold · unfold"),
                    ("E", "fold or unfold everything"),
                    ("enter", "open it in the viewer"),
                    ("y · Y", "copy it · copy its command or path"),
                    ("H L", "previous · next message of yours"),
                    ("g G", "top · bottom"),
                    ("ctrl+u ctrl+d · pgup pgdn", "scroll"),
                    ("tab · i · esc", "back to the prompt"),
                ],
            ),
            (
                "Session",
                &[
                    ("ctrl+c", "stop the turn; pressed twice when idle, quit"),
                    ("ctrl+n · ctrl+r", "new session · resume one"),
                    ("ctrl+o · shift+tab", "always-approve on or off"),
                    ("ctrl+e", "show or fold all thinking"),
                    ("ctrl+d", "quit"),
                    ("shift+drag", "select text with the mouse"),
                ],
            ),
        ];
        let mut lines = vec![Line::from(Span::styled("Keys and commands", Style::new().fg(t.fg).add_modifier(Modifier::BOLD))), Line::default()];
        let key_w = 36;
        for (title, rows) in sections {
            lines.push(Line::from(Span::styled(title.to_string(), Style::new().fg(t.accent))));
            for (key, what) in rows.iter() {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {key:<key_w$}"), Style::new().fg(t.text)),
                    Span::styled(what.to_string(), Style::new().fg(t.dim)),
                ]));
            }
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled("Commands", Style::new().fg(t.accent))));
        for (c, d) in COMMANDS {
            lines.push(Line::from(vec![Span::styled(format!("  {c:<key_w$}"), Style::new().fg(t.text)), Span::styled(d.to_string(), Style::new().fg(t.dim))]));
        }
        let skip = (*scroll).min(lines.len().saturating_sub(body.height as usize));
        f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), body);
    }

    fn draw_keys(&self, f: &mut Frame, area: Rect) {
        let t = th();
        if let Some((msg, _)) = &self.toast {
            f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::new().fg(t.warn))), area);
            return;
        }
        let keys: &[(&str, &str)] = match (&self.overlay, &self.card) {
            (Some(Overlay::Viewer(_)), _) => &[("j/k", "scroll"), ("space", "page"), ("g/G", "top/bottom"), ("y", "copy"), ("q", "close")],
            (Some(Overlay::Picker(_)), _) => &[("type", "filter"), ("↑↓", "choose"), ("enter", "open"), ("esc", "close")],
            (Some(Overlay::Help(_)), _) => &[("j/k", "scroll"), ("any key", "close")],
            (_, Some(Card::Approve { .. })) => &[("1-3", "choose"), ("↑↓", "move"), ("type", "feedback"), ("enter", "confirm"), ("esc", "reject")],
            (_, Some(Card::Ask { .. })) => &[("1-9", "pick"), ("type", "answer"), ("enter", "send"), ("esc", "skip")],
            _ if self.focus == Focus::Scrollback => {
                &[("j/k", "nav"), ("e", "fold"), ("E", "all"), ("enter", "view"), ("y", "copy"), ("H/L", "turn"), ("tab", "prompt")]
            }
            _ if self.shell_mode => &[("enter", "run in the sandbox"), ("esc", "leave shell mode")],
            _ if self.turn.is_some() => &[("enter", "queue"), ("ctrl+enter", "send now"), ("ctrl+c", "stop"), ("tab", "transcript")],
            _ => &[("enter", "send"), ("shift+enter", "newline"), ("/", "commands"), ("@", "files"), ("!", "shell"), ("tab", "transcript"), ("?", "keys")],
        };
        let mut spans = vec![Span::raw(" ")];
        hints(&mut spans, keys);
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        if self.scroll > 0 && self.overlay.is_none() {
            let right = Line::from(vec![
                Span::styled(format!("↑ {}  ", super::n_lines(self.scroll)), Style::new().fg(t.muted)),
                Span::styled("end", Style::new().fg(t.text)),
                Span::styled(" follows ", Style::new().fg(t.dim)),
            ]);
            f.render_widget(Paragraph::new(right.right_aligned()), area);
        } else if self.think_open {
            f.render_widget(Paragraph::new(Line::from(Span::styled("thinking shown ", Style::new().fg(t.muted))).right_aligned()), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_rows_wraps_and_places_the_cursor() {
        let (rows, pos) = input_rows("abcdef\ngh", 3, 4);
        assert_eq!(rows, ["abcd", "ef", "gh"]);
        assert_eq!(pos, (0, 3));
        let (rows, pos) = input_rows("abcd", 4, 4);
        assert_eq!(rows, ["abcd", ""]);
        assert_eq!(pos, (1, 0));
        assert_eq!(input_rows("ab\n", 3, 4).1, (1, 0));
    }
}
