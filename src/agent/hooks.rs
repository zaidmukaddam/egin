//! hooks/<event>: executables in the agent folder that run on the host when a session's run stores
//! an event of that type (`tool_call`, `tool_result`, `approval_request`, `question`, `error`, ...),
//! with the event as JSON on stdin, in order, without holding the run up. For notifications, audit
//! logs, metrics.
//!
//! hooks/done is different: it runs when the model has answered, before the session is marked done,
//! with `{"type": "done", "session", "text"}` on stdin. Whatever it prints becomes the next user
//! message and the run goes on, so a hook can check the work (run the tests, say) and send the agent
//! back until it passes. Printing nothing, or exiting non-zero, lets the session finish.

use crate::{agent::Agent, sandbox, App};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};

/// How many times hooks/done may send one run on before the answer stands.
pub const MAX_CONTINUES: usize = 20;
const TIMEOUT_SECS: u64 = 120;

fn hook(agent: &Agent, kind: &str) -> Option<PathBuf> {
    let p = agent.dir.join("hooks").join(kind);
    (!kind.is_empty() && !kind.contains(['/', '.']) && p.is_file()).then_some(p)
}

async fn run(agent: &Agent, exe: &std::path::Path, input: &Value) -> anyhow::Result<sandbox::Out> {
    sandbox::host(&exe.to_string_lossy(), &[], &agent.workspace.to_string_lossy(), Some(input.to_string().into_bytes()), TIMEOUT_SECS).await
}

/// Passes this session's events to its hooks until dropped. Nothing is set up when the agent has none.
pub struct Watch(Option<(Arc<App>, String)>);

impl Drop for Watch {
    fn drop(&mut self) {
        // Dropping the sender ends the runner once it has run the hooks for events already sent.
        if let Some((app, sid)) = &self.0 {
            app.hooks.lock().unwrap().remove(sid);
        }
    }
}

pub fn watch(app: &Arc<App>, agent: &Arc<Agent>, sid: &str) -> Watch {
    if !agent.dir.join("hooks").is_dir() {
        return Watch(None);
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    app.hooks.lock().unwrap().insert(sid.to_string(), tx);
    let (agent, session) = (agent.clone(), sid.to_string());
    tokio::spawn(async move {
        while let Some(mut event) = rx.recv().await {
            let kind = event["type"].as_str().unwrap_or("").to_string();
            let Some(exe) = hook(&agent, &kind).filter(|_| kind != "done") else { continue };
            event["session"] = json!(session);
            match run(&agent, &exe, &event).await {
                Ok(o) if o.ok => {}
                Ok(o) => eprintln!("egin: hooks/{kind} failed for session {session}: {}", o.text.trim()),
                Err(e) => eprintln!("egin: hooks/{kind}: {e:#}"),
            }
        }
    });
    Watch(Some((app.clone(), sid.to_string())))
}

/// Run hooks/done, if there is one. Returns the message to continue with, if it printed one.
pub async fn on_done(agent: &Agent, sid: &str, text: &str) -> Option<String> {
    let exe = hook(agent, "done")?;
    match run(agent, &exe, &json!({"type": "done", "session": sid, "text": text})).await {
        Ok(o) if o.ok => Some(o.text.trim().to_string()).filter(|t| !t.is_empty()),
        Ok(o) => {
            eprintln!("egin: hooks/done failed for session {sid}; the answer stands: {}", o.text.trim());
            None
        }
        Err(e) => {
            eprintln!("egin: hooks/done: {e:#}");
            None
        }
    }
}
