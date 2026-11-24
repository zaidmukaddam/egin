//! `egin chat`: a fullscreen terminal UI over one session, modelled on grok-build's TUI. A prompt and a transcript of
//! blocks take the keyboard in turn (Tab). Blocks fold one at a time or as a verb group ("Read 3 files"), cards take
//! over the composer for approvals and questions, pickers list sessions and models, and a turn row shows what runs.
//! Everything on screen comes from the session's events; approvals and questions are answered through
//! `App::pending`, as `egin serve` answers them.

mod blocks;
mod files;
mod input;
mod logo;
mod text;
mod theme;
mod view;

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::SetTitle,
};
use egin::{
    agent::{run, Agent},
    model, App, Event,
};
use futures_util::StreamExt;
use ratatui::{layout::Rect, DefaultTerminal};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast::error::RecvError, mpsc};

/// How much of a block shows: its header only, a preview, or all of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Collapsed,
    Truncated,
    Expanded,
}

pub(crate) enum Kind {
    Welcome,
    User(String),
    Queued(String),
    Assistant {
        text: String,
        live: bool,
    },
    Thinking {
        text: String,
        started: Instant,
        took: Option<Duration>,
    },
    Tool {
        id: String,
        name: String,
        args: Value,
        out: Option<String>,
        failed: bool,
        started: Instant,
        at_ms: u64,
        took: Option<Duration>,
    },
    /// A command the user ran with `!`, in the agent's sandbox.
    Shell {
        id: u64,
        cmd: String,
        out: Option<String>,
        ok: bool,
        started: Instant,
        took: Option<Duration>,
    },
    Note(String),
    Error(String),
    Interrupted,
    Worked {
        took: Duration,
        tools: u64,
        out_tok: u64,
        cost: f64,
    },
}

pub(crate) struct Item {
    pub kind: Kind,
    /// Set when the user folded or unfolded it; otherwise the kind's default applies.
    pub mode: Option<Mode>,
    pub at: chrono::DateTime<chrono::Local>,
}

impl Item {
    fn new(kind: Kind) -> Item {
        Item { kind, mode: None, at: chrono::Local::now() }
    }

    pub fn live(&self) -> bool {
        matches!(
            self.kind,
            Kind::Assistant { live: true, .. } | Kind::Thinking { took: None, .. } | Kind::Tool { out: None, .. } | Kind::Shell { out: None, .. }
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Prompt,
    Scrollback,
}

pub(crate) struct Turn {
    task: tokio::task::AbortHandle,
    started: Instant,
    phase: String,
    phase_at: Instant,
    out_tok: u64,
    tools: u64,
    cost: f64,
}

/// A blocking card: it takes the composer's place until the user answers.
pub(crate) enum Card {
    Approve { id: String, tool: String, args: Value, from: Option<String>, pick: usize, feedback: String },
    Ask { id: String, question: String, options: Vec<String>, pick: usize, other: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickKind {
    Resume,
    Model,
}

pub(crate) struct Picker {
    pub kind: PickKind,
    pub title: &'static str,
    /// (label, detail, value)
    pub rows: Vec<(String, String, String)>,
    pub filter: String,
    pub pick: usize,
}

impl Picker {
    pub fn shown(&self) -> Vec<&(String, String, String)> {
        self.rows.iter().filter(|r| files::fuzzy(&self.filter, &format!("{} {}", r.0, r.1)).is_some()).collect()
    }
}

pub(crate) struct Viewer {
    pub title: String,
    pub text: String,
    pub markdown: bool,
    pub scroll: usize,
}

pub(crate) enum Overlay {
    Picker(Picker),
    Viewer(Viewer),
    Help(usize),
}

pub(crate) enum Msg {
    Shell { id: u64, ok: bool, out: String },
    Models(Vec<(String, model::ModelInfo)>),
}

/// What the last frame put where, for the mouse and for moving the selection.
#[derive(Default)]
pub(crate) struct Hits {
    /// (first row, last row, unit anchor) of each transcript unit on screen.
    pub units: Vec<(u16, u16, usize)>,
    /// Every unit's anchor in transcript order, with its first and last line.
    pub order: Vec<(usize, usize, usize)>,
    pub stop: Option<Rect>,
    pub menu: Vec<(Rect, &'static str)>,
    pub composer: Option<Rect>,
    pub body: Rect,
    /// The transcript line at the top of the body this frame.
    pub start: usize,
}

pub(crate) struct Ui {
    app: Arc<App>,
    agent: Arc<Agent>,
    overrides: toml::Table,
    sid: String,
    items: Vec<Item>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist: Option<usize>,
    pick: usize,
    /// Esc closed the completion popup; typing brings it back.
    hide_popup: bool,
    focus: Focus,
    sel: Option<usize>,
    open_groups: HashSet<usize>,
    think_open: bool,
    scroll: usize,
    last_total: usize,
    turn: Option<Turn>,
    cancelling: bool,
    card: Option<Card>,
    overlay: Option<Overlay>,
    trusted: HashSet<String>,
    yolo: bool,
    shell_mode: bool,
    stash: Option<String>,
    pastes: Vec<(String, String)>,
    files: Option<Vec<String>>,
    ctx: u64,
    window: Option<u64>,
    models: Vec<(String, model::ModelInfo)>,
    cost: f64,
    toast: Option<(String, Instant)>,
    quit_at: Option<Instant>,
    esc_at: Option<Instant>,
    born: Instant,
    shown_busy: Option<bool>,
    replaying: bool,
    next_shell: u64,
    tx: mpsc::UnboundedSender<Msg>,
    logo: Option<logo::Logo>,
    logo_at: Option<(u16, u16)>,
    hits: Hits,
    branch: Option<String>,
}

pub async fn chat(agent: Arc<Agent>, app: Arc<App>, overrides: toml::Table) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut ui = Ui::new(agent, app, overrides, tx)?;
    let mut term = ratatui::init();
    let mut out = std::io::stdout();
    // Shift+Enter is only told apart from Enter where the terminal speaks the kitty keyboard protocol.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    ui.logo = logo::detect().map(|p| logo::Logo::new(p, 4));
    execute!(out, EnableMouseCapture, EnableBracketedPaste)?;
    if enhanced {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    let result = ui.run(&mut term, &mut rx).await;
    if let Some(l) = &ui.logo {
        l.clear();
    }
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, SetTitle(""));
    progress(false);
    ratatui::restore();
    if let Some(t) = ui.turn.take() {
        t.task.abort();
    }
    eprintln!("\x1b[2msession {} · resume it with /resume in egin chat, or: egin run {} --session {}\x1b[0m", ui.sid, ui.agent.dir.display(), ui.sid);
    result
}

/// The terminal tab's progress indicator (OSC 9;4), indeterminate while a turn runs.
fn progress(on: bool) {
    let mut o = std::io::stdout();
    let _ = write!(o, "\x1b]9;4;{};0\x07", if on { 3 } else { 0 });
    let _ = o.flush();
}

pub(crate) fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        format!("{}m {:02}s", s as u64 / 60, s as u64 % 60)
    }
}

pub(crate) fn k(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// "1 line", "3 lines".
pub(crate) fn n_lines(n: usize) -> String {
    format!("{n} line{}", if n == 1 { "" } else { "s" })
}

#[test]
fn n_lines_singular() {
    assert_eq!(n_lines(1), "1 line");
    assert_eq!(n_lines(0), "0 lines");
    assert_eq!(n_lines(12), "12 lines");
}

/// Copy to the clipboard: pbcopy on a local Mac, else OSC 52 through the terminal.
pub(crate) fn copy(text: &str) -> bool {
    use std::process::{Command, Stdio};
    if cfg!(target_os = "macos") && std::env::var("SSH_CONNECTION").is_err() {
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            return child.wait().is_ok_and(|s| s.success());
        }
    }
    use base64::Engine;
    let mut o = std::io::stdout();
    write!(o, "\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text)).and_then(|_| o.flush()).is_ok()
}

fn git_branch(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git").args(["-C"]).arg(dir).args(["rev-parse", "--abbrev-ref", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|b| !b.is_empty())
}

impl Ui {
    fn new(agent: Arc<Agent>, app: Arc<App>, overrides: toml::Table, tx: mpsc::UnboundedSender<Msg>) -> Result<Ui> {
        let sid = run::new_session(&app, &agent)?;
        let (a, t) = (app.clone(), tx.clone());
        tokio::spawn(async move {
            if let Ok(models) = model::fetch_models(&a).await {
                let mut list: Vec<_> = models.into_iter().collect();
                list.sort_by(|x, y| x.0.cmp(&y.0));
                let _ = t.send(Msg::Models(list));
            }
        });
        let branch = git_branch(&agent.dir);
        Ok(Ui {
            app,
            agent,
            overrides,
            sid,
            items: vec![Item::new(Kind::Welcome)],
            input: String::new(),
            cursor: 0,
            history: vec![],
            hist: None,
            pick: 0,
            hide_popup: false,
            focus: Focus::Prompt,
            sel: None,
            open_groups: HashSet::new(),
            think_open: false,
            scroll: 0,
            last_total: 0,
            turn: None,
            cancelling: false,
            card: None,
            overlay: None,
            trusted: HashSet::new(),
            yolo: false,
            shell_mode: false,
            stash: None,
            pastes: vec![],
            files: None,
            ctx: 0,
            window: None,
            models: vec![],
            cost: 0.0,
            toast: None,
            quit_at: None,
            esc_at: None,
            born: Instant::now(),
            shown_busy: None,
            replaying: false,
            next_shell: 0,
            tx,
            logo: None,
            logo_at: None,
            hits: Hits::default(),
            branch,
        })
    }

    async fn run(&mut self, term: &mut DefaultTerminal, msgs: &mut mpsc::UnboundedReceiver<Msg>) -> Result<()> {
        let mut rx = self.app.events.subscribe();
        let mut keys = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            self.terminal_state();
            term.draw(|f| self.draw(f))?;
            // The logo image sits over cells the frame left blank for it.
            if let Some(l) = &mut self.logo {
                if l.show(self.logo_at) {
                    term.clear()?;
                    term.draw(|f| self.draw(f))?;
                    if let Some(l) = &mut self.logo {
                        l.show(self.logo_at);
                    }
                }
            }
            tokio::select! {
                k = keys.next() => match k {
                    Some(Ok(ev)) => {
                        if !self.on_input(ev) {
                            return Ok(());
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                },
                e = rx.recv() => {
                    match e {
                        Ok(e) => self.on_event(e),
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return Ok(()),
                    }
                    // Tokens come in bursts; take them all before drawing again.
                    while let Ok(e) = rx.try_recv() {
                        self.on_event(e);
                    }
                }
                Some(m) = msgs.recv() => self.on_msg(m),
                _ = tick.tick(), if self.animating() => {}
            }
        }
    }

    fn animating(&self) -> bool {
        self.turn.is_some() || self.toast.is_some() || self.items.iter().any(Item::live) || matches!(self.items.as_slice(), [Item { kind: Kind::Welcome, .. }])
    }

    fn terminal_state(&mut self) {
        let busy = self.turn.is_some();
        if self.shown_busy != Some(busy) {
            self.shown_busy = Some(busy);
            let name = self.agent.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let _ = execute!(std::io::stdout(), SetTitle(if busy { format!("egin · working · {name}") } else { format!("egin · {name}") }));
            progress(busy);
        }
        if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_millis(2200)) {
            self.toast = None;
        }
    }

    fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
    }

    fn push(&mut self, kind: Kind) {
        if matches!(self.items.first(), Some(Item { kind: Kind::Welcome, .. })) && !matches!(kind, Kind::Note(_) | Kind::Error(_)) {
            self.items.remove(0);
            self.sel = None;
            self.open_groups.clear();
        }
        self.items.push(Item::new(kind));
    }

    fn note(&mut self, s: impl Into<String>) {
        self.push(Kind::Note(s.into()));
    }

    fn phase(&mut self, p: impl Into<String>) {
        if let Some(t) = &mut self.turn {
            let p = p.into();
            if t.phase != p {
                t.phase = p;
                t.phase_at = Instant::now();
            }
        }
    }

    fn close_thinking(&mut self) {
        for it in self.items.iter_mut().rev() {
            if let Kind::Thinking { started, took: took @ None, .. } = &mut it.kind {
                *took = Some(started.elapsed());
            }
        }
    }

    fn answer(&self, id: &str, v: Value) {
        if let Some(tx) = self.app.pending.lock().unwrap().remove(id) {
            let _ = tx.send(v);
        }
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Shell { id, ok, out } => {
                for it in self.items.iter_mut().rev() {
                    if let Kind::Shell { id: sid, out: o, ok: k, started, took, .. } = &mut it.kind {
                        if *sid == id {
                            *o = Some(out);
                            *k = ok;
                            *took = Some(started.elapsed());
                            break;
                        }
                    }
                }
            }
            Msg::Models(list) => {
                self.window = list.iter().find(|(id, _)| *id == self.agent.cfg.model).map(|(_, i)| i.context).filter(|c| *c > 0);
                self.models = list;
            }
        }
    }

    fn on_event(&mut self, e: Event) {
        let b = &e.body;
        let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
        let kind = b["type"].as_str().unwrap_or("");
        // Helpers and subagents ask in their own sessions; nobody else is there to answer.
        if !self.replaying {
            match kind {
                "approval_request" => {
                    let tool = s("tool");
                    if self.yolo || self.trusted.contains(&tool) {
                        self.answer(&s("call_id"), json!(true));
                    } else {
                        let from = (e.session != self.sid).then(|| "a helper".to_string());
                        self.card = Some(Card::Approve { id: s("call_id"), tool, args: b["args"].clone(), from, pick: 0, feedback: String::new() });
                        self.phase("Waiting for you");
                    }
                    return;
                }
                "question" => {
                    let options = b["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(String::from)).collect();
                    self.card = Some(Card::Ask { id: s("call_id"), question: s("question"), options, pick: 0, other: String::new() });
                    self.phase("Waiting for you");
                    return;
                }
                _ => {}
            }
        }
        if e.session != self.sid {
            return;
        }
        match kind {
            "user" => {
                let text = s("text");
                if let Some(i) = self.items.iter().position(|it| matches!(&it.kind, Kind::Queued(q) if *q == text)) {
                    self.items.remove(i);
                }
                self.push(Kind::User(text));
            }
            "queued" => self.push(Kind::Queued(s("text"))),
            "reasoning" => {
                self.phase("Thinking");
                match self.items.last_mut() {
                    Some(Item { kind: Kind::Thinking { text, took: None, .. }, .. }) => text.push_str(&s("text")),
                    _ => self.push(Kind::Thinking { text: s("text"), started: Instant::now(), took: None }),
                }
            }
            "token" => {
                self.close_thinking();
                self.phase("Responding");
                match self.items.last_mut() {
                    Some(Item { kind: Kind::Assistant { text, live: true }, .. }) => text.push_str(&s("text")),
                    _ => self.push(Kind::Assistant { text: s("text"), live: true }),
                }
            }
            "assistant" => {
                self.close_thinking();
                let content = b["content"].as_str().unwrap_or("").to_string();
                let reasoning = b["reasoning"].as_str().unwrap_or("").to_string();
                let streamed = self.items.iter_mut().rev().find_map(|it| match &mut it.kind {
                    Kind::Assistant { text, live: live @ true } => Some((text, live)),
                    _ => None,
                });
                match streamed {
                    Some((text, live)) => {
                        *live = false;
                        if !content.trim().is_empty() {
                            *text = content;
                        }
                    }
                    None => {
                        // Not streamed (a resumed session, or a provider that answers whole): its thinking comes with it.
                        let thought = self.items.last().is_some_and(|it| matches!(it.kind, Kind::Thinking { .. }));
                        if !reasoning.trim().is_empty() && !thought {
                            self.push(Kind::Thinking {
                                text: reasoning,
                                started: Instant::now(),
                                took: Some(Duration::from_millis(b["ms"].as_u64().unwrap_or(0))),
                            });
                        }
                        if !content.trim().is_empty() {
                            self.push(Kind::Assistant { text: content, live: false });
                        }
                    }
                }
                let u = &b["usage"];
                self.ctx = u["prompt"].as_u64().unwrap_or(self.ctx);
                let cost = b["cost"].as_f64().unwrap_or(0.0);
                self.cost += cost;
                if let Some(t) = &mut self.turn {
                    t.out_tok += u["completion"].as_u64().unwrap_or(0);
                    t.cost += cost;
                }
                self.phase("Thinking");
            }
            "tool_call" => {
                let (name, args) = (s("tool"), b["args"].clone());
                let label = format!("{} {}", blocks::verbs(&name).0, text::trunc(&blocks::detail(&name, &args), 32));
                self.phase(label);
                if let Some(t) = &mut self.turn {
                    t.tools += 1;
                }
                let at_ms = b["at"].as_u64().unwrap_or(0);
                self.push(Kind::Tool { id: s("call_id"), name, args, out: None, failed: false, started: Instant::now(), at_ms, took: None });
            }
            "tool_result" => {
                let id = s("call_id");
                let output = s("output");
                let interrupted = b["interrupted"].as_bool().unwrap_or(false);
                let (replaying, at) = (self.replaying, b["at"].as_u64().unwrap_or(0));
                for it in self.items.iter_mut().rev() {
                    if let Kind::Tool { id: tid, out, failed, started, at_ms, took, .. } = &mut it.kind {
                        if *tid == id {
                            *failed = interrupted || output.starts_with("error:");
                            *out = Some(output);
                            *took = Some(if replaying { Duration::from_millis(at.saturating_sub(*at_ms)) } else { started.elapsed() });
                            break;
                        }
                    }
                }
                self.phase("Thinking");
            }
            "web_search" | "web_extract" | "web_fetch" | "code_execution" => self.push(Kind::Tool {
                id: String::new(),
                name: kind.into(),
                args: b.clone(),
                out: Some(String::new()),
                failed: false,
                started: Instant::now(),
                at_ms: 0,
                took: Some(Duration::ZERO),
            }),
            "elided" | "summarised" => self.note("context compacted to fit the model's window"),
            "subagent" => self.note(format!("{} started", s("agent"))),
            "deadline" => self.note(format!("{} left on the clock", secs(Duration::from_secs_f64(b["left_secs"].as_f64().unwrap_or(0.0))))),
            "model_timeout" => self.note("the model took too long; asked again with low effort"),
            "done" | "error" | "cancelled" => self.end_turn(kind, &s("message")),
            _ => {}
        }
    }

    fn end_turn(&mut self, kind: &str, message: &str) {
        self.close_thinking();
        for it in &mut self.items {
            match &mut it.kind {
                Kind::Tool { out: out @ None, failed, took, started, .. } => {
                    *out = Some("interrupted".into());
                    *failed = true;
                    *took = Some(started.elapsed());
                }
                Kind::Assistant { live, .. } => *live = false,
                _ => {}
            }
        }
        self.card = None;
        self.cancelling = false;
        match kind {
            "error" => self.push(Kind::Error(message.to_string())),
            "cancelled" if !self.replaying => self.push(Kind::Interrupted),
            _ => {}
        }
        if let Some(t) = self.turn.take() {
            if kind == "done" {
                self.push(Kind::Worked { took: t.started.elapsed(), tools: t.tools, out_tok: t.out_tok, cost: t.cost });
            }
        }
        // A message queued as the run was finishing would otherwise wait for the next one.
        if kind == "done" && !self.replaying && run::deliver_inbox(&self.app, &self.sid, 100).unwrap_or(0) > 0 {
            self.start_turn();
        }
    }

    fn start_turn(&mut self) {
        let (app, agent, sid) = (self.app.clone(), self.agent.clone(), self.sid.clone());
        let task = tokio::spawn(async move {
            let _ = run::run_session(app, agent, sid).await;
        });
        let now = Instant::now();
        self.turn = Some(Turn { task: task.abort_handle(), started: now, phase: "Starting".into(), phase_at: now, out_tok: 0, tools: 0, cost: 0.0 });
    }

    fn interrupt(&mut self) {
        if let Some(t) = &self.turn {
            self.cancelling = true;
            t.task.abort();
            let _ = self.app.store.set_status(&self.sid, "cancelled");
            self.app.emit(&self.sid, "cancelled", json!({}));
        }
    }

    /// Send a message: into the running turn's inbox, or as a new turn.
    fn send(&mut self, text: &str) {
        self.scroll = 0;
        self.sel = None;
        let sent = if self.turn.is_some() { run::enqueue(&self.app, &self.sid, text, &[], vec![]) } else { run::send(&self.app, &self.sid, text) };
        match sent {
            Ok(()) if self.turn.is_none() => self.start_turn(),
            Ok(()) => {}
            Err(e) => self.push(Kind::Error(format!("{e:#}"))),
        }
    }

    /// A `!` command, run in the session's sandbox like the agent's own bash calls.
    fn shell(&mut self, cmd: String) {
        self.scroll = 0;
        let id = self.next_shell;
        self.next_shell += 1;
        self.push(Kind::Shell { id, cmd: cmd.clone(), out: None, ok: true, started: Instant::now(), took: None });
        let (app, agent, sid, tx) = (self.app.clone(), self.agent.clone(), self.sid.clone(), self.tx.clone());
        tokio::spawn(async move {
            let out = egin::sandbox::exec(&app, &agent, &sid, &["sh".into(), "-c".into(), cmd], None, 300).await;
            let _ = tx.send(Msg::Shell { id, ok: out.ok, out: out.text });
        });
    }

    fn new_session(&mut self) {
        self.interrupt();
        self.turn = None;
        match run::new_session(&self.app, &self.agent) {
            Ok(sid) => {
                self.sid = sid;
                self.items = vec![Item::new(Kind::Welcome)];
                self.reset_view();
                self.ctx = 0;
                self.cost = 0.0;
            }
            Err(e) => self.push(Kind::Error(format!("{e:#}"))),
        }
    }

    fn reset_view(&mut self) {
        self.sel = None;
        self.open_groups.clear();
        self.scroll = 0;
        self.focus = Focus::Prompt;
        self.card = None;
    }

    /// Switch to a stored session and replay its events into the transcript.
    fn resume(&mut self, sid: &str) {
        self.interrupt();
        self.turn = None;
        let events = match self.app.store.events_after(sid, 0) {
            Ok(e) => e,
            Err(e) => return self.push(Kind::Error(format!("{e:#}"))),
        };
        self.sid = sid.to_string();
        self.items = vec![];
        self.reset_view();
        self.replaying = true;
        for (id, body) in events {
            self.on_event(Event { session: sid.to_string(), id: Some(id), body });
        }
        self.replaying = false;
        // After the replay, which adds each model call's cost again; the store's total is the true one.
        self.cost = self.app.store.stats(sid).map(|s| s.cost).unwrap_or(0.0);
        self.close_thinking();
        if self.items.is_empty() {
            self.items.push(Item::new(Kind::Welcome));
        }
        self.toast(format!("resumed session {}", &sid[..sid.len().min(12)]));
    }

    fn set_model(&mut self, id: &str) {
        self.overrides.insert("model".into(), id.into());
        match Agent::load(&self.agent.dir, &self.overrides) {
            Ok(a) => {
                self.agent = Arc::new(a);
                self.window = self.models.iter().find(|(m, _)| m == id).map(|(_, i)| i.context).filter(|c| *c > 0);
                let later = if self.turn.is_some() { " from the next turn" } else { "" };
                self.toast(format!("model set to {id}{later}"));
            }
            Err(e) => self.push(Kind::Error(format!("{e:#}"))),
        }
    }

    /// The conversation as Markdown, for /export.
    fn transcript_markdown(&self) -> String {
        let mut out = format!("# egin session {}\n\n", self.sid);
        for it in &self.items {
            match &it.kind {
                Kind::User(t) => out += &format!("## You · {}\n\n{t}\n\n", it.at.format("%H:%M")),
                Kind::Assistant { text, .. } => out += &format!("{text}\n\n"),
                Kind::Tool { name, args, out: o, .. } => {
                    out += &format!("**{} {}**\n\n", blocks::verbs(name).1, blocks::detail(name, args));
                    if let Some(o) = o.as_deref().filter(|o| !o.is_empty()) {
                        out += &format!("```\n{}\n```\n\n", o.trim_end());
                    }
                }
                Kind::Shell { cmd, out: o, .. } => out += &format!("```\n$ {cmd}\n{}\n```\n\n", o.as_deref().unwrap_or("").trim_end()),
                Kind::Error(e) => out += &format!("> error: {e}\n\n"),
                _ => {}
            }
        }
        out
    }

    fn command(&mut self, cmd: &str, arg: &str) -> bool {
        match cmd {
            "/exit" | "/quit" => return false,
            "/new" | "/clear" => self.new_session(),
            "/resume" => self.open_resume(),
            "/model" if arg.is_empty() => self.open_models(),
            "/model" => self.set_model(arg),
            "/help" | "/keys" => self.overlay = Some(Overlay::Help(0)),
            "/always-approve" | "/yolo" => {
                self.yolo = !self.yolo;
                self.toast(if self.yolo { "always-approve on: every tool call runs without asking" } else { "always-approve off" });
            }
            "/copy" => {
                let last = self.items.iter().rev().find_map(|it| match &it.kind {
                    Kind::Assistant { text, .. } => Some(text.clone()),
                    _ => None,
                });
                match last {
                    Some(t) if copy(&t) => self.toast("copied the last answer"),
                    Some(_) => self.toast("couldn't reach the clipboard"),
                    None => self.toast("no answer to copy yet"),
                }
            }
            "/export" => {
                let path = if arg.is_empty() { format!("egin-{}.md", &self.sid[..self.sid.len().min(12)]) } else { arg.to_string() };
                match std::fs::write(&path, self.transcript_markdown()) {
                    Ok(()) => self.toast(format!("wrote {path}")),
                    Err(e) => self.push(Kind::Error(format!("writing {path}: {e}"))),
                }
            }
            "/context" => {
                let s = match self.window {
                    Some(w) => format!("context: {} of {} tokens ({:.0}%)", k(self.ctx), k(w), 100.0 * self.ctx as f64 / w as f64),
                    None => format!("context: {} tokens", k(self.ctx)),
                };
                self.note(s);
            }
            "/usage" => match self.app.store.stats(&self.sid) {
                Ok(s) => self.note(format!(
                    "{} model calls · {} tool calls · {} prompt tokens ({} cached) · {} out · ${:.4} · {}",
                    s.model_calls,
                    s.tool_calls,
                    k(s.prompt_tokens),
                    k(s.cached_tokens),
                    k(s.completion_tokens),
                    s.cost,
                    secs(Duration::from_millis(s.wall_ms))
                )),
                Err(e) => self.push(Kind::Error(format!("{e:#}"))),
            },
            _ => self.push(Kind::Error(format!("no command {cmd}; /help lists them"))),
        }
        true
    }

    fn open_resume(&mut self) {
        let sessions = self.app.store.list_sessions(None, 60).unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        let rows = sessions
            .iter()
            .filter(|s| s["id"].as_str() != Some(self.sid.as_str()) && s["title"].is_string())
            .map(|s| {
                let ago = now - s["created"].as_i64().unwrap_or(now);
                let when = match ago {
                    a if a < 60 => "just now".to_string(),
                    a if a < 3600 => format!("{}m ago", a / 60),
                    a if a < 86_400 => format!("{}h ago", a / 3600),
                    a => format!("{}d ago", a / 86_400),
                };
                let title = s["title"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
                (title, format!("{when} · {}", s["status"].as_str().unwrap_or("")), s["id"].as_str().unwrap_or("").to_string())
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return self.toast("no earlier sessions for this agent");
        }
        self.overlay = Some(Overlay::Picker(Picker { kind: PickKind::Resume, title: "Resume a session", rows, filter: String::new(), pick: 0 }));
    }

    fn open_models(&mut self) {
        if self.models.is_empty() {
            return self.toast("the model list hasn't loaded; try /model <id>");
        }
        let current = self.agent.cfg.model.clone();
        let rows = self
            .models
            .iter()
            .map(|(id, m)| {
                let mark = if *id == current { "  ← current" } else { "" };
                (id.clone(), format!("{} ctx · ${}/{} per M{mark}", k(m.context), m.input, m.output), id.clone())
            })
            .collect();
        let pick = self.models.iter().position(|(id, _)| *id == current).unwrap_or(0);
        self.overlay = Some(Overlay::Picker(Picker { kind: PickKind::Model, title: "Pick a model", rows, filter: String::new(), pick }));
    }
}
