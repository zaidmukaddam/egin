//! Shared fixtures: a scripted mock of the router, search and contents APIs that speaks the real
//! wire format (SSE chunks), so the production client is what runs, plus an agent folder per test.
#![allow(dead_code)] // each test crate uses a different subset

use axum::{extract::State, routing::get, routing::post, Json, Router};
use egin::{
    agent::{run, Agent},
    server, App, Approve,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct Mock {
    pub url: String,
    script: Arc<Mutex<VecDeque<Value>>>,
    reqs: Arc<Mutex<Vec<Value>>>,
}

impl Mock {
    /// Every chat request the harness sent, in order (summarisation calls excluded).
    pub fn reqs(&self) -> Vec<Value> {
        self.reqs.lock().unwrap().clone()
    }
}

/// A scripted assistant reply with text only.
pub fn say(text: &str) -> Value {
    json!({"content": text})
}

/// A scripted assistant reply calling tools: (call id, tool name, arguments).
pub fn call(calls: &[(&str, &str, Value)]) -> Value {
    let calls: Vec<Value> = calls.iter().map(|(id, name, args)| json!({"id": id, "name": name, "arguments": args.to_string()})).collect();
    json!({"content": "", "tool_calls": calls})
}

pub fn bash(id: &str, command: &str) -> Value {
    call(&[(id, "bash", json!({"command": command}))])
}

/// A scripted rate limit: the router answers 429 to this request.
pub fn limited() -> Value {
    json!({"status": 429})
}

/// A scripted 400 with this body, as the router sends it: `policy_block()` for its content policy.
pub fn bad_request(message: &str) -> Value {
    json!({"status": 400, "body": {"error": {"message": message, "type": "invalid_request_error", "param": null, "code": "400"}}})
}

pub fn policy_block() -> Value {
    bad_request("The request was blocked by the content policy.")
}

async fn chat(State(m): State<Mock>, Json(body): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let summarising = body.get("tools").is_none() && body["messages"][0]["content"].as_str().is_some_and(|c| c.starts_with("Summarise this agent transcript"));
    let msg = if summarising {
        say("SUMMARY")
    } else {
        m.reqs.lock().unwrap().push(body);
        m.script.lock().unwrap().pop_front().unwrap_or(say("script exhausted"))
    };
    if msg["status"] == 429 {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "slow down").into_response();
    }
    if msg["status"] == 400 {
        return (axum::http::StatusCode::BAD_REQUEST, msg["body"].to_string()).into_response();
    }
    // Split every string across two chunks, as a real stream does.
    let mut chunks = vec![];
    if let Some(r) = msg["reasoning"].as_str() {
        chunks.push(json!({"choices": [{"delta": {"role": "assistant", "reasoning_content": r}}]}));
    }
    let text = msg["content"].as_str().unwrap_or("");
    let (a, b) = text.split_at(text.len() / 2);
    chunks.push(json!({"choices": [{"delta": {"role": "assistant", "content": a}}]}));
    chunks.push(json!({"choices": [{"delta": {"content": b}}]}));
    for (i, c) in msg["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let args = c["arguments"].as_str().unwrap();
        let (a, b) = args.split_at(args.len() / 2);
        chunks.push(json!({"choices": [{"delta": {"tool_calls": [{"index": i, "id": c["id"], "function": {"name": c["name"], "arguments": a}}]}}]}));
        chunks.push(json!({"choices": [{"delta": {"tool_calls": [{"index": i, "function": {"arguments": b}}]}}]}));
    }
    chunks.push(json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}));
    chunks.push(json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 10, "prompt_tokens_details": {"cached_tokens": 40}}}));
    (chunks.iter().map(|c| format!("data: {c}\n\n")).collect::<String>() + "data: [DONE]\n\n").into_response()
}

async fn mock(script: Vec<Value>) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let m = Mock { url: format!("http://{}", listener.local_addr().unwrap()), script: Arc::new(Mutex::new(script.into())), ..Default::default() };
    let models = json!({"data": [{"id": "mock", "max_input_tokens": 100000, "pricing": {"input_per_mtok": "1", "cached_input_per_mtok": "0.1", "output_per_mtok": "2"}}]});
    let search = json!({"results": {"web": [{"title": "Mock Result", "url": "https://example.com", "contents": {"highlights": ["mock highlight"]}}]}});
    let contents = json!([{"title": "Page", "url": "https://example.com", "markdown": "page body"}]);
    let app = Router::new()
        .route("/chat/completions", post(chat))
        .route("/models", get(move || async move { Json(models) }))
        .route("/search", post(move || async move { Json(search) }))
        .route("/contents", post(move || async move { Json(contents) }))
        .with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    m
}

/// One test's world: an agent folder (from the basic template, on the mock model, tools run
/// locally) and the app pointed at the mock.
pub struct Fx {
    pub app: Arc<App>,
    pub dir: PathBuf,
    pub mock: Mock,
}

impl Fx {
    /// Load the agent with extra agent.toml overrides.
    pub fn agent(&self, overrides: &str) -> Arc<Agent> {
        Arc::new(Agent::load(&self.dir, &toml::from_str(overrides).unwrap()).unwrap())
    }

    /// Start a session with one prompt and run it to the end. Returns (session id, answer).
    pub async fn go(&self, agent: &Arc<Agent>, prompt: &str) -> (String, anyhow::Result<String>) {
        let sid = run::new_session(&self.app, agent).unwrap();
        run::send(&self.app, &sid, prompt).unwrap();
        let out = run::run_session(self.app.clone(), agent.clone(), sid.clone()).await;
        (sid, out)
    }

    /// Write a file into the agent folder (optionally executable).
    pub fn write(&self, path: &str, content: &str, exec: bool) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        if exec {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// The tool results stored for a session, in order.
    pub fn tool_outputs(&self, sid: &str) -> Vec<String> {
        let msgs = self.app.store.messages(sid).unwrap();
        msgs.iter().filter(|(_, m)| m["role"] == "tool").map(|(_, m)| m["content"].as_str().unwrap().to_string()).collect()
    }

    /// Serve the HTTP API for an agent on a free port; returns its base URL.
    pub async fn serve(&self, agent: Arc<Agent>) -> String {
        self.serve_with(agent, None).await
    }

    pub async fn serve_with(&self, agent: Arc<Agent>, token: Option<&str>) -> String {
        let (router, _) = server::router(self.app.clone(), agent, token.map(String::from));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        base
    }
}

/// A fixture whose mock model replies with `script`, in order.
pub async fn fx(script: Vec<Value>, approve: Approve) -> Fx {
    let dir = std::env::temp_dir().join(format!("egin-test-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "model = \"mock\"\napproval = \"never\"\nsandbox = \"local\"\n").unwrap();
    let mock = mock(script).await;
    let app = App::with(&egin::state_db(&dir), approve, "key", &mock.url, &mock.url).unwrap();
    Fx { app, dir, mock }
}

/// The same, as one of several egin processes sharing the session store at `store_url`: its own
/// agent folder and model, the others' sessions.
pub async fn fx_on(store_url: &str, script: Vec<Value>, approve: Approve) -> Fx {
    let mut f = fx(script, approve).await;
    let store = egin::store::Store::remote(store_url, "").unwrap();
    f.app = App::with_store(store, approve, "key", &f.mock.url, &f.mock.url).unwrap();
    f
}

/// A libSQL server's HTTP API (`POST /v2/pipeline`, as Turso documents it) over an in-memory SQLite
/// database. Returns its URL and the database, for a test to reach behind the store's back.
pub async fn libsql() -> (String, Arc<Mutex<rusqlite::Connection>>) {
    use rusqlite::types::{Value as Sql, ValueRef};
    let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
    let execute = |db: &rusqlite::Connection, stmt: &Value| -> rusqlite::Result<Value> {
        let args: Vec<Sql> = stmt["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| match a["type"].as_str() {
                Some("integer") => Sql::Integer(a["value"].as_str().unwrap().parse().unwrap()),
                Some("float") => Sql::Real(a["value"].as_f64().unwrap()),
                Some("text") => Sql::Text(a["value"].as_str().unwrap().into()),
                _ => Sql::Null,
            })
            .collect();
        let mut st = db.prepare(stmt["sql"].as_str().unwrap())?;
        let (n, reads) = (st.column_count(), st.readonly());
        let cols: Vec<Value> = st.column_names().iter().map(|c| json!({"name": c, "decltype": null})).collect();
        let mut rows = vec![];
        let mut q = st.query(rusqlite::params_from_iter(args))?;
        while let Some(r) = q.next()? {
            let mut row = vec![];
            for i in 0..n {
                row.push(match r.get_ref(i)? {
                    ValueRef::Integer(v) => json!({"type": "integer", "value": v.to_string()}),
                    ValueRef::Real(v) => json!({"type": "float", "value": v}),
                    ValueRef::Text(t) => json!({"type": "text", "value": String::from_utf8_lossy(t)}),
                    _ => json!({"type": "null"}),
                });
            }
            rows.push(Value::Array(row));
        }
        drop(q);
        drop(st);
        let id = if reads { Value::Null } else { json!(db.last_insert_rowid().to_string()) };
        Ok(json!({"cols": cols, "rows": rows, "affected_row_count": if reads { 0 } else { db.changes() }, "last_insert_rowid": id}))
    };
    let shared = db.clone();
    let pipeline = move |Json(body): Json<Value>| {
        let db = shared.clone();
        async move {
            let db = db.lock().unwrap();
            let results: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| match r["type"].as_str() {
                    Some("execute") => match execute(&db, &r["stmt"]) {
                        Ok(result) => json!({"type": "ok", "response": {"type": "execute", "result": result}}),
                        Err(e) => json!({"type": "error", "error": {"message": e.to_string(), "code": "SQLITE_ERROR"}}),
                    },
                    _ => json!({"type": "ok", "response": {"type": "close"}}),
                })
                .collect();
            Json(json!({"baton": null, "base_url": null, "results": results}))
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/v2/pipeline", post(pipeline))).await.unwrap() });
    (url, db)
}

pub fn hmac_hex(secret: &str, data: &[u8]) -> String {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(data);
    hex::encode(m.finalize().into_bytes())
}

/// A minimal stdio MCP server with one tool, `add`.
pub const MCP_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m: continue
    if m["method"] == "initialize": r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "t", "version": "1"}}
    elif m["method"] == "tools/list": r = {"tools": [{"name": "add", "description": "Add two numbers", "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}}]}
    elif m["method"] == "tools/call": a = m["params"]["arguments"]; r = {"content": [{"type": "text", "text": str(a["a"] + a["b"])}]}
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;
