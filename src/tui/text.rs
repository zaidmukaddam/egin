//! Text layout for the terminal UI: word and character wrapping of styled spans, and markdown to lines.

use super::theme::th;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Cut `s` to at most `w` columns, ending in `…` when cut.
pub fn trunc(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut col = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if col + cw + 1 > w {
            break;
        }
        out.push(c);
        col += cw;
    }
    out + "…"
}

/// Pad a line with spaces in `style` out to `w` columns (for full-width bands).
pub fn pad(mut line: Line<'static>, w: usize, style: Style) -> Line<'static> {
    let have = width(&line.spans);
    if have < w {
        line.spans.push(Span::styled(" ".repeat(w - have), style));
    }
    line.style = style;
    line
}

/// Words with their trailing spaces, and "\n" on its own.
fn pieces(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut in_space = false;
    for (i, c) in s.char_indices() {
        if c == '\n' {
            if start < i {
                out.push(&s[start..i]);
            }
            out.push("\n");
            start = i + 1;
            in_space = false;
        } else if c.is_whitespace() {
            in_space = true;
        } else if in_space {
            out.push(&s[start..i]);
            start = i;
            in_space = false;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Greedy word wrap of styled spans to `w` columns. The first line starts with `first`, later ones with
/// `rest`; a word longer than a line is split.
pub fn wrap(first: &[Span<'static>], rest: &[Span<'static>], body: &[Span<'static>], w: usize) -> Vec<Line<'static>> {
    let w = w.max(4);
    let mut out = vec![];
    let mut cur: Vec<Span<'static>> = first.to_vec();
    let mut col = width(first);
    let mut fresh = true;
    let br = |cur: &mut Vec<Span<'static>>, col: &mut usize, fresh: &mut bool, out: &mut Vec<Line<'static>>| {
        out.push(Line::from(std::mem::replace(cur, rest.to_vec())));
        *col = width(rest);
        *fresh = true;
    };
    for span in body {
        for p in pieces(&span.content) {
            if p == "\n" {
                br(&mut cur, &mut col, &mut fresh, &mut out);
                continue;
            }
            if fresh && p.trim().is_empty() {
                continue;
            }
            if !fresh && col + p.trim_end().width() > w {
                br(&mut cur, &mut col, &mut fresh, &mut out);
                if p.trim().is_empty() {
                    continue;
                }
            }
            let mut p = p.to_string();
            while col + p.trim_end().width() > w {
                // Longer than a whole line: take what fits.
                let mut take = String::new();
                let mut c = col;
                for ch in p.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if c + cw > w && !take.is_empty() {
                        break;
                    }
                    take.push(ch);
                    c += cw;
                }
                p = p[take.len()..].to_string();
                cur.push(Span::styled(take, span.style));
                br(&mut cur, &mut col, &mut fresh, &mut out);
            }
            // Trailing spaces that would pass the edge are dropped, so no line is wider than `w`.
            if col + p.width() > w {
                p.truncate(p.trim_end().len());
            }
            if !p.is_empty() {
                col += p.width();
                cur.push(Span::styled(p, span.style));
                fresh = false;
            }
        }
    }
    out.push(Line::from(cur));
    out
}

/// Wrap one unstyled text by characters (code, tool output), keeping indentation.
pub fn wrap_chars(text: &str, w: usize, style: Style) -> Vec<Line<'static>> {
    let w = w.max(4);
    let mut out = vec![];
    for raw in text.split('\n') {
        let raw = raw.replace('\t', "    ");
        let mut cur = String::new();
        let mut col = 0;
        for c in raw.chars() {
            let cw = c.width().unwrap_or(0);
            if col + cw > w {
                out.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
                col = 0;
            }
            cur.push(c);
            col += cw;
        }
        out.push(Line::from(Span::styled(cur, style)));
    }
    out
}

struct List {
    next: Option<u64>,
}

/// Markdown to styled lines, wrapped to `w` columns.
pub fn markdown(src: &str, w: usize) -> Vec<Line<'static>> {
    let t = th();
    let body = Style::new().fg(t.text);
    let mut md =
        Md { w, out: vec![], spans: vec![], styles: vec![body], lists: vec![], marker: None, quote: 0, gap: false, code: None, table: None, link: None };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(src, opts) {
        md.event(ev);
    }
    md.flush();
    if let Some((_, code)) = md.code.take() {
        md.code_block(&code, "");
    }
    md.out
}

struct Md {
    w: usize,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<List>,
    marker: Option<String>,
    quote: usize,
    gap: bool,
    code: Option<(String, String)>,
    table: Option<Vec<Vec<String>>>,
    link: Option<String>,
}

impl Md {
    fn style(&self) -> Style {
        *self.styles.last().unwrap()
    }

    fn push_style(&mut self, f: impl Fn(Style) -> Style) {
        let s = f(self.style());
        self.styles.push(s);
    }

    fn quote_prefix(&self) -> Vec<Span<'static>> {
        (0..self.quote).map(|_| Span::styled("│ ", Style::new().fg(th().faint))).collect()
    }

    fn block_start(&mut self) {
        if self.gap && !self.out.is_empty() {
            let q = self.quote_prefix();
            self.out.push(Line::from(q));
        }
        self.gap = false;
    }

    /// Prefixes for the paragraph being flushed: quote bars, list indentation and the item's marker.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut first = self.quote_prefix();
        let mut rest = first.clone();
        let depth = self.lists.len();
        if depth > 0 {
            let indent = "  ".repeat(depth - 1);
            let marker = self.marker.take();
            let mw = marker.as_deref().map_or(2, |m| m.width());
            first.push(Span::raw(indent.clone()));
            rest.push(Span::raw(indent + &" ".repeat(mw)));
            match marker {
                Some(m) => first.push(Span::styled(m, Style::new().fg(th().muted))),
                None => first.push(Span::raw(" ".repeat(mw))),
            }
        }
        (first, rest)
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        self.block_start();
        let (first, rest) = self.prefixes();
        let spans = std::mem::take(&mut self.spans);
        let lines = wrap(&first, &rest, &spans, self.w);
        self.out.extend(lines);
    }

    fn code_block(&mut self, code: &str, lang: &str) {
        let t = th();
        self.block_start();
        let band = Style::new().bg(t.code);
        let q = self.quote_prefix();
        let qw = width(&q);
        let inner = self.w.saturating_sub(qw + 4);
        let label = if lang.is_empty() { String::new() } else { format!("{lang} ") };
        let mut head = q.clone();
        head.push(Span::styled(format!("{:>w$}", label, w = self.w.saturating_sub(qw)), band.fg(t.dim)));
        self.out.push(Line::from(head));
        for line in wrap_chars(code.trim_end_matches('\n'), inner, band.fg(t.code_fg)) {
            let mut l = q.clone();
            l.push(Span::styled("  ", band));
            l.extend(line.spans);
            let have = width(&l);
            l.push(Span::styled(" ".repeat(self.w.saturating_sub(have)), band));
            self.out.push(Line::from(l));
        }
        let mut foot = q;
        foot.push(Span::styled(" ".repeat(self.w.saturating_sub(qw)), band));
        self.out.push(Line::from(foot));
        self.gap = true;
    }

    fn table_block(&mut self, rows: Vec<Vec<String>>) {
        let t = th();
        self.block_start();
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; cols];
        for r in &rows {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(c.width());
            }
        }
        // Shrink the widest columns until the row fits.
        let fit = self.w.saturating_sub(2 * cols.saturating_sub(1));
        while widths.iter().sum::<usize>() > fit && widths.iter().any(|w| *w > 6) {
            let i = (0..cols).max_by_key(|i| widths[*i]).unwrap();
            widths[i] -= 1;
        }
        for (ri, r) in rows.iter().enumerate() {
            let mut spans = self.quote_prefix();
            for (i, w) in widths.iter().enumerate() {
                let cell = r.get(i).map(String::as_str).unwrap_or("");
                let style = if ri == 0 { Style::new().fg(t.fg).add_modifier(Modifier::BOLD) } else { Style::new().fg(t.text) };
                spans.push(Span::styled(format!("{:<w$}", trunc(cell, *w), w = *w), style));
                if i + 1 < cols {
                    spans.push(Span::styled("  ", style));
                }
            }
            self.out.push(Line::from(spans));
            if ri == 0 {
                let mut rule = self.quote_prefix();
                rule.push(Span::styled("─".repeat(widths.iter().sum::<usize>() + 2 * cols.saturating_sub(1)), Style::new().fg(t.faint)));
                self.out.push(Line::from(rule));
            }
        }
        self.gap = true;
    }

    fn event(&mut self, ev: Event) {
        let t = th();
        if let Some((lang, code)) = &mut self.code {
            match ev {
                Event::Text(s) => code.push_str(&s),
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, code) = (lang.clone(), code.clone());
                    self.code = None;
                    self.code_block(&code, &lang);
                }
                _ => {}
            }
            return;
        }
        if let Some(rows) = &mut self.table {
            match ev {
                Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(vec![]),
                Event::Start(Tag::TableCell) => {
                    if let Some(r) = rows.last_mut() {
                        r.push(String::new())
                    }
                }
                Event::Text(s) | Event::Code(s) => {
                    if let Some(c) = rows.last_mut().and_then(|r| r.last_mut()) {
                        c.push_str(&s)
                    }
                }
                Event::End(TagEnd::Table) => {
                    let rows = self.table.take().unwrap();
                    self.table_block(rows);
                }
                _ => {}
            }
            return;
        }
        match ev {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                self.flush();
                self.gap = true;
            }
            Event::Start(Tag::Heading { level, .. }) => {
                self.flush();
                let s = match level {
                    HeadingLevel::H1 | HeadingLevel::H2 => Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
                    HeadingLevel::H3 => Style::new().fg(t.text).add_modifier(Modifier::BOLD),
                    _ => Style::new().fg(t.muted).add_modifier(Modifier::BOLD),
                };
                self.styles.push(s);
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.styles.pop();
                self.gap = true;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.flush();
                self.quote += 1;
                self.push_style(|s| s.fg(t.muted).add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush();
                self.quote -= 1;
                self.styles.pop();
                self.gap = true;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.split(|c: char| c == ',' || c.is_whitespace()).next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Event::Start(Tag::List(start)) => {
                self.flush();
                if self.lists.is_empty() {
                    self.block_start();
                }
                self.lists.push(List { next: start });
            }
            Event::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.gap = true;
                }
            }
            Event::Start(Tag::Item) => {
                self.flush();
                self.gap = false;
                let l = self.lists.last_mut().unwrap();
                self.marker = Some(match &mut l.next {
                    Some(n) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    None => "• ".into(),
                });
            }
            Event::End(TagEnd::Item) => {
                self.flush();
                self.gap = false;
            }
            Event::TaskListMarker(done) => self.marker = Some(if done { "✔ ".into() } else { "☐ ".into() }),
            Event::Start(Tag::Emphasis) => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Event::Start(Tag::Strong) => self.push_style(|s| s.fg(t.fg).add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Strikethrough) => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.link = Some(dest_url.to_string());
                self.push_style(|s| s.fg(t.fg).add_modifier(Modifier::UNDERLINED));
            }
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                self.styles.pop();
            }
            Event::End(TagEnd::Link) => {
                self.styles.pop();
                if let Some(url) = self.link.take() {
                    let shown: String = self.spans.last().map(|s| s.content.to_string()).unwrap_or_default();
                    if !url.is_empty() && shown.trim() != url && !url.starts_with('#') {
                        self.spans.push(Span::styled(format!(" ({url})"), Style::new().fg(t.dim)));
                    }
                }
            }
            Event::Start(Tag::Table(_)) => {
                self.flush();
                self.table = Some(vec![]);
            }
            Event::Text(s) => self.spans.push(Span::styled(s.to_string(), self.style())),
            Event::Code(s) => self.spans.push(Span::styled(s.to_string(), Style::new().fg(t.fg).bg(t.inline))),
            Event::SoftBreak => self.spans.push(Span::styled(" ", self.style())),
            Event::HardBreak => self.spans.push(Span::raw("\n")),
            Event::Rule => {
                self.flush();
                self.block_start();
                self.out.push(Line::from(Span::styled("─".repeat(self.w.min(24)), Style::new().fg(t.faint))));
                self.gap = true;
            }
            Event::Html(s) | Event::InlineHtml(s) => self.spans.push(Span::styled(s.to_string(), Style::new().fg(t.dim))),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn wraps_words_and_splits_long_ones() {
        let body = [Span::raw("the quick brown fox jumps over"), Span::raw(" abcdefghijklmnop")];
        let got = plain(&wrap(&[Span::raw("> ")], &[Span::raw("  ")], &body, 12));
        assert_eq!(got, ["> the quick ", "  brown fox ", "  jumps over", "  abcdefghij", "  klmnop"]);
        assert!(got.iter().all(|l| l.width() <= 12));
    }

    #[test]
    fn markdown_lists_code_and_gaps() {
        let got = plain(&markdown("Hi **there**\n\n- one\n- two\n\n```rs\nfn x() {}\n```\nend", 30));
        assert_eq!(got[0], "Hi there");
        assert_eq!(got[1], "");
        assert_eq!(got[2], "• one");
        assert_eq!(got[3], "• two");
        assert!(got.iter().any(|l| l.contains("fn x() {}")));
        assert_eq!(got.last().unwrap(), "end");
    }
}
