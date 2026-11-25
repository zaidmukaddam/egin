//! An agent is a folder: agent.toml, instructions.md, memory/, skills/, tools/,
//! middleware/, subagents/, schedules/, evals/ and a workspace/.

pub mod hooks;
pub mod output;
pub mod run;
pub mod step;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// agent.toml. Every design choice is a switch so evals can ablate it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: String,
    /// How to reach `model`: "openai" (any chat-completions endpoint), "anthropic" (the Messages
    /// API) or "bedrock" (Claude in Amazon Bedrock). Unset: chosen from the model id.
    pub provider: Option<String>,
    /// The provider's endpoint, when not its default.
    pub base_url: Option<String>,
    /// The environment variable holding the provider's API key, when not its default.
    pub api_key_env: Option<String>,
    pub reasoning_effort: Option<String>,
    /// Other models a client (the web chat, the HTTP API) may pick for a session, besides `model`.
    pub models: Vec<String>,
    pub max_tokens: Option<u64>,
    /// Overrides the limit read from the router's /v1/models.
    pub context_limit: Option<u64>,
    /// Whether the model can see images. Unset: what the router's /v1/models says.
    pub vision: Option<bool>,
    /// The model that transcribes audio for read_file. Unset: the session's model if it takes audio,
    /// else the cheapest router model that does.
    pub audio_model: Option<String>,
    /// "typed" adds read_file/write_file/edit_file; "bash" leaves only bash.
    pub tool_style: String,
    /// "client" gives the agent web_search and web_fetch (run by `search`); "server" lets the model
    /// provider run its own web tools; "off".
    pub web: String,
    /// Who runs `web_search` and `web_fetch` with web = "client": "you" (You.com), "brave",
    /// "parallel", "firecrawl", "context.dev" or "searxng" (keys from YDC_API_KEY, BRAVE_API_KEY,
    /// PARALLEL_API_KEY, FIRECRAWL_API_KEY, CONTEXT_DEV_API_KEY; SEARXNG_URL).
    pub search: String,
    /// "highlights" or "snippets".
    pub search_mode: String,
    pub search_count: u32,
    pub freshness: Option<String>,
    pub include_domains: Vec<String>,
    pub exclude_domains: Vec<String>,
    /// Web results and fetched pages containing any of these strings (case-sensitive) are withheld from
    /// the model: for evals whose answers are republished on sites a domain list cannot keep up with.
    pub exclude_terms: Vec<String>,
    /// Withhold web results and pages that quote the session's task (twelve or more of its words in a
    /// row, outside what the task itself puts in quotation marks). For evals: a page that reproduces
    /// a benchmark question usually prints its answer too.
    pub exclude_echo: bool,
    /// Allow several tool calls per turn and run the safe ones concurrently.
    pub batching: bool,
    pub planning: bool,
    /// Truncate long tool output as it enters the context.
    pub compaction: bool,
    pub tool_output_max: usize,
    /// Characters of a page, document or search result shown at once (read_file, web_fetch,
    /// web_search). Longer ones come a part at a time.
    pub page_max: usize,
    /// Replace old tool output with a stub once the context passes `elide_at` of the limit.
    pub elision: bool,
    pub elide_at: f64,
    pub keep_recent: usize,
    /// Elision and summarisation treat the context window as at most this many tokens; 0 uses the full window.
    pub context_focus: u64,
    /// Summarise older turns once the context passes `summarise_at` of the limit.
    pub summarisation: bool,
    pub summarise_at: f64,
    /// "ask" gates every unsafe tool call behind an approval; "once" asks the first time each tool is
    /// called in a session and remembers a yes; "never" runs them.
    pub approval: String,
    /// A message sent while the session runs: "steer" hands it to the model at its next step;
    /// "queue" holds it until the current answer is done, then runs it as the next turn.
    pub mid_run: String,
    /// Adds `ask_user`, which pauses the run until someone answers the question over the HTTP API.
    pub ask: bool,
    pub max_turns: u64,
    /// Spend limit per session in USD; 0 disables it.
    pub max_cost: f64,
    /// Wall-clock seconds a run has, when something outside egin will stop it (a benchmark's time
    /// limit, a job runner). At 60% and 85% of it the model is told how long is left; bash waits and
    /// model calls are capped so one of them cannot eat the rest. 0 disables it.
    pub deadline: u64,
    /// Every this many model calls in an unattended run, the model is asked to finish if the work is in
    /// place and to name what is still unmet if it is not. 0 disables it.
    pub check_in: u64,
    /// Longest a bash call waits; the model can ask for less with `timeout_secs`.
    pub bash_timeout: u64,
    /// Default wait for a bash call before it moves to the background (or is killed).
    pub bash_wait: u64,
    /// A command still running after its wait keeps running as a background job the model can
    /// check and stop with later bash calls. Off: it is killed at the wait.
    pub bash_background: bool,
    /// "auto" (microsandbox where the host supports it, else local), "microsandbox", "daytona"
    /// (cloud, needs DAYTONA_API_KEY), "docker" / "docker-cloud" (Docker Sandboxes via the
    /// signed-in `sbx` CLI) or "local".
    pub sandbox: String,
    pub image: String,
    pub cpus: u8,
    pub memory_mb: u32,
    /// Seconds without activity before the session's VM stops; it restarts on the next tool call.
    pub idle_timeout: u64,
    /// Working directory inside the sandbox; defaults to /workspace (VM) or the workspace folder (local).
    pub workdir: Option<String>,
    /// Egress for the VM: [] none, ["*"] all, or domains (".example.com" for subdomains).
    pub network: Vec<String>,
    /// Host env vars handed to the VM as placeholders, substituted only on requests to `host`.
    pub secrets: Vec<Secret>,
    /// MCP tools: "listed" sends every tool to the model; "deferred" keeps them out of the prompt
    /// behind tool_search / tool_call; "auto" defers once the connections expose more than 20.
    pub mcp_tools: String,
    /// write_file refuses to overwrite a file this session has not read with read_file, or that
    /// changed since it was read.
    pub read_before_write: bool,
    /// Adds the `memory` tool, which edits memory/notes.md up to this many bytes; 0 disables it.
    pub memory_max: usize,
    /// Adds `search_files`: a full-text and metadata index over the workspace.
    pub index: bool,
    /// Before a run ends, send the model back once to check its work against the task: each
    /// requirement, edge cases, stray files. For unattended task runs; off for chat.
    pub review: bool,
    /// Adds the `helper` tool: a fresh copy of the agent, on the same sandbox and files, answers one prompt.
    pub helper: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub name: String,
    pub host: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: "kimi-k3.1-pro".into(),
            provider: None,
            base_url: None,
            api_key_env: None,
            reasoning_effort: None,
            models: vec![],
            max_tokens: None,
            context_limit: None,
            vision: None,
            audio_model: None,
            tool_style: "typed".into(),
            web: "client".into(),
            search: "you".into(),
            search_mode: "highlights".into(),
            search_count: 5,
            freshness: None,
            include_domains: vec![],
            exclude_domains: vec![],
            exclude_terms: vec![],
            exclude_echo: false,
            batching: true,
            planning: false,
            compaction: true,
            // eve showed FrontierHarness outputs whole (p99 23k chars, max 53k); cut at 10k, egin re-read and re-ran
            // to see the rest.
            tool_output_max: 50_000,
            page_max: 30_000,
            elision: true,
            elide_at: 0.5,
            keep_recent: 6,
            context_focus: 256_000,
            summarisation: true,
            summarise_at: 0.75,
            approval: "ask".into(),
            mid_run: "steer".into(),
            ask: false,
            max_turns: 60,
            max_cost: 0.0,
            deadline: 0,
            check_in: 0,
            bash_timeout: 600,
            bash_wait: 60,
            bash_background: true,
            sandbox: "auto".into(),
            image: "python:3.13-slim".into(),
            cpus: 2,
            memory_mb: 2048,
            idle_timeout: 600,
            workdir: None,
            network: vec!["*".into()],
            secrets: vec![],
            mcp_tools: "auto".into(),
            read_before_write: true,
            memory_max: 0,
            index: false,
            review: false,
            helper: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// Locked tools that loading this skill makes available for the rest of the session.
    pub unlocks: Vec<String>,
}

/// tools/<name>.json describes it; tools/<name>[.ext] is the executable.
#[derive(Debug, Clone)]
pub struct CustomTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub safe: bool,
    pub exe: PathBuf,
    /// Re-run after a crash instead of reported as unknown; the script skips the steps its journal
    /// ($EGIN_STEPS) says are done.
    pub durable: bool,
    /// Hidden from the model until a tool or skill that `unlocks` it succeeds in the session.
    pub locked: bool,
    /// Locked tools this one makes available when it succeeds.
    pub unlocks: Vec<String>,
}

fn names(list: &str) -> Vec<String> {
    list.split(',').map(|s| s.trim().trim_matches(['[', ']', '"']).trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// schedules/<name>.toml: `prompt` plus either `cron` (5 fields, local time) or `every_secs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    #[serde(skip)]
    pub name: String,
    pub cron: Option<String>,
    pub every_secs: Option<u64>,
    pub prompt: String,
}

impl Schedule {
    /// Time from `now` until the next run.
    pub fn next(&self, now: chrono::DateTime<chrono::Local>) -> Result<std::time::Duration> {
        if let Some(expr) = &self.cron {
            let cron: croner::Cron = expr.parse().with_context(|| format!("schedule {}: bad cron {expr:?}", self.name))?;
            let at = cron.find_next_occurrence(&now, false).with_context(|| format!("schedule {}: no next time for {expr:?}", self.name))?;
            return Ok((at - now).to_std().unwrap_or_default());
        }
        match self.every_secs {
            Some(s) => Ok(std::time::Duration::from_secs(s.max(1))),
            None => bail!("schedule {} needs `cron` or `every_secs`", self.name),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub dir: PathBuf,
    pub workspace: PathBuf,
    pub cfg: Config,
    pub instructions: String,
    pub memory: String,
    pub skills: Vec<Skill>,
    pub tools: Vec<CustomTool>,
    pub subagents: Vec<String>,
    pub schedules: Vec<Schedule>,
    pub connections: Vec<crate::tools::mcp::Connection>,
    /// channels/<name>.toml, read by the HTTP server (webhook, slack).
    pub channels: HashMap<String, toml::Table>,
    /// instrumentation/<name>.toml: where spans go, besides the OTEL_* environment.
    pub exporters: Vec<crate::telemetry::Exporter>,
    /// Settings egin chose because agent.toml left them out, each with why, for the CLI to print once.
    pub chosen: Vec<String>,
    /// What the caller set on top of agent.toml (`--set`, `--model`, an eval variant, a session's model).
    /// A subagent inherits each one its own agent.toml leaves out.
    pub overrides: toml::Table,
}

/// The model to use when agent.toml names none: the router's default with a You.com key, otherwise
/// a good model on whichever provider has a key. None when no key is set (the default stays, and
/// the startup check says which key to set).
fn default_model() -> Option<(&'static str, &'static str)> {
    use crate::model::has_key;
    if has_key("you") {
        return None;
    }
    [
        ("anthropic", "anthropic/claude-sonnet-5-5"),
        ("openai", "openai/gpt-6-luna"),
        ("gateway", "anthropic/claude-sonnet-5.5"),
        ("bedrock", "us.anthropic.claude-sonnet-5-5"),
    ]
    .into_iter()
    .find(|(p, _)| has_key(p))
    .map(|(p, m)| (m, crate::model::key_envs(p)[0]))
}

/// Web search when agent.toml sets neither `web` nor `search`: You.com with its key, else the first
/// search provider with a key, else the model provider's own search where it has one, else none.
fn default_web(model: &str) -> Option<(&'static str, &'static str, String)> {
    let set = |n: &str| std::env::var(n).is_ok_and(|v| !v.is_empty());
    // A router model needs the You.com key anyway, and that key also searches.
    if set("YDC_API_KEY") || crate::model::route(model).0 == "you" {
        return None;
    }
    let found = [
        ("parallel", "PARALLEL_API_KEY"),
        ("firecrawl", "FIRECRAWL_API_KEY"),
        ("context.dev", "CONTEXT_DEV_API_KEY"),
        ("brave", "BRAVE_API_KEY"),
        ("searxng", "SEARXNG_URL"),
    ]
    .into_iter()
    .find(|(_, k)| set(k));
    if let Some((search, key)) = found {
        return Some(("client", search, format!("search = \"{search}\" ({key} is set)")));
    }
    match crate::model::route(model).0 {
        "anthropic" | "gateway" => Some(("server", "you", "web = \"server\": the model provider's own web search (no search key is set)".into())),
        _ => Some((
            "off",
            "you",
            "web = \"off\": no search key is set (YDC_API_KEY, PARALLEL_API_KEY, FIRECRAWL_API_KEY, CONTEXT_DEV_API_KEY or BRAVE_API_KEY)".into(),
        )),
    }
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

/// Tools run as programs, so a script saved without the executable bit gets it.
fn executable(p: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(p)?.permissions();
        if perm.mode() & 0o111 == 0 {
            perm.set_mode(perm.mode() | 0o755);
            fs::set_permissions(p, perm)?;
        }
    }
    Ok(())
}

/// A tool declared in a script's opening comments (`#`, `//` or `--`), so one file is the whole tool:
///
/// ```text
/// #!/usr/bin/env python3
/// # description: Check whether a URL answers, and how fast.
/// # safe: true
/// # param url (string, required): The URL to check
/// # param timeout (number): Seconds to wait
/// ```
///
/// Types are string, number, integer, boolean, array (of strings) and object. A script with no
/// `description:` line is not a tool (a helper the others call). The arguments arrive as JSON on stdin.
fn header_tool(path: &Path, text: &str) -> Result<Option<CustomTool>> {
    let mut description = None;
    let (mut safe, mut durable, mut locked, mut unlocks) = (false, false, false, vec![]);
    let (mut properties, mut required) = (serde_json::Map::new(), vec![]);
    for line in text.lines().skip_while(|l| l.starts_with("#!")) {
        let line = line.trim();
        let Some(body) = ["#", "//", "--"].iter().find_map(|c| line.strip_prefix(c)) else {
            if line.is_empty() {
                continue;
            }
            break;
        };
        let body = body.trim();
        if let Some(d) = body.strip_prefix("description:") {
            description = Some(d.trim().to_string());
        } else if let Some(s) = body.strip_prefix("safe:") {
            safe = s.trim() == "true";
        } else if let Some(s) = body.strip_prefix("durable:") {
            durable = s.trim() == "true";
        } else if let Some(s) = body.strip_prefix("locked:") {
            locked = s.trim() == "true";
        } else if let Some(s) = body.strip_prefix("unlocks:") {
            unlocks = names(s);
        } else if let Some(p) = body.strip_prefix("param ") {
            let (head, about) = p.split_once("):").map(|(h, a)| (h, a.trim())).unwrap_or((p.trim_end_matches(')'), ""));
            let Some((name, flags)) = head.split_once('(') else { bail!("`param {p}`: write it as `param name (type, required): what it is`") };
            let name = name.trim();
            let mut flags = flags.split(',').map(str::trim);
            let ty = flags.next().unwrap_or("string");
            let mut schema = match ty {
                "string" | "number" | "integer" | "boolean" | "object" => serde_json::json!({"type": ty}),
                "array" => serde_json::json!({"type": "array", "items": {"type": "string"}}),
                t => bail!("param {name}: unknown type {t:?} (use string, number, integer, boolean, array or object)"),
            };
            if !about.is_empty() {
                schema["description"] = about.into();
            }
            if flags.any(|f| f == "required") {
                required.push(name.to_string());
            }
            properties.insert(name.into(), schema);
        }
    }
    Ok(description.map(|description| CustomTool {
        name: stem(path),
        description,
        parameters: serde_json::json!({"type": "object", "properties": properties, "required": required}),
        safe,
        exe: path.to_path_buf(),
        durable,
        locked,
        unlocks,
    }))
}

impl Agent {
    /// The overrides a subagent in `dir` runs with: this agent's, minus the keys the subagent's own
    /// agent.toml sets, so `--model` reaches a subagent that names no model and spares one that does.
    pub fn inherited(&self, dir: &Path) -> toml::Table {
        let own: toml::Table = fs::read_to_string(dir.join("agent.toml")).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        self.overrides.iter().filter(|(k, _)| !own.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn load(dir: &Path, overrides: &toml::Table) -> Result<Agent> {
        let dir = dir.canonicalize().with_context(|| format!("no agent folder at {}", dir.display()))?;
        // An app with its agent in agent/ (the web template) can be named by the app's own folder.
        let dir = if !dir.join("agent.toml").exists() && dir.join("agent/agent.toml").exists() { dir.join("agent") } else { dir };
        let mut table: toml::Table = match fs::read_to_string(dir.join("agent.toml")) {
            Ok(s) => toml::from_str(&s).context("agent.toml")?,
            Err(_) => toml::Table::new(),
        };
        table.extend(overrides.clone());
        // What agent.toml leaves out is chosen from the keys that are set, so one key is enough to start.
        let mut chosen = vec![];
        if !table.contains_key("model") {
            if let Some((model, key)) = default_model() {
                table.insert("model".into(), model.into());
                chosen.push(format!("model = \"{model}\" ({key} is set)"));
            }
        }
        if !table.contains_key("web") && !table.contains_key("search") {
            let model = table.get("model").and_then(|m| m.as_str()).unwrap_or(&Config::default().model).to_string();
            if let Some((web, search, why)) = default_web(&model) {
                table.insert("web".into(), web.into());
                table.insert("search".into(), search.into());
                chosen.push(why);
            }
        }
        // A model profile (profiles/<model>.toml) sets per-model defaults that agent.toml and
        // overrides still win over; `profile = false` turns it off for ablations.
        let use_profile = table.remove("profile").and_then(|v| v.as_bool()).unwrap_or(true);
        let model = table.get("model").and_then(|m| m.as_str()).unwrap_or(&Config::default().model).to_string();
        let mut profile_text = String::new();
        if use_profile {
            if let Ok(s) = fs::read_to_string(dir.join("profiles").join(format!("{model}.toml"))) {
                let mut merged: toml::Table = toml::from_str(&s).with_context(|| format!("profiles/{model}.toml"))?;
                merged.extend(table);
                table = merged;
            }
            profile_text = fs::read_to_string(dir.join("profiles").join(format!("{model}.md"))).unwrap_or_default();
        }
        let mut cfg: Config = toml::Value::Table(table).try_into().context("agent.toml")?;
        match cfg.sandbox.as_str() {
            "auto" if crate::sandbox::vm_supported() => cfg.sandbox = "microsandbox".into(),
            "auto" => {
                eprintln!("egin: no hardware virtualization on this host; tools run locally (approval = \"{}\")", cfg.approval);
                cfg.sandbox = "local".into();
            }
            "microsandbox" if !crate::sandbox::vm_supported() => bail!("sandbox = \"microsandbox\" needs KVM (Linux) or Apple Silicon"),
            "microsandbox" | "local" | "daytona" | "daytona-macos" | "e2b" | "vercel" | "docker" | "docker-cloud" => {}
            s => bail!("unknown sandbox {s:?}: use auto, microsandbox, daytona, daytona-macos, e2b, vercel, docker, docker-cloud or local"),
        }
        if !matches!(cfg.approval.as_str(), "ask" | "once" | "never") {
            bail!("unknown approval {:?}: use ask, once or never", cfg.approval);
        }
        if !matches!(cfg.mid_run.as_str(), "steer" | "queue") {
            bail!("unknown mid_run {:?}: use steer or queue", cfg.mid_run);
        }
        // Tools that run on the host see its real environment, so a secret could not be kept from them.
        if cfg.sandbox == "local" && !cfg.secrets.is_empty() {
            bail!("`secrets` needs sandbox = \"microsandbox\": with tools running on this host, they would see the real values");
        }

        let memory = entries(&dir.join("memory"))
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .filter_map(|p| fs::read_to_string(p).ok())
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut skills = vec![];
        for p in entries(&dir.join("skills")) {
            let Ok(text) = fs::read_to_string(p.join("SKILL.md")) else { continue };
            // Frontmatter is optional: `---\ndescription: ...\n---\nbody`.
            let (front, body) = match text.strip_prefix("---").and_then(|t| t.split_once("\n---")) {
                Some((f, b)) => (f, b.trim_start()),
                None => ("", text.as_str()),
            };
            let description = front.lines().find_map(|l| l.strip_prefix("description:")).unwrap_or("").trim();
            let unlocks = front.lines().find_map(|l| l.strip_prefix("unlocks:")).map(names).unwrap_or_default();
            skills.push(Skill { name: stem(&p), description: description.into(), body: body.into(), unlocks });
        }

        let mut tools = vec![];
        let tool_files = entries(&dir.join("tools"));
        for p in tool_files.iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
            let spec: Value = serde_json::from_str(&fs::read_to_string(p)?).with_context(|| p.display().to_string())?;
            if let Some(k) = spec
                .as_object()
                .and_then(|o| o.keys().find(|k| !matches!(k.as_str(), "description" | "safe" | "parameters" | "durable" | "locked" | "unlocks")))
            {
                bail!("{}: unknown key {k:?} (a tool takes description, safe, parameters, durable, locked and unlocks)", p.display());
            }
            let name = stem(p);
            let Some(exe) = tool_files.iter().find(|q| *q != p && stem(q) == name) else { bail!("tool {name} has no executable next to {}", p.display()) };
            executable(exe)?;
            tools.push(CustomTool {
                description: spec["description"].as_str().unwrap_or("").into(),
                parameters: spec.get("parameters").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                safe: spec["safe"].as_bool().unwrap_or(false),
                exe: exe.clone(),
                durable: spec["durable"].as_bool().unwrap_or(false),
                locked: spec["locked"].as_bool().unwrap_or(false),
                unlocks: spec["unlocks"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect(),
                name,
            });
        }
        // A tool can also be one script that describes itself in its opening comments (see `header_tool`).
        let described = |n: &str| tool_files.iter().any(|q| q.extension().is_some_and(|e| e == "json") && stem(q) == n);
        for p in tool_files.iter().filter(|p| p.is_file() && p.extension().is_none_or(|e| e != "json") && !described(&stem(p))) {
            let Ok(text) = fs::read_to_string(p) else { continue };
            if let Some(t) = header_tool(p, &text).with_context(|| p.display().to_string())? {
                executable(p)?;
                tools.push(t);
            }
        }
        // `unlocks` must name locked tools: anything else is a typo that would never take effect.
        let unlockers = tools.iter().map(|t| (&t.name, &t.unlocks)).chain(skills.iter().map(|s| (&s.name, &s.unlocks)));
        for (who, list) in unlockers {
            if let Some(n) = list.iter().find(|n| !tools.iter().any(|t| &t.name == *n && t.locked)) {
                bail!("{who} unlocks {n:?}, which is not a locked tool in tools/ (give it `locked: true`)");
            }
        }

        let tomls = |sub: &str| entries(&dir.join(sub)).into_iter().filter(|p| p.extension().is_some_and(|e| e == "toml"));
        let mut schedules = vec![];
        for p in tomls("schedules") {
            let mut s: Schedule = toml::from_str(&fs::read_to_string(&p)?).with_context(|| p.display().to_string())?;
            s.name = stem(&p);
            s.next(chrono::Local::now())?; // reject a bad schedule at load time
            schedules.push(s);
        }
        let mut connections = vec![];
        for p in tomls("connections") {
            let mut c: crate::tools::mcp::Connection = toml::from_str(&fs::read_to_string(&p)?).with_context(|| p.display().to_string())?;
            c.name = stem(&p);
            c.oauth()?;
            connections.push(c);
        }
        let mut channels = HashMap::new();
        for p in tomls("channels") {
            let table: toml::Table = toml::from_str(&fs::read_to_string(&p)?).with_context(|| p.display().to_string())?;
            crate::server::channels::check(&stem(&p), &table)?;
            channels.insert(stem(&p), table);
        }
        let mut exporters = vec![];
        for p in tomls("instrumentation") {
            let mut e: crate::telemetry::Exporter = toml::from_str(&fs::read_to_string(&p)?).with_context(|| p.display().to_string())?;
            e.name = stem(&p);
            exporters.push(e);
        }

        let workspace = dir.join("workspace");
        fs::create_dir_all(&workspace)?;
        let mut instructions = fs::read_to_string(dir.join("instructions.md")).unwrap_or_default();
        if !profile_text.trim().is_empty() {
            instructions = format!("{}\n\n{}", instructions.trim_end(), profile_text.trim());
        }
        Ok(Agent {
            instructions,
            subagents: entries(&dir.join("subagents")).iter().filter(|p| p.is_dir()).map(|p| stem(p)).collect(),
            workspace,
            cfg,
            memory,
            skills,
            tools,
            schedules,
            connections,
            channels,
            exporters,
            dir,
            chosen,
            overrides: overrides.clone(),
        })
    }

    /// Built once per session and never rewritten, so the prompt prefix stays cacheable.
    pub fn system_prompt(&self) -> String {
        let mut s = self.instructions.trim().to_string();
        if !self.memory.trim().is_empty() {
            s += &format!("\n\n# Memory\n{}", self.memory.trim());
        }
        if !self.skills.is_empty() {
            s += "\n\n# Skills\nLoad a skill with the `skill` tool before doing work it covers.";
            for k in &self.skills {
                s += &format!("\n- {}: {}", k.name, k.description);
            }
        }
        if !self.subagents.is_empty() {
            s += &format!("\n\n# Subagents\nDelegate self-contained work with the `task` tool: {}.", self.subagents.join(", "));
        }
        s += &format!("\n\n# Environment\nWorking directory: {}. Commands and relative paths run there.", crate::sandbox::workdir(self));
        if self.cfg.deadline > 0 {
            let minutes = (self.cfg.deadline as f64 / 60.0).round().max(1.0);
            s += &format!(
                "\nThis task has a time limit of about {minutes} minute{}, after which you are stopped. Get a working result in place early, then improve it.",
                if minutes == 1.0 { "" } else { "s" }
            );
        }
        if self.cfg.batching {
            s += "\nWhen several independent tool calls are needed, issue them together in one turn.";
        }
        if self.cfg.memory_max > 0 {
            s += "\nSave facts worth keeping across sessions with the `memory` tool; they appear under # Memory next time.";
        }
        if self.cfg.planning {
            s += "\nFor multi-step work, record a short plan with the `plan` tool first and update it as steps finish.";
        }
        s
    }
}

/// The starter templates: each folder in templates/ is one, embedded whole, so adding a template or a
/// file to one is only a matter of adding files.
static TEMPLATES: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Names `egin init --template` accepts, besides a path to a folder.
pub fn template_names() -> Vec<String> {
    let mut names: Vec<String> = TEMPLATES.dirs().map(|d| d.path().display().to_string()).collect();
    names.sort();
    names
}

/// Write a starter agent folder.
pub fn init(dir: &Path) -> Result<()> {
    init_template(dir, "basic")
}

/// Copy a template (a built-in name, or a path to any folder laid out as an agent or an app with
/// agent/) into `dir`, and name the model to use for the key that is set.
pub fn init_template(dir: &Path, template: &str) -> Result<()> {
    if dir.join("agent.toml").exists() || dir.join("agent/agent.toml").exists() {
        bail!("{} already holds an agent", dir.display());
    }
    let mut files: Vec<(PathBuf, Vec<u8>)> = vec![];
    if let Some(root) = TEMPLATES.get_dir(template) {
        fn walk(d: &include_dir::Dir, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            out.extend(d.files().map(|f| (f.path().strip_prefix(root).unwrap_or(f.path()).to_path_buf(), f.contents().to_vec())));
            // A template run in place leaves its session store behind; it is never part of the template.
            d.dirs().filter(|s| !s.path().ends_with(".egin")).for_each(|s| walk(s, root, out));
        }
        walk(root, root.path(), &mut files);
    } else if Path::new(template).is_dir() {
        fn walk(d: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
            for p in entries(d) {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if matches!(name.as_ref(), "node_modules" | ".next" | ".git" | ".egin") {
                    continue;
                }
                if p.is_dir() {
                    walk(&p, root, out)?;
                } else {
                    out.push((p.strip_prefix(root)?.to_path_buf(), fs::read(&p)?));
                }
            }
            Ok(())
        }
        walk(Path::new(template), Path::new(template), &mut files)?;
    } else {
        bail!("no template {template:?}: use {}, or a path to a folder", template_names().join(", "));
    }
    for (path, content) in &files {
        let p = dir.join(path);
        fs::create_dir_all(p.parent().unwrap_or(dir))?;
        fs::write(&p, content)?;
        // By folder name, so a subagent's tools/ and middleware/ run too.
        let folder = path.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str());
        if folder == Some("tools") && path.extension().is_none_or(|e| e != "json") || folder == Some("middleware") {
            executable(&p)?;
        }
    }
    // Name the model for the key that is set, so agent.toml says what runs.
    let toml_path = [dir.join("agent.toml"), dir.join("agent/agent.toml")].into_iter().find(|p| p.exists());
    if let Some(p) = toml_path {
        let text = fs::read_to_string(&p)?;
        let names_model = text.lines().any(|l| l.trim_start().starts_with("model"));
        let model = default_model().map(|(m, _)| m.to_string()).or_else(|| crate::model::has_key("you").then(|| Config::default().model));
        match (names_model, model) {
            (false, Some(m)) => fs::write(&p, format!("model = \"{m}\"\n{text}"))?,
            (false, None) => fs::write(&p, format!("# Set ANTHROPIC_API_KEY, OPENAI_API_KEY, AI_GATEWAY_API_KEY or YDC_API_KEY and egin picks a model,\n# or name one: model = \"anthropic/claude-sonnet-5-5\"\n{text}"))?,
            _ => {}
        }
    }
    fs::create_dir_all(if dir.join("agent").is_dir() { dir.join("agent/workspace") } else { dir.join("workspace") })?;
    Ok(())
}
