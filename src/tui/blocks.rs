//! Transcript blocks: what each item shows in each fold mode, the verb groups that fold a run of quiet tool calls into
//! one row ("Read 3 files, Searched 2 patterns"), and the layout that turns items into selectable units.

use super::{
    input::GROUP,
    k, secs,
    text::{markdown, pad, trunc, width, wrap, wrap_chars},
    theme::{mix, th, LIME},
    Item, Kind, Mode, Ui,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use std::{f64::consts::PI, time::Duration};
use unicode_width::UnicodeWidthStr;

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What a tool call is doing, in the present and the past tense.
pub fn verbs(name: &str) -> (&'static str, &'static str) {
    match name {
        "bash" => ("Running", "Ran"),
        "read_file" => ("Reading", "Read"),
        "write_file" => ("Writing", "Wrote"),
        "edit_file" => ("Editing", "Edited"),
        "search_files" => ("Searching", "Searched"),
        "web_search" => ("Searching the web", "Searched the web"),
        "web_fetch" | "web_extract" => ("Fetching", "Fetched"),
        "code_execution" => ("Running code", "Ran code"),
        "plan" => ("Planning", "Planned"),
        "memory" => ("Remembering", "Remembered"),
        "skill" => ("Loading skill", "Loaded skill"),
        "task" | "helper" => ("Delegating", "Delegated"),
        "ask_user" => ("Asking", "Asked"),
        _ => ("Calling", "Called"),
    }
}

/// The argument that names what a call acts on: the command, path, URL or query.
pub fn target(args: &Value) -> Option<String> {
    ["command", "path", "url", "query", "pattern", "name", "question", "prompt", "description", "task"]
        .iter()
        .find_map(|k| args[*k].as_str().filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// That argument on one line, beside the verb.
pub fn detail(name: &str, args: &Value) -> String {
    let main = match target(args) {
        Some(s) => s.lines().next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" "),
        None if args.as_object().is_some_and(|o| !o.is_empty()) => args.to_string(),
        None => String::new(),
    };
    let main = if name == "bash" { format!("$ {main}") } else { main };
    match verbs(name).1 {
        "Called" => format!("{name} {main}"),
        _ => main,
    }
}

/// How a verb group counts a tool: (done, doing, one, many). Commands and edits change things, so they never fold.
fn noun(name: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    match name {
        "read_file" => Some(("Read", "Reading", "file", "files")),
        "search_files" => Some(("Searched", "Searching", "pattern", "patterns")),
        "web_search" => Some(("Searched", "Searching", "web query", "web queries")),
        "web_fetch" | "web_extract" => Some(("Fetched", "Fetching", "page", "pages")),
        "skill" => Some(("Loaded", "Loading", "skill", "skills")),
        _ => None,
    }
}

/// The full text a block holds, for the viewer and for copying: (title, text, is markdown).
pub fn full_text(it: &Item) -> (String, String, bool) {
    match &it.kind {
        Kind::User(t) => ("Your message".into(), t.clone(), false),
        Kind::Queued(t) => ("Queued message".into(), t.clone(), false),
        Kind::Assistant { text, .. } => ("Answer".into(), text.clone(), true),
        Kind::Thinking { text, .. } => ("Thinking".into(), text.clone(), true),
        Kind::Tool { name, args, out, .. } => {
            let body = match name.as_str() {
                "edit_file" => {
                    format!("{}\n\n{}", args["path"].as_str().unwrap_or(""), diff_text(args["old"].as_str().unwrap_or(""), args["new"].as_str().unwrap_or("")))
                }
                "write_file" => args["content"].as_str().unwrap_or("").to_string(),
                _ => out.clone().unwrap_or_default(),
            };
            (format!("{} {}", verbs(name).1, detail(name, args)), body, false)
        }
        Kind::Shell { cmd, out, .. } => (format!("! {cmd}"), out.clone().unwrap_or_default(), false),
        Kind::Note(s) | Kind::Error(s) => ("Note".into(), s.clone(), false),
        _ => (String::new(), String::new(), false),
    }
}

fn diff_text(old: &str, new: &str) -> String {
    old.lines().map(|l| format!("- {l}")).chain(new.lines().map(|l| format!("+ {l}"))).collect::<Vec<_>>().join("\n")
}

/// The laid-out transcript: lines, and each unit's anchor with its first and last line.
pub struct Laid {
    pub lines: Vec<Line<'static>>,
    pub order: Vec<(usize, usize, usize)>,
    /// For each line, the user message whose turn it belongs to: (item, that message's first line).
    pub turn: Vec<Option<(usize, usize)>>,
}

impl Ui {
    /// The fold a block shows: the user's choice, else its kind's default.
    pub(crate) fn mode_of(&self, it: &Item) -> Mode {
        if let Some(m) = it.mode {
            return m;
        }
        match &it.kind {
            Kind::Thinking { .. } if self.think_open => Mode::Expanded,
            Kind::Thinking { took: None, .. } => Mode::Truncated,
            Kind::Thinking { .. } | Kind::Tool { .. } => Mode::Collapsed,
            _ => Mode::Expanded,
        }
    }

    /// Whether a block can fold at all: a block that shows the same folded or not cannot.
    pub(crate) fn foldable(&self, it: &Item) -> bool {
        match &it.kind {
            Kind::Tool { name, out, args, .. } => self.foldable_tool(name, out.as_deref(), args),
            Kind::Thinking { text, .. } => !text.trim().is_empty(),
            Kind::Shell { .. } | Kind::Assistant { .. } => true,
            _ => false,
        }
    }

    /// The end of the verb group starting at `i`, when two or more quiet tool calls (with any finished, folded
    /// thinking between them) run on from it.
    pub(crate) fn run_from(&self, i: usize) -> Option<usize> {
        let mut members = 0;
        let mut end = i;
        for (j, it) in self.items.iter().enumerate().skip(i) {
            match &it.kind {
                Kind::Tool { name, .. } if noun(name).is_some() && self.mode_of(it) == Mode::Collapsed => {
                    members += 1;
                    end = j + 1;
                }
                Kind::Thinking { took: Some(_), .. } if self.mode_of(it) == Mode::Collapsed && members > 0 => {}
                _ => break,
            }
        }
        (members >= 2).then_some(end)
    }

    /// "Read 3 files, Searched 2 patterns · 1 failed", in the present tense while any of a kind still runs.
    fn group_label(&self, range: std::ops::Range<usize>) -> (String, bool, usize, Duration) {
        let mut buckets: Vec<(&'static str, &'static str, &'static str, &'static str, usize, bool)> = vec![];
        let (mut failed, mut running, mut took) = (0, false, Duration::ZERO);
        for it in &self.items[range] {
            if let Kind::Tool { name, out, failed: f, took: t, .. } = &it.kind {
                let Some((done, doing, one, many)) = noun(name) else { continue };
                let live = out.is_none();
                running |= live;
                failed += usize::from(*f);
                took += t.unwrap_or_default();
                match buckets.iter_mut().find(|b| b.0 == done && b.2 == one) {
                    Some(b) => {
                        b.4 += 1;
                        b.5 |= live;
                    }
                    None => buckets.push((done, doing, one, many, 1, live)),
                }
            }
        }
        let parts: Vec<String> = buckets
            .iter()
            .map(|(done, doing, one, many, n, live)| format!("{} {n} {}", if *live { doing } else { done }, if *n == 1 { one } else { many }))
            .collect();
        let mut label = parts.join(", ");
        if failed > 0 {
            label += &format!(" · {failed} failed");
        }
        (label, running, failed, took)
    }

    fn spin(&self) -> &'static str {
        SPINNER[(self.born.elapsed().as_millis() / 80) as usize % SPINNER.len()]
    }

    /// A header row: bullet, verb, what it acts on, and the time on the right.
    fn header(&self, bullet: Span<'static>, verb: &str, verb_style: Style, what: &str, time: &str, w: usize) -> Line<'static> {
        let t = th();
        let shown = trunc(what, w.saturating_sub(verb.width() + time.width() + 5));
        let used = 2 + verb.width() + 1 + shown.width();
        Line::from(vec![
            bullet,
            Span::styled(verb.to_string(), verb_style),
            Span::raw(" "),
            Span::styled(shown, Style::new().fg(t.muted)),
            Span::raw(" ".repeat(w.saturating_sub(used + time.width()))),
            Span::styled(time.to_string(), Style::new().fg(t.dim)),
        ])
    }

    pub(crate) fn item_lines(&self, it: &Item, w: usize) -> Vec<Line<'static>> {
        let t = th();
        let dim = Style::new().fg(t.dim);
        let mode = self.mode_of(it);
        match &it.kind {
            Kind::Welcome => vec![],
            Kind::User(text) => {
                let band = Style::new().bg(t.band);
                let time = it.at.format("%H:%M").to_string();
                let body = [Span::styled(text.clone(), band.fg(t.fg))];
                let mut rows = wrap(&[Span::styled("❯ ", band.fg(t.accent))], &[Span::styled("  ", band)], &body, w.saturating_sub(time.width() + 2));
                // The time sits at the right of the first row.
                if let Some(first) = rows.first_mut() {
                    let have = width(&first.spans);
                    first.spans.push(Span::styled(" ".repeat(w.saturating_sub(have + time.width() + 1)), band));
                    first.spans.push(Span::styled(time, band.fg(t.dim)));
                }
                let mut lines = vec![Line::default()];
                lines.extend(rows);
                lines.push(Line::default());
                lines.into_iter().map(|l| pad(l, w, band)).collect()
            }
            Kind::Queued(text) => vec![Line::from(vec![
                Span::styled("⧗ ", dim),
                Span::styled(trunc(text.lines().next().unwrap_or(""), w.saturating_sub(24)), dim.add_modifier(Modifier::ITALIC)),
                Span::styled("  queued for the next step", Style::new().fg(t.faint)),
            ])],
            Kind::Assistant { text, live } => {
                let mut lines = markdown(text, w);
                if *live {
                    let on = (self.born.elapsed().as_millis() / 500).is_multiple_of(2);
                    match lines.last_mut() {
                        Some(l) => l.spans.push(Span::styled(if on { "▍" } else { " " }, Style::new().fg(t.accent))),
                        None => lines.push(Line::from(Span::styled("▍", Style::new().fg(t.accent)))),
                    }
                }
                lines
            }
            Kind::Thinking { text, started, took } => {
                let head = match took {
                    None => self.header(
                        Span::styled("◆ ", Style::new().fg(t.accent)),
                        "Thinking…",
                        Style::new().fg(t.text).add_modifier(Modifier::ITALIC),
                        "",
                        &secs(started.elapsed()),
                        w,
                    ),
                    Some(d) if d.is_zero() => self.header(Span::styled("◇ ", dim), "Thought", dim, "", "", w),
                    Some(d) => self.header(Span::styled("◇ ", dim), "Thought", dim, &format!("for {}", secs(*d)), "", w),
                };
                let mut lines = vec![head];
                if mode != Mode::Collapsed {
                    // Thinking is markdown too, shown a step quieter than the answer.
                    let body: Vec<Line<'static>> = markdown(text.trim(), w.saturating_sub(2))
                        .into_iter()
                        .map(|l| {
                            let spans = l
                                .spans
                                .into_iter()
                                .map(|s| Span::styled(s.content, Style::new().fg(t.dim).bg(s.style.bg.unwrap_or(t.bg)).add_modifier(Modifier::ITALIC)));
                            Line::from([vec![Span::raw("  ")], spans.collect()].concat())
                        })
                        .collect();
                    let keep = if mode == Mode::Expanded { body.len() } else { 3 };
                    let skip = body.len().saturating_sub(keep);
                    if skip > 0 {
                        lines.push(Line::from(Span::styled(format!("  … {skip} earlier lines"), Style::new().fg(t.faint))));
                    }
                    lines.extend(body.into_iter().skip(skip));
                }
                lines
            }
            Kind::Tool { name, args, out, failed, started, took, .. } => self.tool_lines(name, args, out.as_deref(), *failed, *started, *took, mode, w),
            Kind::Shell { cmd, out, ok, started, took, .. } => {
                let (bullet, time) = match (out, took) {
                    (None, _) => (Span::styled(format!("{} ", self.spin()), Style::new().fg(t.warn)), secs(started.elapsed())),
                    (Some(_), Some(d)) => (Span::styled("! ", Style::new().fg(if *ok { t.warn } else { t.err })), secs(*d)),
                    _ => (Span::styled("! ", Style::new().fg(t.warn)), String::new()),
                };
                let mut lines = vec![self.header(bullet, cmd, Style::new().fg(t.fg), "", &time, w)];
                if let Some(o) = out.as_deref().map(str::trim_end).filter(|o| !o.is_empty()) {
                    let all: Vec<&str> = o.lines().collect();
                    let keep = if mode == Mode::Collapsed {
                        0
                    } else if mode == Mode::Truncated {
                        6
                    } else {
                        800
                    };
                    for l in all.iter().take(keep) {
                        for row in wrap_chars(l, w.saturating_sub(4), Style::new().fg(if *ok { t.text } else { t.err })) {
                            let mut spans = vec![Span::styled("  │ ", Style::new().fg(t.faint))];
                            spans.extend(row.spans);
                            lines.push(Line::from(spans));
                        }
                    }
                    if all.len() > keep && keep > 0 {
                        lines.push(Line::from(Span::styled(format!("  │ … +{} lines (enter to view)", all.len() - keep), Style::new().fg(t.faint))));
                    }
                }
                lines
            }
            Kind::Note(s) => wrap(&[Span::styled("· ", Style::new().fg(t.faint))], &[Span::raw("  ")], &[Span::styled(s.clone(), dim)], w),
            Kind::Error(s) => wrap(&[Span::styled("✗ ", Style::new().fg(t.err))], &[Span::raw("  ")], &[Span::styled(s.clone(), Style::new().fg(t.err))], w),
            Kind::Interrupted => vec![Line::from(vec![
                Span::styled("■ ", Style::new().fg(t.warn)),
                Span::styled("Interrupted", Style::new().fg(t.warn)),
                Span::styled(" · tell egin what to do instead", dim),
            ])],
            Kind::Worked { took, tools, out_tok, cost } => {
                let mut tail = vec![];
                if *tools > 0 {
                    tail.push(format!("{tools} tool call{}", if *tools == 1 { "" } else { "s" }));
                }
                if *out_tok > 0 {
                    tail.push(format!("⇣{}", k(*out_tok)));
                }
                if *cost > 0.0 {
                    tail.push(format!("${cost:.4}"));
                }
                let mut spans = vec![Span::styled(format!("Worked for {}", secs(*took)), dim)];
                if !tail.is_empty() {
                    spans.push(Span::styled(format!(" · {}", tail.join(" · ")), Style::new().fg(t.faint)));
                }
                vec![Line::from(spans)]
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_lines(
        &self,
        name: &str,
        args: &Value,
        out: Option<&str>,
        failed: bool,
        started: std::time::Instant,
        took: Option<Duration>,
        mode: Mode,
        w: usize,
    ) -> Vec<Line<'static>> {
        let t = th();
        let (doing, did) = verbs(name);
        let running = out.is_none();
        let bullet = match (running, failed) {
            (true, _) => Span::styled(format!("{} ", self.spin()), Style::new().fg(t.accent)),
            (_, true) => Span::styled("▸ ", Style::new().fg(t.err)),
            _ => Span::styled(if mode == Mode::Collapsed || !self.foldable_tool(name, out, args) { "▸ " } else { "▾ " }, Style::new().fg(t.dim)),
        };
        let verb_style = match (running, failed) {
            (true, _) => Style::new().fg(t.fg),
            (_, true) => Style::new().fg(t.err),
            _ => Style::new().fg(t.text),
        };
        let time = match took {
            Some(d) if d >= Duration::from_millis(100) => secs(d),
            Some(_) => String::new(),
            None => secs(started.elapsed()),
        };
        let mut lines = vec![self.header(bullet, if running { doing } else { did }, verb_style, &detail(name, args), &time, w)];
        let gutter = || Span::styled("  │ ", Style::new().fg(t.faint));
        let out = out.map(str::trim_end).unwrap_or("");
        if mode == Mode::Collapsed {
            // A failure says why even folded.
            if failed {
                if let Some(first) = out.lines().find(|l| !l.trim().is_empty()) {
                    lines.push(Line::from(vec![
                        gutter(),
                        Span::styled(trunc(first.trim_start_matches("error: "), w.saturating_sub(4)), Style::new().fg(t.err)),
                    ]));
                }
            }
            return lines;
        }
        // The rows to show: an edit as its diff, a new file as its text, anything else as its output.
        let rows: Vec<(String, Style)> = match name {
            "edit_file" if !failed => {
                let (old, new) = (args["old"].as_str().unwrap_or(""), args["new"].as_str().unwrap_or(""));
                old.lines()
                    .map(|l| (format!("- {l}"), Style::new().fg(t.del_fg).bg(t.del_bg)))
                    .chain(new.lines().map(|l| (format!("+ {l}"), Style::new().fg(t.add_fg).bg(t.add_bg))))
                    .collect()
            }
            "write_file" if !failed => {
                args["content"].as_str().unwrap_or("").lines().enumerate().map(|(i, l)| (format!("{:>4}  {l}", i + 1), Style::new().fg(t.text))).collect()
            }
            _ => out.lines().map(|l| (l.to_string(), Style::new().fg(if failed { t.err } else { t.dim }))).collect(),
        };
        let n = rows.len();
        let (head, tail) = match mode {
            Mode::Truncated if n > 5 => (2, 3),
            _ => (n.min(600), 0),
        };
        let push = |(l, style): &(String, Style), lines: &mut Vec<Line<'static>>| {
            let diff = style.bg.is_some();
            for row in wrap_chars(l, w.saturating_sub(4), *style) {
                let mut spans = vec![gutter()];
                spans.extend(row.spans);
                let mut line = Line::from(spans);
                if diff {
                    line = pad(line, w, Style::new());
                    let have = width(&line.spans);
                    if have < w {
                        line.spans.push(Span::styled(" ".repeat(w - have), Style::new().bg(style.bg.unwrap())));
                    }
                }
                lines.push(line);
            }
        };
        for r in rows.iter().take(head) {
            push(r, &mut lines);
        }
        let hidden = n.saturating_sub(head + tail);
        if hidden > 0 {
            lines.push(Line::from(vec![gutter(), Span::styled(format!("… +{hidden} lines (enter to view)"), Style::new().fg(t.faint))]));
        }
        for r in rows.iter().skip(n - tail) {
            push(r, &mut lines);
        }
        lines
    }

    fn foldable_tool(&self, name: &str, out: Option<&str>, args: &Value) -> bool {
        out.is_some_and(|o| !o.trim().is_empty()) || matches!(name, "edit_file" | "write_file") || args["command"].as_str().is_some_and(|c| c.contains('\n'))
    }

    /// Every item laid out at `w` columns as units behind a two-column gutter: a rail that pulses down what still
    /// runs, a band for the user's messages, and a lime bar beside the selected unit.
    pub(crate) fn layout(&self, w: usize) -> Laid {
        // ponytail: lays the whole transcript out again each frame; cache settled items' lines if long sessions stutter.
        let t = th();
        let phase = self.born.elapsed().as_secs_f64();
        let cw = w.saturating_sub(2);
        let mut laid = Laid { lines: vec![], order: vec![], turn: vec![] };
        let mut turn: Option<(usize, usize)> = None;
        let mut prev_tool = false;
        let mut i = 0;
        while i < self.items.len() {
            let it = &self.items[i];
            if matches!(it.kind, Kind::Welcome) {
                i += 1;
                continue;
            }
            // A run of quiet tool calls folds into one header, unless it was opened.
            let mut units: Vec<(usize, Vec<Line<'static>>, bool, bool)> = vec![];
            let mut next = i + 1;
            let tool_like;
            if let Some(end) = self.run_from(i) {
                let (label, running, failed, took) = self.group_label(i..end);
                let open = self.open_groups.contains(&i);
                let bullet = if running {
                    Span::styled(format!("{} ", self.spin()), Style::new().fg(t.accent))
                } else {
                    Span::styled(if open { "▾ " } else { "▸ " }, Style::new().fg(if failed > 0 && !running { t.err } else { t.dim }))
                };
                let time = if running || took < Duration::from_millis(100) { String::new() } else { secs(took) };
                let (verb, what) = label.split_once(' ').map_or((label.as_str(), ""), |(a, b)| (a, b));
                units.push((i | GROUP, vec![self.header(bullet, verb, Style::new().fg(if running { t.fg } else { t.text }), what, &time, cw)], running, false));
                if open {
                    for j in i..end {
                        let m = &self.items[j];
                        units.push((
                            j,
                            self.item_lines(m, cw).into_iter().map(|l| Line::from([vec![Span::raw("  ")], l.spans].concat())).collect(),
                            m.live(),
                            false,
                        ));
                    }
                }
                next = end;
                tool_like = true;
            } else {
                units.push((i, self.item_lines(it, cw), it.live(), matches!(it.kind, Kind::User(_))));
                tool_like = matches!(it.kind, Kind::Tool { .. });
            }
            if !laid.lines.is_empty() && (!prev_tool || !tool_like) {
                laid.lines.push(Line::default());
                laid.turn.push(turn);
            }
            prev_tool = tool_like;
            for (anchor, lines, live, band) in units {
                if lines.is_empty() {
                    continue;
                }
                if band {
                    turn = Some((anchor, laid.lines.len()));
                }
                let first = laid.lines.len();
                let selected = self.sel == Some(anchor);
                for line in lines {
                    let row = laid.lines.len() as f64;
                    let gutter = if selected {
                        Span::styled("▎ ", Style::new().fg(t.accent).bg(if band { t.band } else { t.hover }))
                    } else if band {
                        Span::styled("  ", Style::new().bg(t.band))
                    } else if live {
                        let b = (PI * (row / 9.0 - phase * 1.4)).sin().powi(2);
                        Span::styled("┃ ", Style::new().fg(mix(0x2e2e2e, LIME, b)))
                    } else {
                        Span::raw("  ")
                    };
                    let mut spans = vec![gutter];
                    spans.extend(line.spans);
                    let mut l = Line::from(spans);
                    if band || selected {
                        let bg = if band { t.band } else { t.hover };
                        l = pad(l, w, Style::new().bg(bg));
                        if selected && !band {
                            for s in &mut l.spans {
                                s.style = s.style.bg(s.style.bg.filter(|c| *c != t.bg).unwrap_or(bg));
                            }
                        }
                    }
                    laid.lines.push(l);
                    laid.turn.push(turn);
                }
                laid.order.push((anchor, first, laid.lines.len() - 1));
            }
            i = next;
        }
        laid
    }
}
