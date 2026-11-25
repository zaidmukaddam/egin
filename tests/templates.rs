//! `egin init --template`: every template writes an agent that loads, subagents included, and the
//! marketing team's voice check blocks a bad draft through a subagent's middleware.

use egin::agent::{init_template, template_names, Agent};
use std::io::Write;
use std::process::{Command, Stdio};

fn fresh(template: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("egin-template-{template}-{}", egin::new_id()));
    init_template(&dir, template).unwrap();
    dir
}

#[test]
fn every_template_writes_an_agent_that_loads() {
    for name in template_names() {
        let dir = fresh(&name);
        let agent = Agent::load(&dir, &toml::Table::new()).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        for sub in &agent.subagents {
            Agent::load(&agent.dir.join("subagents").join(sub), &toml::Table::new()).unwrap_or_else(|e| panic!("{name}/{sub}: {e:#}"));
        }
    }
}

#[test]
fn a_model_override_reaches_subagents_that_name_no_model() {
    let dir = fresh("marketing");
    std::fs::write(dir.join("subagents/positioning/agent.toml"), "model = \"openai/gpt-6-luna\"\n").unwrap();
    let set = toml::Table::from_iter([
        ("model".to_string(), toml::Value::from("anthropic/claude-sonnet-5-5")),
        ("max_turns".to_string(), toml::Value::from(7)),
        ("max_cost".to_string(), toml::Value::from(0.5)),
    ]);
    let lead = Agent::load(&dir, &set).unwrap();
    let sub = |name: &str| {
        let d = dir.join("subagents").join(name);
        Agent::load(&d, &lead.inherited(&d)).unwrap()
    };
    let writer = sub("writer");
    assert_eq!(writer.cfg.model, "anthropic/claude-sonnet-5-5", "inherited: writer names no model");
    assert_eq!(writer.cfg.max_cost, 0.5, "inherited: writer sets no max_cost");
    assert_eq!(writer.cfg.max_turns, 40, "kept: writer sets its own max_turns");
    assert_eq!(sub("positioning").cfg.model, "openai/gpt-6-luna", "kept: positioning names its own");
}

#[test]
fn the_marketing_voice_check_runs_from_a_subagent() {
    let dir = fresh("marketing");
    let run = |call: serde_json::Value| {
        let mut child = Command::new(dir.join("subagents/social/middleware/pre_tool"))
            .current_dir(dir.join("workspace"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("init marks a subagent's middleware executable");
        child.stdin.take().unwrap().write_all(call.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    };
    let check = |content: &str| run(serde_json::json!({"tool": "write_file", "args": {"path": "/workspace/drafts/social/a.md", "content": content}}));
    let bash = |command: &str| run(serde_json::json!({"tool": "bash", "args": {"command": command}})).0;
    assert!(!bash("mkdir -p drafts; cat > drafts/post.md <<'EOF'\nseamless\nEOF"), "a shell write into drafts/ skips the check, so it's refused");
    assert!(!bash("printf x | tee drafts/social/a.md"));
    assert!(bash("cat drafts/post.md; ls drafts/ 2>/dev/null; mkdir -p drafts"), "reading drafts is fine");
    let (ok, why) = check("## x\nWe're excited to announce a seamless launch \u{2014} #agents\n---\n");
    assert!(!ok);
    for problem in ["em dash", "seamless", "excited to announce", "hashtag"] {
        assert!(why.contains(problem), "names {problem}: {why}");
    }
    let (ok, why) = check(&format!("## x\n{}", "a".repeat(281)));
    assert!(!ok && why.contains("281 characters"), "{why}");
    let (ok, why) = check("## x\nAn agent is a folder. https://egin.dev/a/very/long/link/that/counts/as/twenty/three\n");
    assert!(ok, "{why}");
}
