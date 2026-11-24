//! The `egin` command line: argument parsing and one handler per command.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use egin::{
    agent::{self, output, run, Agent},
    eval::{self, bench, evolve, report},
    model, server,
    tools::{mcp, oauth},
    App, Approve,
};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Parser)]
#[command(name = "egin", version, about = "Filesystem-first framework for durable backend agents")]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a starter agent folder
    Init {
        dir: PathBuf,
        /// basic, research, ops, support, web, or a path to a folder to copy
        #[arg(long, default_value = "basic")]
        template: String,
    },
    /// Add an integration from egin's catalog (a connection, channel, exporter or tool); with no item, list them
    Add {
        /// Such as connection/github or channel/slack; `channel <docs url>` prints a guide for a coding
        /// agent to describe a channel for any other platform
        item: Option<String>,
        /// With `channel`: the provider's webhook docs
        source: Option<String>,
        /// The agent folder, or an app with its agent in agent/
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Overwrite files that already exist
        #[arg(long)]
        force: bool,
    },
    /// Sign in to an MCP connection that uses OAuth (auth = "oauth"), in the browser
    Connect {
        /// The connection's name: connections/<name>.toml
        name: String,
        /// The agent folder, or an app with its agent in agent/
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Print egin's documentation; with no page, the list of pages
    Docs { page: Option<String> },
    /// Show what an agent folder loads to
    Info { dir: PathBuf },
    /// Write a session as an ATIF trajectory (JSON), for Harbor or Repo2RLEnv
    Trajectory {
        dir: PathBuf,
        /// The session; defaults to the most recent one
        #[arg(long)]
        session: Option<String>,
        /// Write here instead of stdout
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// List the router's models with limits and prices
    Models,
    /// Run one prompt to completion
    Run {
        dir: PathBuf,
        /// The prompt; `-` reads it from stdin (keeps a long prompt off the process list)
        prompt: Option<String>,
        /// Approve every tool call
        #[arg(long)]
        yes: bool,
        /// Resume an existing session (the prompt is optional then)
        #[arg(long)]
        session: Option<String>,
        /// Override an agent.toml key, e.g. --set model=glm-5.3
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// A JSON Schema file: the answer is a value that fits it, printed as JSON
        #[arg(long)]
        schema: Option<PathBuf>,
    },
    /// Interactive session in the terminal
    Chat {
        dir: PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// Advance a session its host stores: the session as JSON on stdin, what it is now on stdout
    #[command(hide = true)]
    Step {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Serve the agent over HTTP with a server-sent event stream, channels and schedules
    Serve {
        /// The agent folder, or an app with its agent in agent/
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Port to listen on [default: $PORT, else 8787]
        #[arg(long)]
        port: Option<u16>,
        /// Address to bind; anything but loopback needs EGIN_TOKEN
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Run evals/ across the variants in evals/variants.toml
    Eval {
        dir: PathBuf,
        #[arg(long)]
        variant: Vec<String>,
        #[arg(long)]
        model: Vec<String>,
        /// Only tasks in this split (train or heldout)
        #[arg(long)]
        split: Option<String>,
        /// Only these tasks
        #[arg(long)]
        task: Vec<String>,
        /// Runs at a time
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// Edit a model's profile files from eval traces, keeping only edits that score better
    Evolve {
        dir: PathBuf,
        /// The model whose profile is tuned
        #[arg(long)]
        model: String,
        /// Model that proposes edits (default: the same model)
        #[arg(long)]
        proposer: Option<String>,
        #[arg(long, default_value_t = 4)]
        rounds: usize,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Build an HTML results page from results folders (each with summary.jsonl)
    Report {
        results: Vec<PathBuf>,
        #[arg(long, default_value = "egin results")]
        title: String,
        /// Markdown/plain text file describing the method
        #[arg(long)]
        method: Option<PathBuf>,
        #[arg(long, short, default_value = "results.html")]
        out: PathBuf,
    },
    /// Run Terminal-Bench 2 tasks (or converted SWE-bench tasks) in microVMs
    Bench {
        /// Folder of tasks
        tasks: PathBuf,
        /// Agent folder whose evals/variants.toml defines the arms to compare
        #[arg(long, default_value = "bench/tb2")]
        agent: PathBuf,
        /// Only these tasks (repeatable); default all
        #[arg(long)]
        task: Vec<String>,
        /// File listing task names, one per line
        #[arg(long)]
        subset: Option<PathBuf>,
        #[arg(long)]
        variant: Vec<String>,
        #[arg(long)]
        model: Vec<String>,
        /// Run the reference solutions instead of the agent, to validate environments and tests
        #[arg(long)]
        oracle: bool,
        /// Task environments built and kept at a time
        #[arg(long, default_value_t = 3)]
        jobs: usize,
        /// Agent runs at a time across all tasks (each in its own fork)
        #[arg(long, default_value_t = 8)]
        parallel: usize,
        /// Where results go (default <agent>/results/<run id>)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Where task environments run: microsandbox (local microVMs) or daytona (Daytona cloud
        /// sandboxes: x86, so amd64-only images run as they are; needs DAYTONA_API_KEY)
        #[arg(long, default_value = "microsandbox", value_parser = ["microsandbox", "daytona"])]
        sandbox: String,
    },
}

/// `--set KEY=VALUE` pairs as agent.toml overrides. Bare words are strings; numbers, booleans
/// and arrays parse as TOML.
fn overrides(set: &[String]) -> Result<toml::Table> {
    let mut t = toml::Table::new();
    for kv in set {
        let Some((k, v)) = kv.split_once('=') else { bail!("--set wants KEY=VALUE, got {kv}") };
        let parsed = toml::from_str::<toml::Table>(&format!("v = {v}")).ok().and_then(|mut t| t.remove("v"));
        t.insert(k.into(), parsed.unwrap_or(toml::Value::String(v.into())));
    }
    Ok(t)
}

fn open(dir: &Path, approve: Approve) -> Result<Arc<App>> {
    // Keys are checked when a provider is first called, so an agent needs only the ones it uses.
    App::new(&egin::state_db(dir), approve)
}

/// Before a session starts: say what egin chose for settings agent.toml leaves out, and stop with
/// the exact key to set when the model's provider has none, rather than failing at the first call.
fn ready(app: &App, agent: &Agent) -> Result<()> {
    let providers = ["anthropic", "openai", "gateway", "you", "bedrock", "gemini"];
    // An agent pointed at its own endpoint (base_url + api_key_env) needs only that key.
    let own_key = agent.cfg.api_key_env.as_deref().and_then(|k| std::env::var(k).ok()).is_some_and(|v| !v.is_empty());
    if !own_key && !providers.iter().any(|p| model::has_key(p)) {
        let dir = std::env::current_dir()?;
        bail!(
            "no model API key is set. Set one of ANTHROPIC_API_KEY, OPENAI_API_KEY, AI_GATEWAY_API_KEY or YDC_API_KEY \
             (or a Bedrock or Gemini key) in your shell or in .env.local in {}, and egin picks a model for it",
            dir.display()
        );
    }
    for c in &agent.chosen {
        eprintln!("\x1b[2megin: {c}\x1b[0m");
    }
    model::Endpoint::resolve(app, &agent.cfg.model, Some(&agent.cfg))?;
    Ok(())
}

/// Run a session while printing its tokens and tool activity.
async fn run_printed(app: &Arc<App>, agent: &Arc<Agent>, sid: &str) -> Result<()> {
    let mut rx = app.events.subscribe();
    let main = sid.to_string();
    let printer = tokio::spawn(async move {
        // Status lines go to stderr; start them on a fresh line when streamed text is mid-line.
        let mut mid_line = false;
        let status = |mid: &mut bool, line: String| {
            if std::mem::take(mid) {
                println!();
            }
            eprintln!("\x1b[2m{line}\x1b[0m");
        };
        while let Ok(e) = rx.recv().await {
            let b = &e.body;
            let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
            match b["type"].as_str().unwrap_or("") {
                "token" if e.session == main => {
                    let t = s("text");
                    print!("{t}");
                    std::io::stdout().flush().ok();
                    mid_line = !t.ends_with('\n');
                }
                "tool_call" => status(&mut mid_line, format!("→ {} {}", s("tool"), b["args"].to_string().chars().take(200).collect::<String>())),
                // Tools the model provider runs itself (the router's web tools, Claude's server tools).
                "web_search" | "web_extract" | "web_fetch" | "code_execution" => {
                    let detail = [s("query"), s("url")].into_iter().find(|d| !d.is_empty()).unwrap_or_default();
                    status(&mut mid_line, format!("→ server {} {detail}", s("type")))
                }
                "elided" | "summarised" => status(&mut mid_line, format!("[context {}]", s("type"))),
                "done" | "error" if e.session == main => break,
                _ => {}
            }
        }
    });
    let out = run::run_session(app.clone(), agent.clone(), sid.to_string()).await;
    printer.await.ok();
    println!();
    let s = app.store.stats(sid)?;
    eprintln!(
        "\x1b[2m[session {sid} · {} model calls · {} tool calls · {} prompt tok ({} cached) · {} completion tok · ${:.4} · {:.1}s]\x1b[0m",
        s.model_calls,
        s.tool_calls,
        s.prompt_tokens,
        s.cached_tokens,
        s.completion_tokens,
        s.cost,
        s.wall_ms as f64 / 1000.0
    );
    out.map(|_| ())
}

/// `egin add channel [<docs url>]`: instructions for a coding agent to describe a channel for a
/// platform egin has no built-in channel for.
fn channel_guide(source: Option<&str>) -> String {
    let (provider, from) = match source {
        Some(s) => (s.to_string(), format!("Start from {s}, and follow its links to the webhook and API reference.")),
        None => ("the platform the user names".into(), "Ask the user which platform, then find its webhook documentation.".into()),
    };
    include_str!("channel_guide.md").replace("{{provider}}", &provider).replace("{{source}}", &from)
}

async fn connect(name: &str, dir: &Path) -> Result<()> {
    let dir = if !dir.join("agent.toml").exists() && dir.join("agent/agent.toml").exists() { dir.join("agent") } else { dir.to_path_buf() };
    let agent = Agent::load(&dir, &toml::Table::new())?;
    let names: Vec<&str> = agent.connections.iter().map(|c| c.name.as_str()).collect();
    let c = agent
        .connections
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| anyhow::anyhow!("no connection {name} in {} (it has: {})", dir.display(), names.join(", ")))?;
    if !c.oauth()? {
        bail!("connections/{name}.toml signs in with headers or env; add auth = \"oauth\" to sign in with egin connect");
    }
    let url = mcp::expand(c.url.as_deref().unwrap_or_default());
    let file = oauth::file(&agent.dir, name);
    let http = reqwest::Client::new();
    let t = oauth::login(&http, &file, &url, c.client_id.as_deref(), |link| {
        eprintln!("Opening your browser to sign in to {name}. If it doesn't open, visit:\n\n  {link}\n");
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(link).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    })
    .await?;
    println!(
        "Signed in to {name} at {}. The token is in {}{}.",
        t.issuer,
        file.display(),
        if t.refresh_token.is_some() { " and refreshes itself" } else { "" }
    );
    Ok(())
}

fn add(item: Option<String>, dir: &Path, force: bool) -> Result<()> {
    let Some(item) = item else {
        let mut kind = String::new();
        for it in egin::catalog::items() {
            let (k, _) = it.id.split_once('/').unwrap_or(("", ""));
            if k != kind {
                println!("\n{k}");
                kind = k.to_string();
            }
            println!("  {:<28} {}", it.id, it.description);
        }
        println!("\negin add <item> writes it into the agent folder. `egin docs` has the details.");
        return Ok(());
    };
    // An app with its agent in agent/ takes the agent folder, as `egin serve` does.
    let dir = if !dir.join("agent.toml").exists() && dir.join("agent/agent.toml").exists() { dir.join("agent") } else { dir.to_path_buf() };
    if !dir.join("agent.toml").exists() {
        bail!("{} is not an agent folder (no agent.toml); run egin init first, or pass --dir", dir.display());
    }
    let added = egin::catalog::add(&item, &dir, force)?;
    for p in &added.written {
        println!("wrote {}", dir.join(p).display());
    }
    for p in &added.kept {
        println!("kept  {} (it already exists; --force replaces it)", dir.join(p).display());
    }
    let missing: Vec<&String> = added.item.env.iter().filter(|v| std::env::var(v).map_or(true, |s| s.is_empty())).collect();
    if !missing.is_empty() {
        println!("\nset {} in your shell or in .env.local", missing.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !added.item.next.trim().is_empty() {
        println!("\n{}", added.item.next.trim());
    }
    if !added.item.docs.is_empty() {
        println!("\nmore: {}", added.item.docs);
    }
    Agent::load(&dir, &toml::Table::new()).map(|_| ())
}

static DOCS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/site/src/content/docs");

/// The docs pages, as (name, front matter, body), in the site's order.
fn pages() -> Vec<(String, toml::Table, String)> {
    let mut pages: Vec<_> = DOCS
        .files()
        .filter_map(|f| {
            let name = f.path().file_stem()?.to_string_lossy().to_string();
            let text = f.contents_utf8()?;
            let (front, body) = text.strip_prefix("---\n")?.split_once("\n---\n")?;
            // The front matter is YAML of plain `key: value` lines.
            let mut meta = toml::Table::new();
            for line in front.lines() {
                if let Some((k, v)) = line.split_once(": ") {
                    meta.insert(k.trim().into(), v.trim().parse::<i64>().map(toml::Value::from).unwrap_or_else(|_| v.trim().into()));
                }
            }
            Some((name, meta, body.trim().to_string()))
        })
        .collect();
    let group = |m: &toml::Table| ["Start", "Build", "Run", "Measure"].iter().position(|g| m.get("group").and_then(|v| v.as_str()) == Some(g)).unwrap_or(9);
    pages.sort_by_key(|(_, m, _)| (group(m), m.get("order").and_then(|v| v.as_integer()).unwrap_or(99)));
    pages
}

fn docs(page: Option<&str>) -> Result<()> {
    let pages = pages();
    let text = |m: &toml::Table, k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let Some(page) = page else {
        for (name, meta, _) in &pages {
            println!("{:<14} {}", if name == "index" { "intro" } else { name }, text(meta, "description"));
        }
        println!("\negin docs <page> prints one. They are also at https://egin.dev/docs");
        return Ok(());
    };
    let name = if page == "intro" { "index" } else { page };
    match pages.iter().find(|(n, _, _)| n == name) {
        Some((_, meta, body)) => {
            println!("# {}\n\n{}\n\n{body}", text(meta, "title"), text(meta, "description"));
            Ok(())
        }
        None => bail!("no docs page {page:?}; `egin docs` lists them"),
    }
}

fn info(dir: &Path) -> Result<()> {
    let a = Agent::load(dir, &toml::Table::new())?;
    // First what the agent will use and whether its keys are there; the full config follows.
    let set = |n: &str| std::env::var(n).is_ok_and(|v| !v.is_empty());
    let keys = |envs: &[&str]| match envs.iter().find(|n| set(n)) {
        Some(n) => format!("{n} is set"),
        None => format!("missing: set {}", envs.join(" or ")),
    };
    let c = &a.cfg;
    let provider = c.provider.clone().unwrap_or_else(|| model::route(&c.model).0.to_string());
    let model_keys = c.api_key_env.as_deref().map(|e| vec![e]).unwrap_or_else(|| model::key_envs(&provider).to_vec());
    println!("model:     {} on {provider} ({})", c.model, keys(&model_keys));
    let search_key = match c.search.as_str() {
        "you" => "YDC_API_KEY",
        "parallel" => "PARALLEL_API_KEY",
        "firecrawl" => "FIRECRAWL_API_KEY",
        "context.dev" => "CONTEXT_DEV_API_KEY",
        "brave" => "BRAVE_API_KEY",
        _ => "SEARXNG_URL",
    };
    match c.web.as_str() {
        "client" => println!("web:       {} search ({})", c.search, keys(&[search_key])),
        "server" => println!("web:       the model provider's own search"),
        _ => println!("web:       off"),
    }
    println!("sandbox:   {}", c.sandbox);
    for chosen in &a.chosen {
        println!("chosen:    {chosen}");
    }
    println!("\n{}", toml::to_string(&a.cfg)?);
    let names = |v: Vec<&str>| if v.is_empty() { "-".to_string() } else { v.join(", ") };
    let tools = egin::tools::specs(&a);
    println!("tools:     {}", names(tools.iter().map(|t| t["function"]["name"].as_str().or(t["type"].as_str()).unwrap_or("")).collect()));
    println!("skills:    {}", names(a.skills.iter().map(|s| s.name.as_str()).collect()));
    println!("subagents: {}", names(a.subagents.iter().map(String::as_str).collect()));
    println!("schedules: {}", names(a.schedules.iter().map(|s| s.name.as_str()).collect()));
    println!("mcp:       {}", names(a.connections.iter().map(|c| c.name.as_str()).collect()));
    println!("channels:  {}", names(a.channels.keys().map(String::as_str).collect()));
    println!(
        "traces to: {}",
        names(
            a.exporters
                .iter()
                .map(|e| e.name.as_str())
                .chain(
                    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                        .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"))
                        .ok()
                        .map(|_| "OTEL_* environment")
                )
                .collect()
        )
    );
    println!("workspace: {}", a.workspace.display());
    Ok(())
}

async fn models() -> Result<()> {
    let app = open(Path::new("."), Approve::Auto)?;
    let mut models: Vec<_> = model::fetch_models(&app).await?.into_iter().collect();
    models.sort_by(|a, b| a.0.cmp(&b.0));
    println!("{:<24} {:>10} {:>9} {:>9} {:>9}", "model", "context", "in $/M", "cached", "out $/M");
    for (id, m) in models {
        println!("{id:<24} {:>10} {:>9} {:>9} {:>9}", m.context, m.input, m.cached, m.output);
    }
    Ok(())
}

async fn chat(agent: Arc<Agent>, app: Arc<App>) -> Result<()> {
    let sid = run::new_session(&app, &agent)?;
    loop {
        eprint!("\n> ");
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 || line.trim() == "exit" {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        run::send(&app, &sid, line.trim())?;
        if let Err(e) = run_printed(&app, &agent, &sid).await {
            eprintln!("error: {e:#}");
        }
    }
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.cmd {
            Cmd::Init { dir, template } => {
                agent::init_template(&dir, &template)?;
                match template.as_str() {
                    "web" => println!("created {}\nnext: cd {} && npm install && npm run dev", dir.display(), dir.display()),
                    _ => println!("created {}\nnext: egin run {} \"say hello\"", dir.display(), dir.display()),
                }
            }
            Cmd::Add { item, source, .. } if item.as_deref() == Some("channel") => print!("{}", channel_guide(source.as_deref())),
            Cmd::Add { item, dir, force, .. } => add(item, &dir, force)?,
            Cmd::Connect { name, dir } => connect(&name, &dir).await?,
            Cmd::Docs { page } => docs(page.as_deref())?,
            Cmd::Info { dir } => info(&dir)?,
            Cmd::Trajectory { dir, session, out } => {
                let app = open(&dir, Approve::Auto)?;
                let sid = match session {
                    Some(s) => s,
                    None => app
                        .store
                        .list_sessions(None, 1)?
                        .first()
                        .and_then(|s| s["id"].as_str().map(str::to_string))
                        .context("this agent has no sessions yet")?,
                };
                let t = serde_json::to_string_pretty(&egin::atif::export(&app, &sid)?)?;
                match out {
                    Some(path) => std::fs::write(path, t + "\n")?,
                    None => println!("{t}"),
                }
            }
            Cmd::Models => models().await?,
            Cmd::Run { dir, prompt, yes, session, set, schema } => {
                let agent = Arc::new(Agent::load(&dir, &overrides(&set)?)?);
                let app = open(&agent.dir, if yes { Approve::Auto } else { Approve::Stdin })?;
                ready(&app, &agent)?;
                let sid = match session {
                    Some(s) if app.store.status(&s)?.is_some() => s,
                    Some(s) => bail!("no session {s}"),
                    None => run::new_session(&app, &agent)?,
                };
                if let Some(path) = schema {
                    let s: serde_json::Value =
                        anyhow::Context::with_context(serde_json::from_str(&std::fs::read_to_string(&path)?), || path.display().to_string())?;
                    output::set(&app, &sid, Some(&s))?;
                }
                match prompt {
                    Some(p) if p == "-" => run::send(&app, &sid, &std::io::read_to_string(std::io::stdin())?)?,
                    Some(p) => run::send(&app, &sid, &p)?,
                    None if app.store.messages(&sid)?.len() < 2 => bail!("a prompt is required"),
                    None => {}
                }
                run_printed(&app, &agent, &sid).await?;
            }
            Cmd::Chat { dir, yes, set } => {
                let set = overrides(&set)?;
                let agent = Arc::new(Agent::load(&dir, &set)?);
                // The fullscreen UI answers approvals itself; piped, the plain loop asks on stdin.
                let tty = std::io::IsTerminal::is_terminal(&std::io::stdout()) && std::io::IsTerminal::is_terminal(&std::io::stdin());
                let approve = match (yes, tty) {
                    (true, _) => Approve::Auto,
                    (false, true) => Approve::Api,
                    (false, false) => Approve::Stdin,
                };
                let app = open(&agent.dir, approve)?;
                ready(&app, &agent)?;
                if tty {
                    crate::tui::chat(agent, app, set).await?;
                } else {
                    chat(agent, app).await?;
                }
            }
            Cmd::Step { dir } => {
                let agent = Arc::new(Agent::load(&dir, &toml::Table::new())?);
                // An approval or a question stops the step: the host brings the answer with a later one.
                let app = App::stateless(Approve::Host)?;
                let mut first = String::new();
                std::io::stdin().read_line(&mut first)?;
                let input: serde_json::Value = serde_json::from_str(&first).context("the session on stdin is not one line of JSON")?;
                // A host that keeps stdin open can cancel the turn by writing another line.
                let (tx, rx) = tokio::sync::oneshot::channel();
                std::thread::spawn(move || {
                    if matches!(std::io::stdin().read_line(&mut String::new()), Ok(n) if n > 0) {
                        let _ = tx.send(());
                    }
                });
                let cancel = async {
                    if rx.await.is_err() {
                        std::future::pending::<()>().await;
                    }
                };
                // One JSON value a line. While the step runs: token deltas as `live`, and stored events
                // as `event`, ahead of the `result` that carries them again. Then the `result`.
                let line = |v: serde_json::Value| {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{v}");
                    let _ = out.flush();
                };
                let result =
                    agent::step::step(&app, &agent, &input, cancel, |e, stored| line(serde_json::json!({ if stored { "event" } else { "live" }: e }))).await?;
                line(serde_json::json!({"result": result}));
            }
            Cmd::Serve { dir, port, host } => {
                let agent = Arc::new(Agent::load(&dir, &toml::Table::new())?);
                let app = open(&agent.dir, Approve::Api)?;
                ready(&app, &agent)?;
                // Hosts that run containers (Fly, Railway, Render, Cloud Run) name the port in $PORT.
                let port = port.or_else(|| std::env::var("PORT").ok()?.parse().ok()).unwrap_or(8787);
                server::serve(app, agent, &host, port).await?;
            }
            Cmd::Eval { dir, variant, model, split, task, jobs, set } => {
                let dir = dir.canonicalize()?;
                let app = open(&dir, Approve::Auto)?;
                let opts = eval::Opts { variants: variant, models: model, split, only: task, jobs, overrides: overrides(&set)?, results: None };
                let outcomes = eval::run_evals(&app, &dir, &opts).await?;
                print!("{}", eval::table(&outcomes));
                if outcomes.iter().any(|o| !o.pass) {
                    std::process::exit(1);
                }
            }
            Cmd::Evolve { dir, model, proposer, rounds, jobs } => {
                evolve::check_model(&model)?;
                let dir = dir.canonicalize()?;
                let app = open(&dir, Approve::Auto)?;
                let summary = evolve::evolve(&app, &dir, &model, proposer.as_deref().unwrap_or(&model), rounds, jobs).await?;
                println!("{}", serde_json::to_string_pretty(&summary)?);
            }
            Cmd::Report { results, title, method, out } => {
                let outcomes = report::load(&results)?;
                let method = method.map(std::fs::read_to_string).transpose()?.unwrap_or_default();
                std::fs::write(&out, report::html(&title, &method, &outcomes))?;
                println!("wrote {} ({} runs)", out.display(), outcomes.len());
            }
            Cmd::Bench { tasks, agent, task, subset, variant, model, oracle, jobs, parallel, out, sandbox } => {
                let agent_dir = agent.canonicalize()?;
                let app = open(&agent_dir, Approve::Auto)?;
                let mut names = task;
                if let Some(f) = subset {
                    let text = std::fs::read_to_string(f)?;
                    names.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from));
                }
                let results = out.unwrap_or_else(|| agent_dir.join("results").join(egin::new_id()));
                let opts = bench::BenchOpts {
                    tasks,
                    agent_dir,
                    names,
                    variants: variant,
                    models: model,
                    oracle,
                    jobs,
                    parallel,
                    results,
                    daytona: sandbox == "daytona",
                };
                let outcomes = bench::run_bench(&app, &opts).await?;
                print!("{}", eval::table(&outcomes));
            }
        }
        Ok(())
    }
}
