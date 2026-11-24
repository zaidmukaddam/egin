//! `egin step`: advance a session that someone else stores.
//!
//! A serverless host keeps no process and no disk between requests. On Vercel Workflow the session is
//! the workflow's state: each step hands the whole session in, egin does a bounded amount of work on
//! it in memory, and hands back what the session is now. Nothing is left behind, so any instance can
//! take the next step and a step that died is simply run again.
//!
//! In: `{"id", "messages", "events", "stats", "message"?, "answers"?, "calls"?, "seconds"?, "cancel"?}`
//! or `{"info": true}`.
//! - `messages` and `events` are the session so far, as an earlier step returned them. Empty
//!   `messages` starts a session.
//! - `message` is a new user message to add first.
//! - `answers` are replies to what the session is waiting on: `{"call_id", "allow"}` for an approval
//!   request, `{"call_id", "answer"}` for an `ask_user` question.
//! - `seconds` is how long this step may go on starting model calls. A turn that ends sooner ends the
//!   step; one that does not is handed back `running`, for the next step to continue.
//! - `calls` is how many model calls this step may make: 1 by default, any number when `seconds` is
//!   given. With 0 it only takes the message.
//! - `cancel` ends the turn in progress, turning down whatever it was waiting on.
//!
//! Out: `{"messages", "events", "stats", "status"}`. `messages` is the whole context, `events` only
//! the ones this step added, in order: event ids are positions in the session's log, counted from 1.
//! `status` is `running` when the turn has work left, `waiting` when a tool call needs an approval
//! or an answer first (the request is the step's last `approval_request` or `question` event), else
//! `done`, `error`, `cancelled` or `idle`.
//!
//! While the step runs, `seen` gets every event as it happens: token deltas, which are not stored,
//! and stored ones ahead of the result that carries them, so a host can show and save a reply or a
//! tool call without waiting for the step to end. When `cancel` resolves, the run stops where it
//! stands and the turn ends as with the `cancel` input.

use super::{run, Agent};
use crate::{app::Budget, App};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

/// End the turn in progress as cancelled.
fn end_turn(app: &App, sid: &str) -> Result<()> {
    let store = &app.store;
    // What the turn was waiting on is turned down, so the calls behind it get a result and the
    // next turn does not stop at them again.
    for (asked, stored, body) in [("approval_request", "approval", json!({"allow": false})), ("question", "answer", json!({"answer": ""}))] {
        for request in store.events_of(sid, asked)? {
            let call = request["call_id"].as_str().unwrap_or("");
            if store.find_event(sid, stored, call)?.is_none() {
                let mut body = body.clone();
                body["call_id"] = json!(call);
                app.emit(sid, stored, body);
            }
        }
    }
    // A call the turn never ran gets its result now, right behind the message that made it:
    // a result that arrived after the next user message would be out of place for the model.
    let all = store.messages(sid)?;
    if let Some(last) = all.iter().rposition(|(_, m)| m["role"] == "assistant") {
        let done: Vec<&str> = all[last + 1..].iter().filter_map(|(_, m)| m["tool_call_id"].as_str()).collect();
        for c in all[last].1["tool_calls"].as_array().into_iter().flatten() {
            let call = c["id"].as_str().unwrap_or("");
            if !done.contains(&call) {
                let out = "error: the user cancelled before this call ran";
                store.push_message(sid, &json!({"role": "tool", "tool_call_id": call, "content": out}))?;
                app.emit(sid, "tool_result", json!({"call_id": call, "tool": c["function"]["name"], "output": out}));
            }
        }
    }
    store.set_status(sid, "cancelled")?;
    app.emit(sid, "cancelled", json!({}));
    Ok(())
}

/// `seen` is called with each event's body and whether it is a stored one.
pub async fn step(app: &Arc<App>, agent: &Arc<Agent>, input: &Value, cancel: impl Future<Output = ()>, mut seen: impl FnMut(&Value, bool)) -> Result<Value> {
    if input["info"] == true {
        return Ok(crate::server::info(agent));
    }
    let sid = input["id"].as_str().context("`id` is required")?;
    let store = &app.store;
    store.create_session(sid, &agent.dir.to_string_lossy())?;
    let messages = input["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
    if messages.is_empty() {
        store.push_message(sid, &json!({"role": "system", "content": agent.system_prompt()}))?;
    }
    for m in messages {
        store.push_message(sid, m)?;
    }
    let before = input["events"].as_array().map(Vec::as_slice).unwrap_or_default();
    for e in before {
        store.push_event(sid, e)?;
    }
    if let Ok(stats) = serde_json::from_value(input["stats"].clone()) {
        store.put_stats(sid, &stats)?;
    }

    let mut events = app.events.subscribe();
    let until = input["seconds"].as_f64().and_then(|s| Duration::try_from_secs_f64(s).ok()).map(|d| Instant::now() + d);
    let calls = input["calls"].as_u64().unwrap_or(if until.is_some() { u64::MAX } else { 1 });
    let mut pass = |e: crate::Event| {
        if e.session == sid {
            seen(&e.body, e.id.is_some());
        }
    };
    let mut cancelled = input["cancel"] == true;
    if !cancelled {
        for a in input["answers"].as_array().into_iter().flatten() {
            let call = a["call_id"].as_str().context("an answer needs `call_id`")?;
            let (asked, stored) = if a.get("answer").is_some() { ("question", "answer") } else { ("approval_request", "approval") };
            // An answer to nothing that was asked is dropped, and the first answer to a request stands:
            // a host may deliver a stale or repeated one, and the session should not fail over it.
            let Some(request) = store.find_event(sid, asked, call)? else { continue };
            if store.find_event(sid, stored, call)?.is_none() {
                let body = match a.get("answer") {
                    Some(text) => json!({"call_id": call, "answer": text.as_str().unwrap_or("")}),
                    None => json!({"call_id": call, "tool": request["tool"], "allow": a["allow"].as_bool().unwrap_or(false)}),
                };
                app.emit(sid, stored, body);
            }
        }
        if let Some(text) = input["message"].as_str() {
            run::send(app, sid, text)?;
            store.set_status(sid, "running")?;
        }
        if calls > 0 {
            *app.budget.lock().unwrap() = Some(Budget { session: sid.to_string(), calls, until });
            let run = run::run_session(app.clone(), agent.clone(), sid.to_string());
            tokio::pin!(run, cancel);
            loop {
                tokio::select! {
                    Ok(e) = events.recv() => pass(e),
                    // How it ended is in the session's status and log, which the result carries.
                    _ = &mut run => break,
                    // The run is dropped where it stands, as `egin serve` aborts one.
                    _ = &mut cancel => {
                        cancelled = true;
                        break;
                    }
                }
            }
        }
    }
    if cancelled {
        end_turn(app, sid)?;
    }
    // What the step sent in its last moments leaves before it reports its result.
    while let Ok(e) = events.try_recv() {
        pass(e);
    }
    let messages: Vec<Value> = store.messages(sid)?.into_iter().map(|(_, m)| m).collect();
    let added: Vec<Value> = store.events_after(sid, before.len() as i64)?.into_iter().map(|(_, e)| e).collect();
    Ok(json!({
        "messages": messages,
        "events": added,
        "stats": store.stats(sid)?,
        "status": store.status(sid)?,
    }))
}
