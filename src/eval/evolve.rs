//! `egin evolve`: an outer loop that edits a model's profile files from eval traces (HAR-42).
//!
//! Each round: score the train split, show a proposer model the failing transcripts and the
//! current profiles/<model>.toml and .md, take one edit plus its predicted effect, re-run the train
//! split, and keep the edit only if it scores better. Held-out tasks are never shown to the
//! proposer; they are run once at the end, before and after, to catch overfitting (HAR-43).

use crate::{
    eval::{self, Opts, Outcome},
    model, App,
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub struct Score {
    pub passed: usize,
    pub total: usize,
    pub cost: f64,
}

impl Score {
    fn of(o: &[Outcome]) -> Score {
        Score { passed: o.iter().filter(|o| o.pass).count(), total: o.len(), cost: o.iter().map(|o| o.cost).sum() }
    }
    /// More passes wins; at equal passes, at least 10% cheaper wins.
    fn beats(&self, other: &Score) -> bool {
        self.passed > other.passed || (self.passed == other.passed && self.cost < other.cost * 0.9)
    }
}

impl std::fmt::Display for Score {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}/{} passed, ${:.4}", self.passed, self.total, self.cost)
    }
}

const SWITCHES: &str = "tool_style (\"typed\" adds read/write/edit_file tools, \"bash\" is bash only), web (\"client\", \"server\", \"off\"), \
search_mode (\"highlights\"|\"snippets\"), search_count (int), batching (bool: several tool calls per turn), planning (bool: plan tool), \
compaction (bool) + tool_output_max (chars kept per tool result), elision (bool) + elide_at (0-1) + keep_recent (int), \
summarisation (bool) + summarise_at (0-1), max_turns (int), reasoning_effort (\"low\"|\"medium\"|\"high\"), max_tokens (int).";

async fn score(app: &Arc<App>, dir: &Path, model: &str, split: &str, jobs: usize, results: &Path) -> Result<(Score, Vec<Outcome>)> {
    let opts = Opts { models: vec![model.into()], split: Some(split.into()), jobs, results: Some(results.to_path_buf()), ..Default::default() };
    let out = eval::run_evals(app, dir, &opts).await?;
    Ok((Score::of(&out), out))
}

fn failures(out: &[Outcome]) -> String {
    let mut s = String::new();
    for o in out.iter().filter(|o| !o.pass) {
        let t = std::fs::read_to_string(Path::new(&o.trace).join("transcript.md")).unwrap_or_default();
        // Head and tail of each transcript keep the task and how it ended.
        let t: String =
            if t.len() > 6000 { format!("{}\n[...]\n{}", &t[..t.floor_char_boundary(2500)], &t[t.ceil_char_boundary(t.len() - 3500)..]) } else { t };
        s += &format!("### Failed: {} (error: {})\n{t}\n\n", o.eval, o.error.as_deref().unwrap_or("check failed"));
    }
    if s.is_empty() {
        s = "(no failures; look for wasted turns or cost)\n".into();
        for o in out.iter().take(3) {
            let t = std::fs::read_to_string(Path::new(&o.trace).join("transcript.md")).unwrap_or_default();
            s += &format!("### Passed: {} ({} turns, ${:.4})\n{}\n\n", o.eval, o.turns, o.cost, &t[..t.floor_char_boundary(t.len().min(3000))]);
        }
    }
    s
}

async fn propose(app: &App, proposer: &str, target: &str, toml_text: &str, md_text: &str, history: &str, traces: &str) -> Result<Value> {
    let prompt = format!(
        "You tune an agent harness for the model `{target}`. Its per-model profile is two files:\n\
         - profiles/{target}.toml: overrides for these switches: {SWITCHES}\n\
         - profiles/{target}.md: extra instructions appended to the system prompt.\n\n\
         Current profiles/{target}.toml:\n```toml\n{toml_text}\n```\nCurrent profiles/{target}.md:\n```\n{md_text}\n```\n\n\
         Edits tried so far:\n{history}\n\n\
         Transcripts from the latest run on the training tasks:\n{traces}\n\n\
         Propose ONE edit that should make the agent pass more tasks or cost less without failing more. Generalise: never mention \
         a specific task, file name or answer from the transcripts. Reply with only a JSON object:\n\
         {{\"file\": \"toml\" or \"md\", \"content\": \"the complete new file\", \"rationale\": \"one sentence\", \
         \"prediction\": {{\"pass_delta\": integer, \"cost_change_pct\": number}}}}"
    );
    let body = json!({"model": proposer, "messages": [{"role": "user", "content": prompt}], "stream": true, "stream_options": {"include_usage": true}});
    let ep = model::Endpoint::resolve(app, proposer, None)?;
    let reply = model::chat(app, &ep, &body, &mut |_, _| {}).await?;
    let text = reply.message["content"].as_str().unwrap_or("").to_string();
    let start = text.find('{').context("proposer gave no JSON")?;
    let end = text.rfind('}').context("proposer gave no JSON")?;
    serde_json::from_str(&text[start..=end]).with_context(|| format!("proposer JSON: {}", &text[start..=end]))
}

pub async fn evolve(app: &Arc<App>, dir: &Path, target: &str, proposer: &str, rounds: usize, jobs: usize) -> Result<Value> {
    let profiles = dir.join("profiles");
    std::fs::create_dir_all(&profiles)?;
    let (toml_path, md_path) = (profiles.join(format!("{target}.toml")), profiles.join(format!("{target}.md")));
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let (start_toml, start_md) = (read(&toml_path), read(&md_path));
    let run_dir = dir.join("evals/evolve").join(crate::new_id());
    let log_path = run_dir.join("evolve.jsonl");
    std::fs::create_dir_all(&run_dir)?;
    let log = |v: &Value| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap();
        writeln!(f, "{v}").ok();
    };

    let (mut best, mut out) = score(app, dir, target, "train", jobs, &run_dir.join("r0-train")).await?;
    eprintln!("evolve: start {best}");
    log(&json!({"round": 0, "train": best.to_string()}));
    let mut history = String::from("(none yet)\n");
    for round in 1..=rounds {
        let p = match propose(app, proposer, target, &read(&toml_path), &read(&md_path), &history, &failures(&out)).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("evolve: round {round}: no usable proposal: {e:#}");
                log(&json!({"round": round, "error": format!("{e:#}")}));
                continue;
            }
        };
        let path = if p["file"] == "md" { &md_path } else { &toml_path };
        let before = read(path);
        let content = p["content"].as_str().unwrap_or("").to_string();
        std::fs::write(path, &content)?;
        // A profile that does not load is rejected before it costs a run.
        if let Err(e) = crate::agent::Agent::load(dir, &toml::Table::from_iter([("model".to_string(), toml::Value::from(target))])) {
            std::fs::write(path, &before)?;
            history += &format!("- round {round}: invalid {} ({e:#}); rejected\n", p["file"]);
            log(&json!({"round": round, "proposal": p, "error": format!("{e:#}")}));
            continue;
        }
        let (s, o) = score(app, dir, target, "train", jobs, &run_dir.join(format!("r{round}-train"))).await?;
        let kept = s.beats(&best);
        eprintln!("evolve: round {round}: {} -> {s} ({})", best, if kept { "kept" } else { "reverted" });
        history += &format!(
            "- round {round}: {} edit: {}; predicted pass_delta {} cost {}%; got {s} vs {best}; {}\n",
            p["file"],
            p["rationale"].as_str().unwrap_or(""),
            p["prediction"]["pass_delta"],
            p["prediction"]["cost_change_pct"],
            if kept { "KEPT" } else { "reverted" }
        );
        log(&json!({"round": round, "proposal": p, "train": s.to_string(), "best_before": best.to_string(), "kept": kept,
            "actual_pass_delta": s.passed as i64 - best.passed as i64, "actual_cost_change_pct": if best.cost > 0.0 { (s.cost / best.cost - 1.0) * 100.0 } else { 0.0 }}));
        if kept {
            best = s;
            out = o;
        } else {
            std::fs::write(path, &before)?;
        }
    }

    // Held-out check: the evolved profile against the one we started from.
    let (evolved_toml, evolved_md) = (read(&toml_path), read(&md_path));
    let (held_evolved, _) = score(app, dir, target, "heldout", jobs, &run_dir.join("heldout-evolved")).await?;
    std::fs::write(&toml_path, &start_toml)?;
    std::fs::write(&md_path, &start_md)?;
    let (held_start, _) = score(app, dir, target, "heldout", jobs, &run_dir.join("heldout-start")).await?;
    std::fs::write(&toml_path, &evolved_toml)?;
    std::fs::write(&md_path, &evolved_md)?;
    for p in [&toml_path, &md_path] {
        if read(p).is_empty() {
            let _ = std::fs::remove_file(p);
        }
    }
    let summary = json!({"model": target, "train_best": best.to_string(), "heldout_start": held_start.to_string(),
        "heldout_evolved": held_evolved.to_string(), "evolved_beats_start_on_heldout": held_evolved.beats(&held_start), "log": log_path});
    log(&summary);
    if held_start.beats(&held_evolved) {
        eprintln!("evolve: held-out got worse ({held_evolved} vs {held_start}); the edits overfit the train split");
    }
    Ok(summary)
}

pub fn check_model(model: &str) -> Result<()> {
    if model.contains('/') || model.contains("..") {
        bail!("bad model id {model}");
    }
    Ok(())
}
