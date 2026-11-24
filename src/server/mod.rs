//! HTTP API: sessions, a server-sent event stream, approvals, cancel.

pub mod channels;
pub mod mcp;

use crate::{
    agent::{run, Agent},
    App,
};
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event as Sse, KeepAlive, Sse as SseResponse},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::{self, Stream};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex},
};
use tokio::{sync::broadcast::error::RecvError, task::AbortHandle};

pub struct Srv {
    pub app: Arc<App>,
    pub agent: Arc<Agent>,
    tasks: Mutex<HashMap<String, AbortHandle>>,
}

impl Srv {
    /// Run a session as a tracked (cancellable) task and wait for its answer.
    pub async fn run(&self, sid: &str) -> Result<String> {
        self.app.store.claim(sid, &self.app.runner)?;
        let task = tokio::spawn(run::run_session(self.app.clone(), agent_for(self, sid), sid.to_string()));
        self.tasks.lock().unwrap().insert(sid.to_string(), task.abort_handle());
        let out = task.await;
        self.tasks.lock().unwrap().remove(sid);
        out?
    }
}

type S = State<Arc<Srv>>;

/// The largest file `POST /sessions/{id}/files` accepts.
const MAX_UPLOAD: usize = 25 * 1024 * 1024;
type Reply = Result<Json<Value>, (StatusCode, String)>;

fn err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// A session with its own model or reasoning effort runs the agent loaded with them, so the model's
/// profile and defaults apply as they would in agent.toml. A failed load falls back to the agent.
fn agent_for(srv: &Srv, sid: &str) -> Arc<Agent> {
    match srv.app.store.settings(sid) {
        Ok(Some(mut s)) => {
            // The schema is the session's, read by the loop; it is not an agent.toml key.
            s.remove("output_schema");
            if s.is_empty() {
                return srv.agent.clone();
            }
            Agent::load(&srv.agent.dir, &s).map(Arc::new).unwrap_or_else(|_| srv.agent.clone())
        }
        _ => srv.agent.clone(),
    }
}

pub(crate) fn spawn(srv: &Arc<Srv>, sid: &str) {
    // Marked running now, not when the task starts, so a message sent right after joins this run
    // instead of starting a second one.
    let _ = srv.app.store.claim(sid, &srv.app.runner);
    let task = tokio::spawn(run::run_session(srv.app.clone(), agent_for(srv, sid), sid.to_string()));
    srv.tasks.lock().unwrap().insert(sid.to_string(), task.abort_handle());
}

/// Whether this process is running the session right now.
fn runs_here(srv: &Srv, sid: &str) -> bool {
    srv.tasks.lock().unwrap().get(sid).is_some_and(|t| !t.is_finished())
}

/// Mark a session's running subagents cancelled, and theirs in turn. They run inside the parent's
/// task, so aborting it already stopped them; only their status is left to set.
fn cancel_subagents(app: &App, sid: &str, why: &str) -> Result<()> {
    for child in app.store.children(sid)? {
        if app.store.status(&child)?.as_deref() == Some("running") {
            app.store.set_status(&child, "cancelled")?;
            app.emit(&child, "cancelled", json!({"reason": why}));
        }
        cancel_subagents(app, &child, why)?;
    }
    Ok(())
}

/// Sessions that were mid-run when the server died pick up where their log ends.
pub fn resume_all(srv: &Arc<Srv>) -> Result<()> {
    for sid in srv.app.store.with_status("running")? {
        if adopt(srv, &sid)? {
            continue;
        }
        // Its lease is fresh: another process may be running it. Look again once it would be stale.
        let age = srv.app.store.lease_age(&sid)?.unwrap_or(0);
        let srv = srv.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs((run::LEASE_STALE_SECS - age).max(0) as u64 + 1)).await;
            if let Err(e) = adopt(&srv, &sid) {
                eprintln!("egin: resuming session {sid}: {e:#}");
            }
        });
    }
    Ok(())
}

/// Take over a running session whose runner stopped renewing its lease: its process died, or it is
/// a serverless instance that was frozen or recycled. The run picks up where the log ends. Returns
/// whether this process took it; only one of several asking at once does, because taking the lease
/// is one statement in the store. A subagent's session is left to its parent: the parent's
/// interrupted `task` call tells its model, which can start the subagent again. A channel session
/// still sends its reply when it finishes.
pub(crate) fn adopt(srv: &Arc<Srv>, sid: &str) -> Result<bool> {
    let app = &srv.app;
    if runs_here(srv, sid) || !app.store.take_over(sid, &app.runner, run::LEASE_STALE_SECS)? {
        return Ok(false);
    }
    if app.store.parent(sid)?.is_some() {
        app.store.set_status(sid, "cancelled")?;
        app.emit(sid, "cancelled", json!({"reason": "egin restarted; the parent session decides whether to run it again"}));
        return Ok(true);
    }
    eprintln!("resuming session {sid}");
    match app.store.thread_of(sid)? {
        Some(thread) => {
            let srv = srv.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                let answer = srv.run(&sid).await;
                channels::deliver(&srv, &thread, &sid, answer).await;
            });
        }
        None => spawn(srv, sid),
    }
    Ok(true)
}

/// `POST /resume`: take over every running session whose runner is gone. `egin serve` does this when
/// it starts, which is enough for a process that stays up. A serverless host has no such process, so
/// a run that died with its instance waits for a request about that session, or for this one from a
/// cron job.
async fn resume_now(State(srv): S) -> Reply {
    let mut resumed = 0;
    for sid in srv.app.store.with_status("running").map_err(err)? {
        resumed += adopt(&srv, &sid).map_err(err)? as usize;
    }
    Ok(Json(json!({"resumed": resumed})))
}

/// The models a client may pick: the agent's own first, then agent.toml's `models`.
fn choices(agent: &Agent) -> Vec<String> {
    let mut v = vec![agent.cfg.model.clone()];
    v.extend(agent.cfg.models.iter().filter(|m| **m != agent.cfg.model).cloned());
    v
}

/// `model` and `reasoning_effort` in a create or message body: checked against what the agent offers
/// and what the model accepts, then kept with the session for this and later turns.
fn choose(srv: &Srv, sid: &str, body: &Value) -> Result<(), (StatusCode, String)> {
    // `output_schema`: a JSON Schema for this and later turns' answers; null clears it.
    if let Some(s) = body.get("output_schema") {
        crate::agent::output::set(&srv.app, sid, Some(s).filter(|s| !s.is_null())).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    }
    let mut settings = srv.app.store.settings(sid).map_err(err)?.unwrap_or_default();
    if let Some(m) = body["model"].as_str() {
        if !choices(&srv.agent).iter().any(|c| c == m) {
            return Err((StatusCode::BAD_REQUEST, format!("model {m:?} is not offered; add it to `models` in agent.toml")));
        }
        settings.insert("model".into(), m.into());
        // An effort the new model does not take is dropped rather than sent.
        let model = m.to_string();
        if settings.get("reasoning_effort").and_then(|e| e.as_str()).is_some_and(|e| !crate::model::reasoning_efforts(&model).contains(&e)) {
            settings.remove("reasoning_effort");
        }
    }
    if let Some(e) = body["reasoning_effort"].as_str() {
        let model = settings.get("model").and_then(|m| m.as_str()).unwrap_or(&srv.agent.cfg.model).to_string();
        let efforts = crate::model::reasoning_efforts(&model);
        if !efforts.contains(&e) {
            return Err((StatusCode::BAD_REQUEST, format!("{model} takes reasoning_effort {}", efforts.join(", "))));
        }
        settings.insert("reasoning_effort".into(), e.into());
    }
    if !settings.is_empty() {
        srv.app.store.set_settings(sid, &settings).map_err(err)?;
    }
    Ok(())
}

/// `GET /agent`: what a client can offer: the models to pick from, and each one's reasoning efforts.
async fn agent_info(State(srv): S) -> Reply {
    Ok(Json(info(&srv.agent)))
}

/// What a client can offer for an agent: the models to pick from, and each one's reasoning efforts.
pub fn info(agent: &Agent) -> Value {
    let models = choices(agent);
    let efforts: serde_json::Map<String, Value> = models.iter().map(|m| (m.clone(), json!(crate::model::reasoning_efforts(m)))).collect();
    json!({
        "model": agent.cfg.model,
        "models": models,
        "reasoning_effort": agent.cfg.reasoning_effort,
        "reasoning_efforts": efforts,
    })
}

/// `files` in a message body: uploads of this session, as returned by the files route.
fn attachments(srv: &Srv, sid: &str, body: &Value) -> Result<Vec<Value>, (StatusCode, String)> {
    let prefix = format!("{}/uploads/{sid}/", crate::sandbox::workdir(&srv.agent));
    let files: Vec<Value> = body["files"].as_array().cloned().unwrap_or_default();
    for f in &files {
        if !f["path"].as_str().is_some_and(|p| p.starts_with(&prefix) && !p.contains("..")) {
            return Err((StatusCode::BAD_REQUEST, "files must be uploads of this session".into()));
        }
    }
    Ok(files)
}

/// The user a request acts for: `X-Egin-User`, set by the app's server after its own sign-in check.
/// Without it a request is the operator's and reaches every session.
fn user(headers: &axum::http::HeaderMap) -> Option<String> {
    headers.get("x-egin-user").and_then(|v| v.to_str().ok()).map(str::trim).filter(|u| !u.is_empty()).map(String::from)
}

/// Every `/sessions/{id}` route: a user reaches only their own sessions. Someone else's answers 404,
/// the same as one that does not exist.
async fn owned(State(srv): S, Path(p): Path<HashMap<String, String>>, req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let (Some(u), Some(sid)) = (user(req.headers()), p.get("id")) {
        match srv.app.store.owner(sid) {
            Ok(Some(Some(o))) if o == u => {}
            Ok(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
            Err(e) => return err(e).into_response(),
        }
    }
    next.run(req).await
}

/// `GET /sessions`: the caller's sessions, newest first, titled by their first message.
async fn list(State(srv): S, headers: axum::http::HeaderMap, Query(q): Query<HashMap<String, String>>) -> Reply {
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50).min(500);
    let sessions = srv.app.store.list_sessions(user(&headers).as_deref(), limit).map_err(err)?;
    Ok(Json(json!({"sessions": sessions})))
}

async fn create(State(srv): S, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> Reply {
    // The body is optional: no body (or `{}`) creates an empty session.
    let body: Value = serde_json::from_slice(&body).unwrap_or_default();
    let sid = run::new_session(&srv.app, &srv.agent).map_err(err)?;
    if let Some(u) = user(&headers) {
        srv.app.store.set_owner(&sid, &u).map_err(err)?;
    }
    choose(&srv, &sid, &body)?;
    if let Some(text) = body["message"].as_str() {
        run::send(&srv.app, &sid, text).map_err(err)?;
        spawn(&srv, &sid);
    }
    Ok(Json(json!({"id": sid})))
}

/// A message to an idle session starts a run. One sent while it runs joins that run, as the agent's
/// `mid_run` says, and answers `{"queued": true}`.
async fn message(State(srv): S, Path(sid): Path<String>, Json(body): Json<Value>) -> Reply {
    let Some(status) = srv.app.store.status(&sid).map_err(err)? else { return Err((StatusCode::NOT_FOUND, "no such session".into())) };
    let Some(text) = body["message"].as_str() else { return Err((StatusCode::BAD_REQUEST, "`message` is required".into())) };
    let running = status == "running";
    // A model change takes effect from the next run, so it is refused mid-run rather than half-applied.
    if running && ["model", "reasoning_effort", "output_schema"].iter().any(|k| body.get(k).is_some()) {
        return Err((StatusCode::CONFLICT, "the session is running; change model, reasoning_effort or output_schema once it is done".into()));
    }
    choose(&srv, &sid, &body)?;
    let files = attachments(&srv, &sid, &body)?;
    let images = if crate::tools::sees_images(&srv.app, &srv.agent).await { inline_images(&srv, &sid, &files) } else { vec![] };
    // Starting the run is one statement in the store, so two processes handed a message for the same
    // idle session cannot both start one: the second queues its message for the first one's run.
    if !running && srv.app.store.start_run(&sid, &srv.app.runner).map_err(err)? {
        if let Err(e) = run::send_with(&srv.app, &sid, text, &files, images) {
            let _ = srv.app.store.set_status(&sid, &status);
            return Err(err(e));
        }
        spawn(&srv, &sid);
        return Ok(Json(json!({"ok": true})));
    }
    run::enqueue(&srv.app, &sid, text, &files, images).map_err(err)?;
    // The run may have ended between the status check and the enqueue; then nothing would pick the
    // message up, so it starts a run of its own. Taking from the inbox is atomic, so a message is
    // never delivered twice.
    if srv.app.store.status(&sid).map_err(err)?.as_deref() != Some("running") && run::deliver_inbox(&srv.app, &sid, 1000).map_err(err)? > 0 {
        spawn(&srv, &sid);
    }
    // The run it joined may have died with its process: then this one continues it, message included.
    adopt(&srv, &sid).map_err(err)?;
    Ok(Json(json!({"ok": true, "queued": true})))
}

/// Attached images under 5 MB as `image_url` parts, read from the upload folder on the host.
/// Larger ones, and every other file, reach the model as paths for read_file.
fn inline_images(srv: &Srv, sid: &str, files: &[Value]) -> Vec<Value> {
    use base64::Engine;
    let dir = srv.agent.workspace.join("uploads").join(sid);
    files
        .iter()
        .filter_map(|f| {
            let name = f["path"].as_str()?.rsplit('/').next()?;
            let crate::tools::docs::Kind::Image(mime) = crate::tools::docs::kind(name) else { return None };
            let bytes = std::fs::read(dir.join(name)).ok().filter(|b| b.len() <= 5 * 1024 * 1024)?;
            let url = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        })
        .collect()
}

/// The file's name made safe: its last path component, with anything but letters, digits,
/// dots, dashes and underscores replaced.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base.chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() {
        "file".into()
    } else {
        s
    }
}

/// `POST /sessions/{id}/files?name=report.pdf` with the raw file as the body. The file lands in
/// the agent's workspace under uploads/<session>/, where the session's VM can read it.
async fn upload(State(srv): S, Path(sid): Path<String>, Query(q): Query<HashMap<String, String>>, data: axum::body::Bytes) -> Reply {
    if srv.app.store.status(&sid).map_err(err)?.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such session".into()));
    }
    let name = safe_name(q.get("name").map(String::as_str).unwrap_or("file"));
    let dir = srv.agent.workspace.join("uploads").join(&sid);
    std::fs::create_dir_all(&dir).map_err(err)?;
    // Never overwrite an earlier upload with the same name.
    let mut file = name.clone();
    let mut n = 1;
    while dir.join(&file).exists() {
        n += 1;
        file = format!("{n}-{name}");
    }
    std::fs::write(dir.join(&file), &data).map_err(err)?;
    let path = format!("{}/uploads/{sid}/{file}", crate::sandbox::workdir(&srv.agent));
    Ok(Json(json!({"name": file, "path": path, "size": data.len()})))
}

async fn show(State(srv): S, Path(sid): Path<String>) -> Reply {
    let store = &srv.app.store;
    let Some(status) = store.status(&sid).map_err(err)? else { return Err((StatusCode::NOT_FOUND, "no such session".into())) };
    if status == "running" {
        adopt(&srv, &sid).map_err(err)?;
    }
    let messages: Vec<Value> = store.messages(&sid).map_err(err)?.into_iter().map(|(_, m)| m).collect();
    Ok(Json(json!({"id": sid, "status": status, "stats": store.stats(&sid).map_err(err)?, "messages": messages})))
}

/// The session as an ATIF trajectory.
async fn trajectory(State(srv): S, Path(sid): Path<String>) -> Reply {
    if srv.app.store.status(&sid).map_err(err)?.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such session".into()));
    }
    Ok(Json(crate::atif::export(&srv.app, &sid).map_err(err)?))
}

/// Hand an answer to the run waiting on a request (`approval_request` or `question`) of this session.
fn answer(srv: &Srv, sid: &str, call: &str, kind: &str, value: Value) -> Reply {
    // The call must belong to this session, so an answer cannot reach into another one.
    let Some(request) = srv.app.store.find_event(sid, kind, call).map_err(err)? else {
        return Err((StatusCode::NOT_FOUND, format!("no {kind} pending for that call_id")));
    };
    let waiting = srv.app.pending.lock().unwrap().remove(call);
    if let Some(tx) = waiting {
        let _ = tx.send(value);
        return Ok(Json(json!({"ok": true})));
    }
    // No run is waiting on it in this process right now: egin restarted, and the session has not been
    // picked up again yet. The answer is stored, and the run uses it when it reaches the call.
    let (stored, body) = match kind {
        "question" => ("answer", json!({"call_id": call, "answer": value})),
        _ => ("approval", json!({"call_id": call, "tool": request["tool"], "allow": value})),
    };
    if srv.app.store.find_event(sid, stored, call).map_err(err)?.is_some() {
        return Err((StatusCode::CONFLICT, "that call was already answered".into()));
    }
    srv.app.emit(sid, stored, body);
    Ok(Json(json!({"ok": true, "stored": true})))
}

async fn approve(State(srv): S, Path(sid): Path<String>, Json(body): Json<Value>) -> Reply {
    answer(&srv, &sid, body["call_id"].as_str().unwrap_or(""), "approval_request", json!(body["allow"].as_bool().unwrap_or(false)))
}

/// `POST /sessions/{id}/answers` with `{"call_id", "answer"}`: the reply to an `ask_user` question.
async fn answers(State(srv): S, Path(sid): Path<String>, Json(body): Json<Value>) -> Reply {
    let Some(text) = body["answer"].as_str() else { return Err((StatusCode::BAD_REQUEST, "`answer` is required".into())) };
    answer(&srv, &sid, body["call_id"].as_str().unwrap_or(""), "question", json!(text))
}

/// Stop the session if it runs, remove its VM, and delete its rows.
async fn delete(State(srv): S, Path(sid): Path<String>) -> Reply {
    if srv.app.store.status(&sid).map_err(err)?.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such session".into()));
    }
    if let Some(task) = srv.tasks.lock().unwrap().remove(&sid) {
        task.abort();
    }
    crate::sandbox::remove(&srv.app, &srv.agent.cfg.sandbox, &sid).await;
    srv.app.store.delete_session(&sid).map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

async fn cancel(State(srv): S, Path(sid): Path<String>) -> Reply {
    let task = srv.tasks.lock().unwrap().remove(&sid);
    match task {
        Some(task) => task.abort(),
        // Another process is running it: the status change below reaches its runner at the next lease
        // renewal, and the run stops there.
        None if srv.app.store.status(&sid).map_err(err)?.as_deref() == Some("running") => {}
        None => return Err((StatusCode::NOT_FOUND, "session is not running".into())),
    }
    srv.app.store.set_status(&sid, "cancelled").map_err(err)?;
    srv.app.emit(&sid, "cancelled", json!({}));
    cancel_subagents(&srv.app, &sid, "its parent session was cancelled").map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

/// `GET /sessions/{id}/history?before=<event id>&limit=500`: a page of the stored log, oldest first,
/// starting at a turn. `earlier` is the `before` for the page ahead of it, or null at the start.
async fn history(State(srv): S, Path(sid): Path<String>, Query(q): Query<HashMap<String, String>>) -> Reply {
    if srv.app.store.status(&sid).map_err(err)?.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such session".into()));
    }
    let before = q.get("before").and_then(|b| b.parse().ok());
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(500).clamp(1, 5000);
    let page = srv.app.store.events_page(&sid, before, limit).map_err(err)?;
    let first = page.first().map(|(id, _)| *id);
    let earlier = match first {
        Some(id) if !srv.app.store.events_page(&sid, Some(id), 1).map_err(err)?.is_empty() => json!(id),
        _ => Value::Null,
    };
    let events: Vec<Value> = page
        .into_iter()
        .map(|(id, mut e)| {
            e["id"] = json!(id);
            e
        })
        .collect();
    Ok(Json(json!({"events": events, "earlier": earlier})))
}

/// One open event stream: what it has sent, and where the rest comes from.
struct Follow {
    srv: Arc<Srv>,
    sid: String,
    rx: tokio::sync::broadcast::Receiver<crate::Event>,
    /// Stored events read from the log and not sent yet.
    backlog: VecDeque<(i64, Value)>,
    /// Every stored event up to this id has been sent.
    last: i64,
    poll: tokio::time::Interval,
    /// Polls since the log last had something new.
    quiet: u32,
}

impl Follow {
    fn catch_up(&mut self) {
        self.backlog = self.srv.app.store.events_after(&self.sid, self.last).unwrap_or_default().into();
    }
}

/// Replays the stored log after `?after=<event id>`, then follows live events. The log is the source
/// of order: this process's own events arrive on the live channel as they are stored, and a session
/// another process is running is read from the store every second, with whole messages in place of
/// token deltas. A stream is also what notices a run whose process died: it takes the run over.
async fn events(
    State(srv): S,
    Path(sid): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<SseResponse<impl Stream<Item = Result<Sse, Infallible>>>, (StatusCode, String)> {
    let after: i64 = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    // Subscribe before reading the backlog so nothing falls between the two.
    let rx = srv.app.events.subscribe();
    let backlog: VecDeque<(i64, Value)> = srv.app.store.events_after(&sid, after).map_err(err)?.into();
    let mut poll = tokio::time::interval(std::time::Duration::from_secs(1));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let follow = Follow { srv, sid, rx, backlog, last: after, poll, quiet: 0 };
    let stream = stream::unfold(follow, |mut f| async move {
        loop {
            while let Some((id, body)) = f.backlog.pop_front() {
                if id > f.last {
                    f.last = id;
                    return Some((Ok(Sse::default().id(id.to_string()).data(body.to_string())), f));
                }
            }
            let here = runs_here(&f.srv, &f.sid);
            tokio::select! {
                // The live channel first: what it holds happened before anything a later read of the
                // log would return, and token deltas must reach the client ahead of their message.
                biased;
                e = f.rx.recv() => match e {
                    Ok(e) if e.session != f.sid => continue,
                    Ok(e) => match e.id {
                        None => return Some((Ok(Sse::default().data(e.body.to_string())), f)),
                        Some(id) if id <= f.last => continue,
                        Some(id) if id == f.last + 1 => {
                            f.last = id;
                            return Some((Ok(Sse::default().id(id.to_string()).data(e.body.to_string())), f));
                        }
                        // Ids are shared by every session in the store, so a jump may hide an event
                        // another process stored for this one: read what the log holds before this
                        // event, and no further, so nothing overtakes the deltas still in the channel.
                        // ponytail: one read per event while several sessions are busy; a per-session
                        // sequence number would tell a real gap from another session's events.
                        Some(id) => {
                            f.catch_up();
                            f.backlog.retain(|(earlier, _)| *earlier < id);
                            f.backlog.push_back((id, e.body));
                        }
                    },
                    // Fell behind the live channel: catch up from the log (token deltas are lost).
                    Err(RecvError::Lagged(_)) => f.catch_up(),
                    Err(RecvError::Closed) => return None,
                },
                _ = f.poll.tick(), if !here => {
                    f.quiet += 1;
                    // After half a minute with nothing new, look every third second.
                    if f.quiet > 30 && f.quiet % 3 != 0 {
                        continue;
                    }
                    if f.quiet % 5 == 0 {
                        let _ = adopt(&f.srv, &f.sid);
                    }
                    f.catch_up();
                    if !f.backlog.is_empty() {
                        f.quiet = 0;
                    }
                }
            }
        }
    });
    Ok(SseResponse::new(stream).keep_alive(KeepAlive::default()))
}

/// Byte comparison whose time does not depend on where the inputs differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// With a token, the session routes need `Authorization: Bearer <token>`. Channel routes carry
/// their own signatures and stay open.
pub fn router(app: Arc<App>, agent: Arc<Agent>, token: Option<String>) -> (Router, Arc<Srv>) {
    let srv = Arc::new(Srv { app, agent, tasks: Mutex::new(HashMap::new()) });
    let by_id = Router::new()
        .route("/sessions/{id}", get(show).delete(delete))
        .route("/sessions/{id}/messages", post(message))
        .route("/sessions/{id}/events", get(events))
        .route("/sessions/{id}/trajectory", get(trajectory))
        .route("/sessions/{id}/history", get(history))
        .route("/sessions/{id}/approvals", post(approve))
        .route("/sessions/{id}/answers", post(answers))
        .route("/sessions/{id}/cancel", post(cancel))
        .route("/sessions/{id}/files", post(upload).layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD)))
        .route_layer(axum::middleware::from_fn_with_state(srv.clone(), owned));
    let mut api = Router::new().route("/sessions", get(list).post(create)).route("/agent", get(agent_info)).route("/resume", post(resume_now)).merge(by_id);
    // The MCP endpoint starts runs like the session routes do, so it sits behind the same token.
    if srv.agent.channels.contains_key("mcp") {
        api = api.route("/mcp", post(mcp::post).get(mcp::get));
    }
    if let Some(token) = token {
        let want = format!("Bearer {token}");
        api = api.layer(axum::middleware::from_fn(move |req: axum::extract::Request, next: axum::middleware::Next| {
            let ok = req.headers().get("authorization").is_some_and(|v| same(v.as_bytes(), want.as_bytes()));
            async move {
                if ok {
                    next.run(req).await
                } else {
                    axum::response::IntoResponse::into_response((StatusCode::UNAUTHORIZED, "missing or wrong bearer token"))
                }
            }
        }));
    }
    // `/health` takes no token: a host's health check has none to send, and it says nothing but "ok".
    let router = api.merge(channels::routes(&srv)).route("/health", get(|| async { "ok" })).with_state(srv.clone());
    (router, srv)
}

pub async fn serve(app: Arc<App>, agent: Arc<Agent>, host: &str, port: u16) -> Result<()> {
    let token = crate::setting("TOKEN").filter(|t| !t.is_empty());
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback && token.is_none() {
        anyhow::bail!("set EGIN_TOKEN before serving on {host}: anyone who can reach the port could run the agent");
    }
    let (router, srv) = router(app.clone(), agent.clone(), token);
    channels::socket_mode(srv.clone());
    channels::board_follow(srv.clone());
    resume_all(&srv)?;
    for s in agent.schedules.clone() {
        let srv = srv.clone();
        tokio::spawn(async move {
            loop {
                let wait = s.next(chrono::Local::now()).unwrap_or(std::time::Duration::from_secs(3600));
                tokio::time::sleep(wait).await;
                let started = run::new_session(&srv.app, &srv.agent).and_then(|sid| run::send(&srv.app, &sid, &s.prompt).map(|_| sid));
                match started {
                    Ok(sid) => {
                        eprintln!("schedule {} started session {sid}", s.name);
                        spawn(&srv, &sid);
                    }
                    Err(e) => eprintln!("schedule {}: {e:#}", s.name),
                }
            }
        });
    }
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    eprintln!("egin serving {} on http://{}", agent.dir.display(), listener.local_addr()?);
    Ok(axum::serve(listener, router).await?)
}
