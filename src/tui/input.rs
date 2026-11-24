//! Keys and the mouse. The keyboard reaches, in order: an open overlay (picker, viewer, shortcuts), a card waiting
//! on the user, then the focused pane: the prompt, or the transcript after Tab.

use super::{blocks, copy, files, Card, Focus, Item, Kind, Mode, Overlay, PickKind, Ui, Viewer};
use crossterm::event::{Event as Ev, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use serde_json::json;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, &str)] = &[
    ("/new", "start a fresh session (also /clear)"),
    ("/resume", "pick an earlier session to continue"),
    ("/model", "switch model"),
    ("/always-approve", "run every tool call without asking"),
    ("/copy", "copy the last answer"),
    ("/export", "write the conversation to a Markdown file"),
    ("/context", "how much of the model's window is in use"),
    ("/usage", "tokens, cost and time for this session"),
    ("/help", "keys and commands"),
    ("/exit", "quit egin"),
];

/// Selection keys for verb-group headers, kept apart from item indexes.
pub const GROUP: usize = 1 << 62;

/// What the composer is completing.
pub enum Popup {
    Commands(Vec<(&'static str, &'static str)>),
    Files(Vec<String>),
    Models(Vec<String>),
}

impl Popup {
    pub fn len(&self) -> usize {
        match self {
            Popup::Commands(v) => v.len(),
            Popup::Files(v) | Popup::Models(v) => v.len(),
        }
    }
}

impl Ui {
    /// The `@` token the cursor sits in: (its start, the query after `@`).
    fn mention(&self) -> Option<(usize, String)> {
        let before = &self.input[..self.cursor];
        let at = before.rfind('@')?;
        let query = &before[at + 1..];
        let starts = at == 0 || before[..at].ends_with(char::is_whitespace);
        (starts && !query.contains(char::is_whitespace)).then(|| (at, query.to_string()))
    }

    pub(crate) fn popup(&mut self) -> Option<Popup> {
        if self.hide_popup || self.card.is_some() || self.overlay.is_some() || self.focus != Focus::Prompt || self.shell_mode {
            return None;
        }
        if let Some(arg) = self.input.strip_prefix("/model ") {
            let ids: Vec<String> = self.models.iter().map(|(id, _)| id.clone()).collect();
            let hits: Vec<String> = files::best(arg.trim(), &ids, 8).into_iter().cloned().collect();
            return (!hits.is_empty()).then_some(Popup::Models(hits));
        }
        if self.input.starts_with('/') && !self.input.contains(char::is_whitespace) {
            let mut hits: Vec<(i64, (&str, &str))> = COMMANDS.iter().filter_map(|c| files::fuzzy(&self.input[1..], &c.0[1..]).map(|s| (s, *c))).collect();
            hits.sort_by_key(|h| std::cmp::Reverse(h.0));
            let hits: Vec<_> = hits.into_iter().map(|(_, c)| c).collect();
            return (!hits.is_empty()).then_some(Popup::Commands(hits));
        }
        let (_, query) = self.mention()?;
        if self.files.is_none() {
            // The workspace the agent's files land in, or the agent folder while that is still empty.
            let mut found = files::index(&self.agent.workspace);
            found.retain(|f| !f.ends_with(".gitkeep"));
            if found.is_empty() {
                found = files::index(&self.agent.dir);
            }
            self.files = Some(found);
        }
        let hits: Vec<String> = files::best(&query, self.files.as_ref().unwrap(), 8).into_iter().cloned().collect();
        (!hits.is_empty()).then_some(Popup::Files(hits))
    }

    pub(crate) fn on_input(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Paste(s) => {
                let s = s.replace("\r\n", "\n").replace('\r', "\n");
                self.paste(&s);
                true
            }
            Ev::Mouse(m) => {
                match m.kind {
                    MouseEventKind::ScrollUp => match &mut self.overlay {
                        Some(Overlay::Viewer(v)) => v.scroll = v.scroll.saturating_sub(3),
                        _ => self.scroll += 3,
                    },
                    MouseEventKind::ScrollDown => match &mut self.overlay {
                        Some(Overlay::Viewer(v)) => v.scroll += 3,
                        _ => self.scroll = self.scroll.saturating_sub(3),
                    },
                    MouseEventKind::Down(MouseButton::Left) => return self.click(m.column, m.row),
                    _ => {}
                }
                true
            }
            Ev::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            _ => true,
        }
    }

    fn paste(&mut self, s: &str) {
        match (&mut self.overlay, &mut self.card) {
            (Some(Overlay::Picker(p)), _) => p.filter.push_str(s.lines().next().unwrap_or("")),
            (_, Some(Card::Approve { feedback, pick, .. })) => {
                *pick = 2;
                feedback.push_str(s);
            }
            (_, Some(Card::Ask { other, pick, options, .. })) => {
                *pick = options.len();
                other.push_str(s);
            }
            _ => {
                self.focus = Focus::Prompt;
                let lines = s.lines().count();
                // A long paste shows as a chip and goes out whole on send.
                if s.len() > 1200 || lines > 15 {
                    let tag = format!("[Pasted #{}: {lines} lines]", self.pastes.len() + 1);
                    self.pastes.push((tag.clone(), s.to_string()));
                    self.insert(&tag);
                } else {
                    self.insert(s);
                }
            }
        }
    }

    fn click(&mut self, x: u16, y: u16) -> bool {
        let inside = |r: &ratatui::layout::Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        if self.overlay.is_some() {
            return true;
        }
        if self.hits.stop.is_some_and(|r| inside(&r)) {
            self.interrupt();
            return true;
        }
        if let Some((_, action)) = self.hits.menu.iter().find(|(r, _)| inside(r)).copied() {
            return self.menu(action);
        }
        if self.hits.composer.is_some_and(|r| inside(&r)) {
            self.focus = Focus::Prompt;
            return true;
        }
        if inside(&self.hits.body) {
            if let Some(&(_, _, key)) = self.hits.units.iter().find(|(a, b, _)| y >= *a && y <= *b) {
                if self.sel == Some(key) {
                    self.toggle(key);
                } else {
                    self.sel = Some(key);
                }
                self.focus = Focus::Scrollback;
            }
        }
        true
    }

    /// A welcome-menu row.
    pub(crate) fn menu(&mut self, action: &str) -> bool {
        match action {
            "help" => self.overlay = Some(Overlay::Help(0)),
            "resume" => self.open_resume(),
            "model" => self.open_models(),
            "quit" => return false,
            _ => {}
        }
        true
    }

    fn on_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.overlay.is_some() {
            return self.overlay_key(&k);
        }
        if self.card.is_some() && self.card_key(&k) {
            return true;
        }
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.turn.is_some() {
                    self.interrupt();
                    self.toast("cancelling the turn");
                } else if self.focus == Focus::Scrollback {
                    self.focus = Focus::Prompt;
                    self.sel = None;
                } else if !self.input.is_empty() || self.shell_mode {
                    self.set_input(String::new());
                    self.shell_mode = false;
                } else if self.quit_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1800)) {
                    return false;
                } else {
                    self.quit_at = Some(Instant::now());
                    self.toast("press ctrl+c again to quit");
                }
                return true;
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return false,
            KeyCode::Char('n') if ctrl => {
                self.new_session();
                return true;
            }
            KeyCode::Char('r') if ctrl => {
                self.open_resume();
                return true;
            }
            KeyCode::Char('o') if ctrl => return self.command("/always-approve", ""),
            KeyCode::BackTab => return self.command("/always-approve", ""),
            KeyCode::Char('e') if ctrl && (self.focus == Focus::Scrollback || self.input.is_empty()) => {
                self.think_open = !self.think_open;
                self.toast(if self.think_open { "showing all thinking" } else { "thinking folded" });
                return true;
            }
            KeyCode::Char('s') if ctrl || k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Char('s') => {
                self.stash_toggle();
                return true;
            }
            _ => {}
        }
        match self.focus {
            Focus::Scrollback => self.scrollback_key(&k),
            Focus::Prompt => self.prompt_key(&k),
        }
    }

    fn stash_toggle(&mut self) {
        match (self.input.trim().is_empty(), self.stash.take()) {
            (false, old) => {
                self.stash = Some(std::mem::take(&mut self.input));
                self.cursor = 0;
                self.toast(if old.is_some() {
                    "draft stashed (it replaced the last one) · ctrl+s brings it back"
                } else {
                    "draft stashed · ctrl+s brings it back"
                });
            }
            (true, Some(s)) => {
                self.set_input(s);
                self.toast("draft restored");
            }
            (true, None) => self.toast("nothing stashed"),
        }
    }

    // ---- overlays ----

    fn overlay_key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.overlay.as_mut().unwrap() {
            Overlay::Picker(p) => {
                let n = p.shown().len();
                match k.code {
                    KeyCode::Esc => self.overlay = None,
                    KeyCode::Char('c') if ctrl => self.overlay = None,
                    KeyCode::Up => p.pick = p.pick.saturating_sub(1),
                    KeyCode::Down => p.pick = (p.pick + 1).min(n.saturating_sub(1)),
                    KeyCode::Backspace => {
                        p.filter.pop();
                        p.pick = 0;
                    }
                    KeyCode::Enter => {
                        let chosen = p.shown().get(p.pick).map(|r| r.2.clone());
                        let kind = p.kind;
                        self.overlay = None;
                        match (kind, chosen) {
                            (PickKind::Resume, Some(id)) => self.resume(&id),
                            (PickKind::Model, Some(id)) => self.set_model(&id),
                            _ => {}
                        }
                    }
                    KeyCode::Char(c) if !ctrl => {
                        p.filter.push(c);
                        p.pick = 0;
                    }
                    _ => {}
                }
            }
            Overlay::Viewer(v) => {
                let page = self.hits.body.height.saturating_sub(3) as usize;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                    KeyCode::Char('c') if ctrl => self.overlay = None,
                    KeyCode::Down | KeyCode::Char('j') => v.scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => v.scroll = v.scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => v.scroll += page,
                    KeyCode::Char('d') if ctrl => v.scroll += page / 2,
                    KeyCode::PageUp => v.scroll = v.scroll.saturating_sub(page),
                    KeyCode::Char('u') if ctrl => v.scroll = v.scroll.saturating_sub(page / 2),
                    KeyCode::Char('g') | KeyCode::Home => v.scroll = 0,
                    KeyCode::Char('G') | KeyCode::End => v.scroll = usize::MAX / 2,
                    KeyCode::Char('y') => {
                        let ok = copy(&v.text);
                        self.toast(if ok { "copied" } else { "couldn't reach the clipboard" });
                    }
                    _ => {}
                }
            }
            Overlay::Help(scroll) => match k.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                _ => self.overlay = None,
            },
        }
        true
    }

    // ---- cards ----

    /// Keys a card takes; false lets the global keys have it.
    fn card_key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(k.code, KeyCode::Char('c' | 'd' | 'r' | 'n')) {
            return false;
        }
        let Some(card) = &mut self.card else { return false };
        match card {
            Card::Approve { pick, feedback, .. } => {
                let choice = match k.code {
                    KeyCode::Up => {
                        *pick = pick.saturating_sub(1);
                        None
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        *pick = (*pick + 1).min(2);
                        None
                    }
                    KeyCode::Char(c @ '1'..='3') if feedback.is_empty() => Some(c as usize - '1' as usize),
                    KeyCode::Enter => Some(*pick),
                    KeyCode::Esc if !feedback.is_empty() => {
                        feedback.clear();
                        None
                    }
                    KeyCode::Esc => Some(2),
                    KeyCode::Backspace => {
                        feedback.pop();
                        None
                    }
                    KeyCode::Char(c) if !ctrl => {
                        *pick = 2;
                        feedback.push(c);
                        None
                    }
                    _ => None,
                };
                if let Some(c) = choice {
                    self.approve(c);
                }
            }
            Card::Ask { options, pick, other, .. } => {
                let rows = options.len() + 1;
                let answer = match k.code {
                    KeyCode::Up => {
                        *pick = pick.saturating_sub(1);
                        None
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        *pick = (*pick + 1).min(rows - 1);
                        None
                    }
                    KeyCode::Char(c @ '1'..='9') if other.is_empty() && (c as usize - '1' as usize) < options.len() => {
                        Some(options[c as usize - '1' as usize].clone())
                    }
                    KeyCode::Enter if *pick < options.len() => Some(options[*pick].clone()),
                    KeyCode::Enter => Some(std::mem::take(other)),
                    KeyCode::Esc if !other.is_empty() => {
                        other.clear();
                        None
                    }
                    KeyCode::Esc => Some(String::new()),
                    KeyCode::Backspace => {
                        other.pop();
                        None
                    }
                    KeyCode::Char(c) if !ctrl => {
                        *pick = options.len();
                        other.push(c);
                        None
                    }
                    _ => None,
                };
                if let Some(a) = answer {
                    let Some(Card::Ask { id, .. }) = self.card.take() else { return true };
                    self.answer(&id, json!(a));
                    self.phase("Thinking");
                }
            }
        }
        true
    }

    /// 0 allows the call, 1 allows the tool for the rest of the session, 2 rejects it (with any feedback typed, which
    /// reaches the agent as a message at its next step).
    fn approve(&mut self, choice: usize) {
        let Some(Card::Approve { id, tool, feedback, .. }) = self.card.take() else { return };
        if choice == 1 {
            self.trusted.insert(tool.clone());
            self.toast(format!("{tool} runs without asking for the rest of this session"));
        }
        self.answer(&id, json!(choice < 2));
        if choice == 2 && !feedback.trim().is_empty() {
            let _ = egin::agent::run::enqueue(&self.app, &self.sid, &format!("I rejected that {tool} call: {}", feedback.trim()), &[], vec![]);
        }
        self.phase("Thinking");
    }

    // ---- the transcript ----

    /// Fold or unfold one unit: a verb group opens to its calls, a block cycles between its header and all of it.
    pub(crate) fn toggle(&mut self, key: usize) {
        if key & GROUP != 0 {
            let i = key & !GROUP;
            if !self.open_groups.remove(&i) {
                self.open_groups.insert(i);
            }
            return;
        }
        let Some(it) = self.items.get(key) else { return };
        if !self.foldable(it) {
            return;
        }
        let next = if self.mode_of(it) == Mode::Collapsed { Mode::Expanded } else { Mode::Collapsed };
        self.items[key].mode = Some(next);
    }

    fn set_fold(&mut self, key: usize, open: bool) {
        if key & GROUP != 0 {
            let i = key & !GROUP;
            if open {
                self.open_groups.insert(i);
            } else {
                self.open_groups.remove(&i);
            }
        } else if self.items.get(key).is_some_and(|it| self.foldable(it)) {
            self.items[key].mode = Some(if open { Mode::Expanded } else { Mode::Collapsed });
        }
    }

    fn select(&mut self, key: Option<usize>) {
        self.sel = key;
        let Some(key) = key else { return };
        let Some(&(_, first, last)) = self.hits.order.iter().find(|(a, _, _)| *a == key) else { return };
        let h = self.hits.body.height as usize;
        let max = self.last_total.saturating_sub(h);
        let top = max.saturating_sub(self.scroll);
        if first < top {
            self.scroll = max.saturating_sub(first);
        } else if last >= top + h {
            self.scroll = max.saturating_sub((last + 1).saturating_sub(h).min(first));
        }
    }

    fn unit_item(&self, key: usize) -> Option<&Item> {
        (key & GROUP == 0).then(|| self.items.get(key)).flatten()
    }

    fn scrollback_key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let order: Vec<usize> = self.hits.order.iter().map(|(a, _, _)| *a).collect();
        let at = self.sel.and_then(|s| order.iter().position(|a| *a == s));
        let half = (self.hits.body.height / 2).max(1) as usize;
        match k.code {
            KeyCode::Tab | KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char(' ') if !ctrl => {
                self.focus = Focus::Prompt;
                self.sel = None;
            }
            KeyCode::Char('j') | KeyCode::Down if !ctrl => {
                self.select(order.get(at.map_or(order.len().saturating_sub(1), |i| (i + 1).min(order.len() - 1))).copied())
            }
            KeyCode::Char('k') | KeyCode::Up if !ctrl => self.select(order.get(at.map_or(order.len().saturating_sub(1), |i| i.saturating_sub(1))).copied()),
            KeyCode::Char('g') | KeyCode::Home => self.select(order.first().copied()),
            KeyCode::Char('G') | KeyCode::End => {
                self.select(order.last().copied());
                self.scroll = 0;
            }
            KeyCode::Char('L') | KeyCode::Char('H') => {
                // The next or previous message of yours.
                let users: Vec<usize> = order.iter().copied().filter(|a| self.unit_item(*a).is_some_and(|it| matches!(it.kind, Kind::User(_)))).collect();
                let cur = self.sel.unwrap_or(0) & !GROUP;
                let pick = if k.code == KeyCode::Char('L') { users.iter().find(|u| **u > cur) } else { users.iter().rev().find(|u| **u < cur) };
                if let Some(u) = pick.copied() {
                    self.select(Some(u));
                }
            }
            KeyCode::Char('e') | KeyCode::Char('o') if !ctrl => {
                if let Some(s) = self.sel {
                    self.toggle(s);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(s) = self.sel {
                    self.set_fold(s, true);
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(s) = self.sel {
                    self.set_fold(s, false);
                }
            }
            KeyCode::Char('E') => {
                let opened = self.items.iter().any(|it| it.mode == Some(Mode::Expanded)) || !self.open_groups.is_empty();
                for i in 0..self.items.len() {
                    if self.foldable(&self.items[i]) && !matches!(self.items[i].kind, Kind::Assistant { .. } | Kind::User(_)) {
                        self.items[i].mode = if opened { None } else { Some(Mode::Expanded) };
                    }
                }
                self.open_groups.clear();
                self.toast(if opened { "folded everything" } else { "unfolded everything" });
            }
            KeyCode::Enter => self.view_selected(),
            KeyCode::Char('f') if ctrl => self.view_selected(),
            KeyCode::Char('y') => {
                let text = self.sel.and_then(|s| self.unit_item(s)).map(|it| blocks::full_text(it).1).unwrap_or_default();
                if !text.is_empty() {
                    let lines = text.lines().count();
                    let ok = copy(&text);
                    self.toast(if ok { format!("copied {lines} line{}", if lines == 1 { "" } else { "s" }) } else { "couldn't reach the clipboard".into() });
                }
            }
            KeyCode::Char('Y') => {
                let what = self.sel.and_then(|s| self.unit_item(s)).and_then(|it| match &it.kind {
                    Kind::Tool { args, .. } => blocks::target(args),
                    Kind::Shell { cmd, .. } => Some(cmd.clone()),
                    _ => None,
                });
                if let Some(w) = what {
                    let ok = copy(&w);
                    self.toast(if ok { format!("copied {}", super::text::trunc(&w, 40)) } else { "couldn't reach the clipboard".into() });
                }
            }
            KeyCode::Char('u') if ctrl => self.scroll += half,
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::PageUp => self.scroll += half * 2,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(half * 2),
            // Anything else is typing: it goes to the prompt.
            KeyCode::Char(c) if !ctrl => {
                self.focus = Focus::Prompt;
                self.sel = None;
                self.insert(&c.to_string());
            }
            _ => {}
        }
        true
    }

    fn view_selected(&mut self) {
        let Some(s) = self.sel else { return };
        if s & GROUP != 0 {
            return self.toggle(s);
        }
        let Some(it) = self.items.get(s) else { return };
        let (title, text, markdown) = blocks::full_text(it);
        if !text.trim().is_empty() {
            self.overlay = Some(Overlay::Viewer(Viewer { title, text, markdown, scroll: 0 }));
        }
    }

    // ---- the prompt ----

    fn prompt_key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let popup = self.popup();
        let n = popup.as_ref().map_or(0, Popup::len);
        if k.code != KeyCode::Esc {
            self.esc_at = None;
        }
        match k.code {
            KeyCode::Esc => {
                if popup.is_some() {
                    self.hide_popup = true;
                } else if self.turn.is_some() {
                    self.toast("ctrl+c stops the turn");
                } else if self.input.is_empty() && self.shell_mode {
                    self.shell_mode = false;
                } else if !self.input.is_empty() {
                    if self.esc_at.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
                        self.stash = Some(std::mem::take(&mut self.input));
                        self.cursor = 0;
                        self.toast("draft cleared and stashed · ctrl+s brings it back");
                    } else {
                        self.esc_at = Some(Instant::now());
                        self.toast("esc again to clear the draft");
                    }
                }
            }
            KeyCode::Enter if k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => self.insert("\n"),
            KeyCode::Char('j') if ctrl => self.insert("\n"),
            KeyCode::Enter if self.input.ends_with('\\') && self.cursor == self.input.len() => {
                self.input.pop();
                self.cursor -= 1;
                self.insert("\n");
            }
            KeyCode::Enter if ctrl && self.turn.is_some() => {
                // Send now: stop the turn, and run this as the next one.
                self.interrupt();
                self.turn = None;
                return self.submit();
            }
            KeyCode::Enter => {
                if let Some(p) = popup {
                    return self.accept(p, true);
                }
                return self.submit();
            }
            KeyCode::Tab if popup.is_some() => {
                return self.accept(popup.unwrap(), false);
            }
            KeyCode::Tab => {
                if !self.hits.order.is_empty() {
                    self.focus = Focus::Scrollback;
                    let last = self.hits.order.last().map(|u| u.0);
                    self.select(last);
                }
            }
            KeyCode::Up if n > 0 => self.pick = self.pick.saturating_sub(1),
            KeyCode::Down if n > 0 => self.pick = (self.pick + 1).min(n - 1),
            KeyCode::Up if !self.input[..self.cursor].contains('\n') => self.history(-1),
            KeyCode::Down if !self.input[self.cursor..].contains('\n') => self.history(1),
            KeyCode::Up | KeyCode::Down => self.vertical(k.code == KeyCode::Up),
            KeyCode::PageUp => self.scroll += self.hits.body.height.saturating_sub(2).max(1) as usize,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(self.hits.body.height.saturating_sub(2).max(1) as usize),
            KeyCode::End if self.input.is_empty() => self.scroll = 0,
            KeyCode::Left => self.cursor = self.input[..self.cursor].char_indices().next_back().map_or(0, |(i, _)| i),
            KeyCode::Right => self.cursor = self.input[self.cursor..].chars().next().map_or(self.cursor, |c| self.cursor + c.len_utf8()),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace if alt || ctrl => self.delete_word(),
            KeyCode::Backspace if self.input.is_empty() && self.shell_mode => self.shell_mode = false,
            KeyCode::Backspace => {
                // A paste chip goes in one press.
                if let Some(tag) = self.pastes.iter().map(|(t, _)| t.clone()).find(|t| self.input[..self.cursor].ends_with(t.as_str())) {
                    let start = self.cursor - tag.len();
                    self.input.replace_range(start..self.cursor, "");
                    self.cursor = start;
                } else if let Some((i, _)) = self.input[..self.cursor].char_indices().next_back() {
                    self.input.remove(i);
                    self.cursor = i;
                }
                self.hide_popup = false;
            }
            KeyCode::Delete if self.cursor < self.input.len() => {
                self.input.remove(self.cursor);
            }
            KeyCode::Char('!') if self.input.is_empty() && !self.shell_mode => self.shell_mode = true,
            KeyCode::Char('?') if self.input.is_empty() && !self.shell_mode => self.overlay = Some(Overlay::Help(0)),
            KeyCode::Char(c) if !ctrl => {
                self.insert(&c.to_string());
                self.pick = 0;
                self.hide_popup = false;
            }
            _ => {}
        }
        true
    }

    /// Take the popup's pick. `run` (Enter) also runs a command that needs no argument.
    fn accept(&mut self, p: Popup, run: bool) -> bool {
        let pick = self.pick.min(p.len().saturating_sub(1));
        self.pick = 0;
        match p {
            Popup::Commands(v) => {
                let cmd = v[pick].0;
                if run && !matches!(cmd, "/export") {
                    self.set_input(String::new());
                    self.history.push(cmd.to_string());
                    return self.command(cmd, "");
                }
                self.set_input(format!("{cmd} "));
            }
            Popup::Models(v) => {
                let id = v[pick].clone();
                self.set_input(String::new());
                self.set_model(&id);
            }
            Popup::Files(v) => {
                if let Some((at, _)) = self.mention() {
                    let path = format!("@{} ", v[pick]);
                    self.input.replace_range(at..self.cursor, &path);
                    self.cursor = at + path.len();
                }
            }
        }
        true
    }

    pub(crate) fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub(crate) fn set_input(&mut self, s: String) {
        self.cursor = s.len();
        self.input = s;
        self.pick = 0;
        self.hide_popup = false;
    }

    fn line_start(&self) -> usize {
        self.input[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.input[self.cursor..].find('\n').map_or(self.input.len(), |i| self.cursor + i)
    }

    fn delete_word(&mut self) {
        let before = &self.input[..self.cursor];
        let start = before.trim_end().rfind(|c: char| c.is_whitespace()).map_or(0, |i| i + 1);
        self.input.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Move the cursor a line up or down inside a multiline draft, keeping its column.
    fn vertical(&mut self, up: bool) {
        let col = self.cursor - self.line_start();
        let target = if up {
            let prev_end = self.line_start().saturating_sub(1);
            self.input[..prev_end].rfind('\n').map_or(0, |i| i + 1)
        } else {
            self.line_end() + 1
        };
        let end = self.input[target..].find('\n').map_or(self.input.len(), |i| target + i);
        let mut c = (target + col).min(end);
        while !self.input.is_char_boundary(c) {
            c -= 1;
        }
        self.cursor = c;
    }

    fn history(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.hist, dir) {
            (None, -1) => Some(self.history.len() - 1),
            (None, _) => None,
            (Some(i), -1) => Some(i.saturating_sub(1)),
            (Some(i), _) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), _) => None,
        };
        if next.is_none() && self.hist.is_none() {
            return;
        }
        self.hist = next;
        self.set_input(next.map(|i| self.history[i].clone()).unwrap_or_default());
    }

    fn submit(&mut self) -> bool {
        let mut text = self.input.trim().to_string();
        if text.is_empty() {
            return true;
        }
        self.set_input(String::new());
        self.hist = None;
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        if self.shell_mode {
            self.shell(text);
            return true;
        }
        if text.starts_with('/') {
            let (cmd, arg) = text.split_once(' ').map_or((text.as_str(), ""), |(c, a)| (c, a.trim()));
            let (cmd, arg) = (cmd.to_string(), arg.to_string());
            return self.command(&cmd, &arg);
        }
        for (tag, body) in std::mem::take(&mut self.pastes) {
            text = text.replace(&tag, &body);
        }
        self.send(&text);
        true
    }
}
