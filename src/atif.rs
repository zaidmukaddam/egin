//! A session as an ATIF trajectory (Agent Trajectory Interchange Format, the schema Harbor validates
//! and Repo2RLEnv turns into RL tasks).
//!
//! Built from the event log, not the message history: the history is rewritten when old tool output
//! is elided or old turns are summarised, while the events keep every reply and every tool result as
//! it happened, which is what replaying a session's file writes needs.

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::app::App;

/// The ATIF version written: v1.7 added `extra` on tool calls, which keeps egin's own form of a call
/// that is exported under a common name.
pub const SCHEMA: &str = "ATIF-v1.7";

/// The ATIF trajectory of session `sid`.
pub fn export(app: &App, sid: &str) -> Result<Value> {
    let events = app.store.events_after(sid, 0)?;
    let mut steps: Vec<Value> = vec![];
    if let Some((_, first)) = app.store.messages(sid)?.into_iter().next() {
        if first["role"] == "system" {
            steps.push(json!({"source": "system", "message": text(&first["content"])}));
        }
    }
    // Which step a tool call was made in, so its result lands in that step's observation.
    let mut call_step: Map<String, Value> = Map::new();
    let mut model: Option<String> = None;
    let (mut elided, mut summarised) = (0u64, 0u64);
    for (_, e) in &events {
        let at = e["at"].as_u64().and_then(timestamp);
        let step = match e["type"].as_str().unwrap_or("") {
            "user" => json!({"source": "user", "message": e["text"].as_str().unwrap_or("")}),
            "assistant" => {
                let m = e["model"].as_str().map(str::to_string);
                if model.is_none() {
                    model = m.clone();
                }
                let mut s = json!({"source": "agent", "message": text(&e["content"])});
                if m.is_some() && m != model {
                    s["model_name"] = json!(m);
                }
                if let Some(r) = e["reasoning"].as_str().filter(|r| !r.is_empty()) {
                    s["reasoning_content"] = json!(r);
                }
                let calls: Vec<Value> = e["tool_calls"].as_array().into_iter().flatten().map(tool_call).collect();
                for c in &calls {
                    call_step.insert(c["tool_call_id"].as_str().unwrap_or_default().to_string(), json!(steps.len()));
                }
                if !calls.is_empty() {
                    s["tool_calls"] = json!(calls);
                }
                let u = &e["usage"];
                let mut metrics = Map::new();
                for (k, from) in [("prompt_tokens", "prompt"), ("completion_tokens", "completion"), ("cached_tokens", "cached")] {
                    if let Some(n) = u[from].as_u64() {
                        metrics.insert(k.into(), json!(n));
                    }
                }
                if let Some(c) = e["cost"].as_f64() {
                    metrics.insert("cost_usd".into(), json!(c));
                }
                if !metrics.is_empty() {
                    s["metrics"] = Value::Object(metrics);
                }
                s
            }
            "tool_result" => {
                // An external harness's web tools carry no call id: there is no step to attach them to.
                let Some(i) = e["call_id"].as_str().and_then(|id| call_step.get(id)).and_then(Value::as_u64) else { continue };
                let s = &mut steps[i as usize];
                if s.get("observation").is_none() {
                    s["observation"] = json!({"results": []});
                }
                let result = json!({"source_call_id": e["call_id"], "content": text(&e["output"])});
                s["observation"]["results"].as_array_mut().unwrap().push(result);
                continue;
            }
            "wrap_up" => json!({"source": "system", "message": format!(
                "egin: limit reached ({}); the model was asked for its final answer without tools.",
                e["reason"].as_str().unwrap_or("")
            )}),
            "deadline" => json!({"source": "system", "message": format!(
                "egin: about {}s left before the run's time limit; the model was told to {}.",
                e["left_secs"].as_f64().unwrap_or_default(),
                if e["kind"] == "check" { "put a working result in place" } else { "save its work" }
            )}),
            "check_in" => json!({"source": "system", "message": format!(
                "egin: {} model calls so far; the model was asked to finish if the work is in place.",
                e["calls"].as_u64().unwrap_or(0)
            )}),
            "elided" => {
                elided += e["count"].as_u64().unwrap_or(0);
                continue;
            }
            "summarised" => {
                summarised += 1;
                continue;
            }
            _ => continue,
        };
        let mut step = step;
        if let Some(t) = at {
            step["timestamp"] = json!(t);
        }
        steps.push(step);
    }
    if steps.is_empty() {
        bail!("session {sid} has no steps to export");
    }
    for (i, s) in steps.iter_mut().enumerate() {
        s["step_id"] = json!(i + 1);
    }
    let stats = app.store.stats(sid)?;
    let mut agent = json!({"name": "egin", "version": env!("CARGO_PKG_VERSION")});
    if let Some(m) = &model {
        agent["model_name"] = json!(m);
    }
    let mut out = json!({
        "schema_version": SCHEMA,
        "session_id": sid,
        "agent": agent,
        "final_metrics": {
            "total_prompt_tokens": stats.prompt_tokens,
            "total_completion_tokens": stats.completion_tokens,
            "total_cached_tokens": stats.cached_tokens,
            "total_cost_usd": stats.cost,
            "total_steps": steps.len(),
        },
        "steps": steps,
    });
    // The model saw a shorter context than the events record when either of these happened.
    if elided > 0 || summarised > 0 {
        out["extra"] = json!({"egin_elided_tool_outputs": elided, "egin_summarisations": summarised});
    }
    Ok(out)
}

/// An OpenAI tool call as an ATIF one: arguments are an object, so a string that is not JSON is
/// kept under `raw` rather than dropped.
///
/// `edit_file` goes out as `edit` with `path`/`old_string`/`new_string`, the names trajectory tools
/// replay (Repo2RLEnv rebuilds a session's files from writes and edits it recognises, and marks any
/// file touched by another tool as lost). Same meaning: the first match of the old text is replaced.
/// egin's own name and arguments stay in `extra`.
fn tool_call(c: &Value) -> Value {
    let raw = c["function"]["arguments"].as_str().unwrap_or("");
    let args = match serde_json::from_str::<Value>(raw) {
        Ok(v @ Value::Object(_)) => v,
        _ if raw.is_empty() => json!({}),
        _ => json!({"raw": raw}),
    };
    let name = c["function"]["name"].as_str().unwrap_or("");
    let mut out = json!({"tool_call_id": c["id"], "function_name": name, "arguments": args});
    if name == "edit_file" && args["old"].is_string() && args["new"].is_string() {
        out["function_name"] = json!("edit");
        out["arguments"] = json!({"path": args["path"], "old_string": args["old"], "new_string": args["new"]});
        out["extra"] = json!({"egin_tool": name, "egin_arguments": args});
    }
    out
}

/// Message content as text: a string as is, the text parts of a parts array joined.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn timestamp(ms: u64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}
