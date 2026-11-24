//! `egin add`: every catalog item installs into a fresh agent, and the agent still loads, so a
//! connection, channel, exporter or tool file in the catalog can't drift out of what egin accepts.

use egin::{agent::Agent, catalog};

fn fresh() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("egin-catalog-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "sandbox = \"local\"\n").unwrap();
    dir
}

#[test]
fn every_item_installs_into_an_agent_that_loads() {
    let items = catalog::items();
    assert!(items.len() >= 25, "the catalog is embedded: {} items", items.len());
    let dir = fresh();
    for it in &items {
        assert!(!it.description.is_empty(), "{} has a description", it.id);
        let added = catalog::add(&it.id, &dir, false).unwrap_or_else(|e| panic!("{}: {e:#}", it.id));
        // The starter agent already has word_count.sh, which is kept rather than written.
        assert!(!added.written.is_empty() || !added.kept.is_empty(), "{} has files", it.id);
    }
    let agent = Agent::load(&dir, &toml::Table::new()).expect("an agent with every catalog item loads");
    let kinds = |prefix: &str| items.iter().filter(|i| i.id.starts_with(prefix)).count();
    assert_eq!(agent.connections.len(), kinds("connection/"));
    assert_eq!(agent.exporters.len(), kinds("instrumentation/"));
    assert_eq!(agent.channels.len(), kinds("channel/"));
}

/// Run one of tool/board's scripts with `args` on stdin against a stand-in for `vercel` that prints the
/// command line it got, and the text it read, as JSON.
fn board_tool(dir: &std::path::Path, tool: &str, args: serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    let fake = dir.join("fake-vercel.sh");
    std::fs::write(&fake, "#!/bin/sh\npython3 -c 'import json,sys; print(json.dumps({\"argv\": sys.argv[1:], \"stdin\": sys.stdin.read()}))' \"$@\"\n")
        .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut child = std::process::Command::new("python3")
        .arg(dir.join("tools").join(format!("{tool}.py")))
        .env("BOARD_CLI", &fake)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(args.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tool}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn board_tools_call_the_vercel_cli_with_the_right_arguments() {
    use serde_json::json;
    let dir = fresh();
    catalog::add("tool/board", &dir, false).unwrap();
    let agent = Agent::load(&dir, &toml::Table::new()).unwrap();
    let mut names: Vec<_> = agent.tools.iter().map(|t| t.name.as_str()).filter(|n| n.starts_with("board")).collect();
    names.sort();
    assert_eq!(
        names,
        ["board_post", "board_read", "board_schedule", "board_task_claim", "board_task_update", "board_tasks"],
        "the shared helper is not a tool"
    );
    let safe: Vec<_> = agent.tools.iter().filter(|t| t.name.starts_with("board") && t.safe).map(|t| t.name.as_str()).collect();
    assert_eq!(safe.len(), 2, "only reading runs without approval: {safe:?}");

    let argv = |tool: &str, args| board_tool(&dir, tool, args)["argv"].clone();
    assert_eq!(
        argv("board_read", json!({"target": "dev", "label": ["kind=bug"], "limit": 5})),
        json!(["board", "read", "dev", "--label", "kind=bug", "--limit", "5", "--json"])
    );
    let post = board_tool(&dir, "board_post", json!({"target": "dev", "text": "deploys are green"}));
    assert_eq!(post["argv"], json!(["board", "post", "dev", "--file", "-", "--json"]));
    assert_eq!(post["stdin"], "deploys are green", "the text goes in on stdin, off the process list");
    assert_eq!(argv("board_post", json!({"target": "msg_7", "text": "on it"}))[1], "reply", "a message ID replies in its thread");
    assert_eq!(
        argv("board_task_claim", json!({"channel": "work/", "label": ["step=fix"], "wait": "10m"})),
        json!(["board", "tasks", "claim", "--ready", "work/", "--label", "step=fix", "--wait", "10m", "--json"])
    );
    assert_eq!(
        argv("board_task_update", json!({"id": "msg_9", "action": "complete", "text": "fixed in a1b2c3"})),
        json!(["board", "tasks", "complete", "msg_9", "fixed in a1b2c3", "--json"])
    );
    assert_eq!(argv("board_task_update", json!({"id": "msg_9", "action": "renew", "text": "ignored"})), json!(["board", "tasks", "renew", "msg_9", "--json"]));
    assert!(board_tool(&dir, "board_task_update", json!({"id": "msg_9", "action": "fail"}))["error"].as_str().unwrap().contains("reason"));
    assert_eq!(
        argv("board_schedule", json!({"target": "msg_9", "text": "check CI", "in": "5m"})),
        json!(["board", "schedule", "msg_9", "--file", "-", "--in", "5m", "--json"])
    );
}

#[test]
fn adding_keeps_existing_files_and_suggests_close_names() {
    let dir = fresh();
    catalog::add("connection/linear", &dir, false).unwrap();
    std::fs::write(dir.join("connections/linear.toml"), "url = \"https://mine.example/mcp\"\n").unwrap();
    let again = catalog::add("connection/linear", &dir, false).unwrap();
    assert!(again.written.is_empty() && again.kept.len() == 1, "an edited file is kept");
    assert!(std::fs::read_to_string(dir.join("connections/linear.toml")).unwrap().contains("mine.example"));
    catalog::add("connection/linear", &dir, true).unwrap();
    assert!(std::fs::read_to_string(dir.join("connections/linear.toml")).unwrap().contains("mcp.linear.app"), "--force replaces it");

    let err = catalog::add("channel/slak", &dir, false).err().unwrap().to_string();
    assert!(err.contains("channel/slack"), "{err}");
}
