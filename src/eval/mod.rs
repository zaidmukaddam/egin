//! Eval runner. A task is evals/<name>.toml, or evals/<name>/task.toml with evals/<name>/files/
//! copied into the workspace first. Every task runs once per variant (evals/variants.toml) and
//! model, in a fresh copy of the workspace, and every run leaves a readable trace:
//!   evals/results/<run>/summary.jsonl
//!   evals/results/<run>/<eval>__<variant>__<model>/{transcript.md, messages.json, events.jsonl, trajectory.json}

pub mod bench;
pub mod evolve;
pub mod grade;
pub mod report;

use crate::{
    agent::{run, Agent},
    App,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub prompt: String,
    /// Shell command run in the workspace afterwards; exit 0 passes.
    pub check: Option<String>,
    /// Text the final answer must contain (case-insensitive).
    pub expect: Option<String>,
    /// The node set the answer's `Final Answer: [...]` line must equal exactly (GraphWalks).
    pub answer_set: Option<Vec<String>>,
    /// A gold answer scored by GAIA's rules against the reply's `FINAL ANSWER:` line.
    pub gaia: Option<String>,
    /// A gold answer graded by a judge model (SimpleQA); only CORRECT passes.
    pub judge: Option<grade::Judge>,
    /// "train" (default) or "heldout"; `evolve` only ever looks at train.
    #[serde(default)]
    pub split: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub files: Option<PathBuf>,
}

impl Task {
    pub fn split(&self) -> &str {
        self.split.as_deref().unwrap_or("train")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub eval: String,
    pub variant: String,
    pub model: String,
    #[serde(default)]
    pub split: String,
    #[serde(default)]
    pub category: String,
    pub pass: bool,
    pub turns: u64,
    pub tool_calls: u64,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub wall_ms: u64,
    pub error: Option<String>,
    #[serde(default)]
    pub trace: String,
}

pub fn load_tasks(dir: &Path) -> Result<Vec<Task>> {
    let evals = dir.join("evals");
    let mut paths: Vec<_> = std::fs::read_dir(&evals).context("no evals/ folder")?.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut tasks = vec![];
    for p in paths {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let (file, files) = if p.is_dir() && p.join("task.toml").exists() {
            (p.join("task.toml"), p.join("files").is_dir().then(|| p.join("files")))
        } else if p.extension().is_some_and(|e| e == "toml") && name != "variants" {
            (p.clone(), None)
        } else {
            continue;
        };
        let mut t: Task = toml::from_str(&std::fs::read_to_string(&file)?).with_context(|| file.display().to_string())?;
        t.name = name;
        t.files = files;
        tasks.push(t);
    }
    Ok(tasks)
}

pub fn variants(dir: &Path) -> Result<toml::Table> {
    let mut v: toml::Table = match std::fs::read_to_string(dir.join("evals/variants.toml")) {
        Ok(s) => toml::from_str(&s).context("variants.toml")?,
        Err(_) => toml::Table::new(),
    };
    if v.is_empty() {
        v.insert("default".into(), toml::Value::Table(toml::Table::new()));
    }
    Ok(v)
}

#[derive(Default, Clone)]
pub struct Opts {
    pub variants: Vec<String>,
    pub models: Vec<String>,
    /// Only tasks in this split.
    pub split: Option<String>,
    /// Only these task names.
    pub only: Vec<String>,
    pub jobs: usize,
    /// Extra overrides applied on top of every variant.
    pub overrides: toml::Table,
    pub results: Option<PathBuf>,
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let ok = Command::new("cp").arg("-R").arg(format!("{}/.", from.display())).arg(to).status()?.success();
    anyhow::ensure!(ok, "copying {} failed", from.display());
    Ok(())
}

/// Render a session as Markdown, for reading a run without tooling.
pub fn transcript(messages: &[Value]) -> String {
    let mut s = String::new();
    for m in messages {
        let role = m["role"].as_str().unwrap_or("");
        match role {
            "tool" => s += &format!("### tool result `{}`\n```\n{}\n```\n\n", m["tool_call_id"].as_str().unwrap_or(""), m["content"].as_str().unwrap_or("")),
            _ => {
                s += &format!("## {role}\n\n");
                if let Some(r) = m["reasoning_content"].as_str() {
                    s += &format!("<details><summary>reasoning</summary>\n\n{r}\n\n</details>\n\n");
                }
                s += &format!("{}\n\n", m["content"].as_str().unwrap_or(""));
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    s += &format!("**→ {}** `{}`\n\n", c["function"]["name"].as_str().unwrap_or(""), c["function"]["arguments"].as_str().unwrap_or(""));
                }
            }
        }
    }
    s
}

/// Save a session's messages, events and transcript under `dir`.
pub fn save_trace(app: &App, sid: &str, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let msgs: Vec<Value> = app.store.messages(sid)?.into_iter().map(|(_, m)| m).collect();
    std::fs::write(dir.join("messages.json"), serde_json::to_string_pretty(&msgs)?)?;
    std::fs::write(dir.join("transcript.md"), transcript(&msgs))?;
    let events: Vec<String> = app.store.events_after(sid, 0)?.into_iter().map(|(_, e)| e.to_string()).collect();
    std::fs::write(dir.join("events.jsonl"), events.join("\n") + "\n")?;
    // ATIF, so a run's trace can be validated by Harbor and mined for RL tasks by Repo2RLEnv.
    if let Ok(t) = crate::atif::export(app, sid) {
        std::fs::write(dir.join("trajectory.json"), serde_json::to_string_pretty(&t)?)?;
    }
    Ok(())
}

/// A session's measured outcome; the caller fills in which eval, variant and model it was.
pub fn outcome(app: &App, sid: &str, pass: bool, error: Option<String>, trace: &Path) -> Result<Outcome> {
    let stats = app.store.stats(sid)?;
    Ok(Outcome {
        eval: String::new(),
        variant: String::new(),
        model: String::new(),
        split: String::new(),
        category: String::new(),
        pass,
        turns: stats.model_calls,
        tool_calls: stats.tool_calls,
        prompt_tokens: stats.prompt_tokens,
        cached_tokens: stats.cached_tokens,
        completion_tokens: stats.completion_tokens,
        cost: stats.cost,
        wall_ms: stats.wall_ms,
        error,
        trace: trace.to_string_lossy().into(),
    })
}

/// Run a task's check in the agent's own sandbox, so it sees files exactly as the agent left them
/// (a VM keeps guest permissions in xattrs, so host-side `test -x` would disagree).
pub async fn check(app: &App, agent: &Agent, sid: &str, check: Option<&str>) -> bool {
    let Some(c) = check else { return true };
    let argv: Vec<String> = ["sh", "-c", c].iter().map(|s| s.to_string()).collect();
    let o = crate::sandbox::exec(app, agent, sid, &argv, None, 300).await;
    if !o.ok {
        app.emit(sid, "check_failed", serde_json::json!({"output": o.text}));
    }
    o.ok
}

/// The judge's verdict, recorded in the session's events; a failed judge call is a fail.
async fn judged(app: &App, sid: &str, task: &Task, answer: &str) -> bool {
    let Some(judge) = &task.judge else { return true };
    match judge.grade(app, &task.prompt, answer).await {
        Ok(g) => {
            app.emit(sid, "judge", serde_json::json!({"model": judge.model, "grade": format!("{g:?}")}));
            g == grade::Grade::Correct
        }
        Err(e) => {
            app.emit(sid, "judge", serde_json::json!({"model": judge.model, "error": format!("{e:#}")}));
            false
        }
    }
}

/// An external harness on one eval task, in the microVM egin would have used for this session (started
/// here, with the task's files in its workspace). Its reply is graded like egin's final answer.
async fn external(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, driver: &Path, prompt: &str) -> Result<String> {
    anyhow::ensure!(agent.cfg.sandbox == "microsandbox", "an external harness runs in a microVM; this agent's sandbox is {:?}", agent.cfg.sandbox);
    let warm = crate::sandbox::exec(app, agent, sid, &["true".to_string()], None, 300).await;
    anyhow::ensure!(warm.ok, "starting the sandbox: {}", warm.text);
    let vm = crate::sandbox::vm_name(sid);
    let (error, reply) = bench::external(app, agent, sid, driver, &vm, None, &crate::sandbox::workdir(agent), prompt, Some(&agent.instructions), None).await?;
    match error {
        Some(e) => Err(anyhow::anyhow!(e)),
        None => Ok(reply),
    }
}

async fn one(app: Arc<App>, dir: PathBuf, task: Task, variant: String, mut overrides: toml::Table, results: PathBuf) -> Result<Outcome> {
    // `external = "<driver folder>"` in a variant runs another harness (bench/eve-arm) on the task instead
    // of egin's loop: same sandbox and files, same model through a metering proxy, egin's web tools, and
    // the agent's own instructions. Grading is identical.
    let driver = overrides.remove("external").and_then(|v| v.as_str().map(|p| dir.join(p)));
    let mut agent = Agent::load(&dir, &overrides)?;
    let work = std::env::temp_dir().join(format!("egin-eval-{}", crate::new_id()));
    copy_dir(&agent.workspace, &work)?;
    if let Some(files) = &task.files {
        copy_dir(files, &work)?;
    }
    agent.workspace = work.canonicalize()?;
    let agent = Arc::new(agent);

    let sid = run::new_session(&app, &agent)?;
    run::send(&app, &sid, &task.prompt)?;
    let result = match &driver {
        None => run::run_session(app.clone(), agent.clone(), sid.clone()).await,
        Some(driver) => external(&app, &agent, &sid, driver, &task.prompt).await,
    };
    let pass = match &result {
        Err(_) => false,
        Ok(answer) => {
            task.expect.as_ref().is_none_or(|e| answer.to_lowercase().contains(&e.to_lowercase()))
                && task.answer_set.as_ref().is_none_or(|want| grade::final_set(answer) == Some(want.iter().cloned().collect()))
                && task.gaia.as_ref().is_none_or(|gold| grade::gaia(answer, gold))
                && judged(&app, &sid, &task, answer).await
                && check(&app, &agent, &sid, task.check.as_deref()).await
        }
    };
    crate::sandbox::remove(&app, &agent.cfg.sandbox, &sid).await;
    let _ = std::fs::remove_dir_all(&agent.workspace);
    let trace = results.join(format!("{}__{}__{}", task.name, variant, agent.cfg.model));
    save_trace(&app, &sid, &trace)?;
    let o = Outcome {
        eval: task.name.clone(),
        variant,
        model: agent.cfg.model.clone(),
        split: task.split().into(),
        category: task.category.clone().unwrap_or_default(),
        ..outcome(&app, &sid, pass, result.err().map(|e| format!("{e:#}")), &trace)?
    };
    eprintln!("{} {} / {} / {}", if o.pass { "PASS" } else { "FAIL" }, o.eval, o.variant, o.model);
    Ok(o)
}

pub async fn run_evals(app: &Arc<App>, dir: &Path, opts: &Opts) -> Result<Vec<Outcome>> {
    let tasks: Vec<Task> = load_tasks(dir)?
        .into_iter()
        .filter(|t| opts.split.as_deref().is_none_or(|s| t.split() == s))
        .filter(|t| opts.only.is_empty() || opts.only.contains(&t.name))
        .collect();
    let results = match &opts.results {
        Some(r) => {
            std::fs::create_dir_all(r)?;
            r.clone()
        }
        // Evals started in the same instant (one per model) drew the same id, shared a folder and
        // overwrote each other's summary. Whoever creates the folder owns it; the others take a new id.
        None => {
            let root = dir.join("evals/results");
            std::fs::create_dir_all(&root)?;
            loop {
                let r = root.join(crate::new_id());
                match std::fs::create_dir(&r) {
                    Ok(()) => break r,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => std::thread::sleep(std::time::Duration::from_millis(1)),
                    Err(e) => return Err(e.into()),
                }
            }
        }
    };

    let mut jobs = vec![];
    for (variant, overrides) in variants(dir)? {
        if !opts.variants.is_empty() && !opts.variants.contains(&variant) {
            continue;
        }
        let models: Vec<Option<&String>> = if opts.models.is_empty() { vec![None] } else { opts.models.iter().map(Some).collect() };
        for model in models {
            for task in &tasks {
                let mut o = overrides.as_table().cloned().unwrap_or_default();
                o.insert("approval".into(), "never".into());
                o.extend(opts.overrides.clone());
                if let Some(m) = model {
                    o.insert("model".into(), m.as_str().into());
                }
                jobs.push((task.clone(), variant.clone(), o));
            }
        }
    }

    // Run `jobs` at a time, keeping the original order in the output.
    let sem = Arc::new(tokio::sync::Semaphore::new(opts.jobs.max(1)));
    let handles: Vec<_> = jobs
        .into_iter()
        .map(|(task, variant, o)| {
            let (app, dir, results, sem) = (app.clone(), dir.to_path_buf(), results.clone(), sem.clone());
            tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
                one(app, dir, task, variant, o, results).await
            })
        })
        .collect();
    let mut out = vec![];
    for h in handles {
        out.push(h.await??);
    }
    let lines: Vec<String> = out.iter().map(|o| serde_json::to_string(o).unwrap()).collect();
    std::fs::write(results.join("summary.jsonl"), lines.join("\n") + "\n")?;
    eprintln!("traces: {}", results.display());
    Ok(out)
}

pub fn table(outcomes: &[Outcome]) -> String {
    let mut s = String::from("| eval | variant | model | pass | turns | tools | prompt tok | cached | completion | cost $ | wall s |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    for o in outcomes {
        s += &format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.4} | {:.1} |\n",
            o.eval,
            o.variant,
            o.model,
            if o.pass { "yes" } else { "NO" },
            o.turns,
            o.tool_calls,
            o.prompt_tokens,
            o.cached_tokens,
            o.completion_tokens,
            o.cost,
            o.wall_ms as f64 / 1000.0
        );
    }
    let passed = outcomes.iter().filter(|o| o.pass).count();
    s + &format!("\n{passed}/{} passed, total ${:.4}\n", outcomes.len(), outcomes.iter().map(|o| o.cost).sum::<f64>())
}
