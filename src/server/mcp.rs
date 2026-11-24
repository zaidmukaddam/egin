//! The agent as an MCP server: with channels/mcp.toml present, `egin serve` answers MCP over
//! Streamable HTTP at /mcp (JSON responses, no server-sent stream), behind EGIN_TOKEN like the other
//! session routes. Another agent, an IDE or Claude can then hand it work:
//!
//!   send_message {message, session?, wait?}  run a turn (new session, or continue one); returns the answer
//!   get_session  {session}                   status and last answer, for runs started with wait = false
//!
//! channels/mcp.toml
//!   description = "What this agent is for"   # optional: shown to the calling model

use crate::{agent::run, server::Srv};
use axum::{extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, response::Response, Json};
use serde_json::{json, Value};
use std::sync::Arc;

const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

fn tools(srv: &Srv) -> Value {
    let name = srv.agent.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "agent".into());
    let about = srv
        .agent
        .channels
        .get("mcp")
        .and_then(|c| c.get("description"))
        .and_then(|d| d.as_str())
        .map(crate::tools::mcp::expand)
        .unwrap_or_else(|| format!("the egin agent {name}"));
    json!([
        {
            "name": "send_message",
            "description": format!("Send a message to {about} and get its answer. Pass `session` to continue a conversation; without it a new one starts. A long task can take minutes: with wait = false the call returns at once, and get_session reports when it is done."),
            "inputSchema": {"type": "object", "properties": {
                "message": {"type": "string"},
                "session": {"type": "string", "description": "A session id from an earlier answer"},
                "wait": {"type": "boolean", "description": "Wait for the answer (default true)"}
            }, "required": ["message"]},
        },
        {
            "name": "get_session",
            "description": "A session's status (running, done, error, cancelled) and its latest answer.",
            "inputSchema": {"type": "object", "properties": {"session": {"type": "string"}}, "required": ["session"]},
            "annotations": {"readOnlyHint": true},
        }
    ])
}

fn result(text: String, structured: Value, error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured, "isError": error})
}

/// The session as this caller may see it: with `X-Egin-User`, only that user's sessions exist.
fn visible(srv: &Srv, user: Option<&str>, sid: &str) -> anyhow::Result<Option<String>> {
    let Some(status) = srv.app.store.status(sid)? else { return Ok(None) };
    match (user, srv.app.store.owner(sid)?) {
        (Some(u), Some(Some(o))) if o != u => Ok(None),
        (Some(_), Some(None)) => Ok(None),
        _ => Ok(Some(status)),
    }
}

fn last_answer(srv: &Srv, sid: &str) -> String {
    let msgs = srv.app.store.messages(sid).unwrap_or_default();
    msgs.iter().rev().find(|(_, m)| m["role"] == "assistant").and_then(|(_, m)| m["content"].as_str()).unwrap_or("").to_string()
}

async fn call(srv: &Arc<Srv>, user: Option<&str>, name: &str, args: &Value) -> anyhow::Result<Value> {
    let arg = |k: &str| args[k].as_str().map(str::to_string);
    match name {
        "send_message" => {
            let Some(text) = arg("message").filter(|t| !t.trim().is_empty()) else { return Ok(result("`message` is required".into(), json!({}), true)) };
            let sid = match arg("session") {
                Some(sid) => match visible(srv, user, &sid)? {
                    None => return Ok(result(format!("no session {sid}"), json!({}), true)),
                    Some(status) if status == "running" => {
                        run::enqueue(&srv.app, &sid, &text, &[], vec![])?;
                        let note = "The session is busy; the message joins its current run. Call get_session for the answer.";
                        return Ok(result(note.into(), json!({"session": sid, "status": "running", "queued": true}), false));
                    }
                    Some(_) => sid,
                },
                None => {
                    let sid = run::new_session(&srv.app, &srv.agent)?;
                    if let Some(u) = user {
                        srv.app.store.set_owner(&sid, u)?;
                    }
                    sid
                }
            };
            run::send(&srv.app, &sid, &text)?;
            if args["wait"] == false {
                crate::server::spawn(srv, &sid);
                return Ok(result(format!("Started session {sid}. Call get_session for the answer."), json!({"session": sid, "status": "running"}), false));
            }
            Ok(match srv.run(&sid).await {
                Ok(answer) => result(answer.clone(), json!({"session": sid, "status": "done", "answer": answer}), false),
                Err(e) => result(format!("The run failed: {e:#}"), json!({"session": sid, "status": "error"}), true),
            })
        }
        "get_session" => {
            let sid = arg("session").unwrap_or_default();
            let Some(status) = visible(srv, user, &sid)? else { return Ok(result(format!("no session {sid}"), json!({}), true)) };
            let answer = if status == "running" { String::new() } else { last_answer(srv, &sid) };
            let text = match status.as_str() {
                "running" => "Still running.".to_string(),
                "done" => answer.clone(),
                other => format!("The session is {other}. {answer}"),
            };
            Ok(result(text, json!({"session": sid, "status": status, "answer": answer}), false))
        }
        other => Ok(result(format!("no tool {other}"), json!({}), true)),
    }
}

/// `POST /mcp`: one JSON-RPC message.
pub async fn post(State(srv): State<Arc<Srv>>, headers: HeaderMap, Json(msg): Json<Value>) -> Response {
    let user = headers.get("x-egin-user").and_then(|v| v.to_str().ok()).map(str::trim).filter(|u| !u.is_empty()).map(String::from);
    // Notifications and responses get no reply.
    if msg.get("id").is_none() || msg.get("method").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = msg["id"].clone();
    let reply = |r: Result<Value, (i64, String)>| match r {
        Ok(result) => Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response(),
        Err((code, message)) => Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})).into_response(),
    };
    let p = &msg["params"];
    match msg["method"].as_str().unwrap_or("") {
        "initialize" => {
            let asked = p["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
            reply(Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "egin", "version": env!("CARGO_PKG_VERSION")},
            })))
        }
        "ping" => reply(Ok(json!({}))),
        "tools/list" => reply(Ok(json!({"tools": tools(&srv)}))),
        "tools/call" => {
            let name = p["name"].as_str().unwrap_or("");
            match call(&srv, user.as_deref(), name, &p["arguments"]).await {
                Ok(r) => reply(Ok(r)),
                Err(e) => reply(Ok(result(format!("error: {e:#}"), json!({}), true))),
            }
        }
        m => reply(Err((-32601, format!("method not found: {m}")))),
    }
}

/// No server-initiated stream: GET is refused, as the spec allows.
pub async fn get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "this server sends no event stream; POST JSON-RPC messages").into_response()
}
