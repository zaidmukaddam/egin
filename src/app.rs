//! Process-wide state shared by the loop, the tools, the server and the eval runners.

use crate::{model, store, tools::mcp};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, oneshot, OnceCell};

/// How unsafe tool calls get approved when the agent's `approval` is "ask".
#[derive(Clone, Copy, PartialEq)]
pub enum Approve {
    Auto,
    Stdin,
    Api,
    /// The session's host answers between runs: a run stops at the request (see `egin step`).
    Host,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub session: String,
    /// Row id in the event log; None for ephemeral events (token deltas).
    pub id: Option<i64>,
    pub body: Value,
}

pub struct App {
    pub store: store::Store,
    /// This process's name in the store: the runner of the sessions it is running.
    pub runner: String,
    pub http: reqwest::Client,
    pub events: broadcast::Sender<Event>,
    pub approve: Approve,
    pub key: String,
    pub base_url: String,
    pub search_url: String,
    /// Approvals (true/false) and questions (the answer) waiting on the HTTP API, by tool call id.
    pub pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    /// Sessions whose agent has hooks/: each stored event is passed to that session's hook runner.
    pub hooks: Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Value>>>,
    /// Live microVMs by session id.
    pub vms: tokio::sync::Mutex<HashMap<String, microsandbox::Sandbox>>,
    /// Daytona sandboxes by session id: (sandbox id, toolbox URL).
    pub daytona: tokio::sync::Mutex<HashMap<String, (String, String)>>,
    /// Connected MCP servers by "<agent dir>#<connection>".
    pub mcp: tokio::sync::Mutex<HashMap<String, Arc<mcp::Client>>>,
    /// EGIN_LOG=json: every stored event is also written to stderr as one JSON line.
    pub log_json: bool,
    /// OTLP/HTTP traces exporter from the OTEL_EXPORTER_OTLP_* environment, if set.
    pub otlp: Option<crate::telemetry::Exporter>,
    models: OnceCell<HashMap<String, model::ModelInfo>>,
    gateway_models: OnceCell<HashMap<String, model::ModelInfo>>,
    /// E2B and Vercel sandboxes by "<backend>:<session id>": the handles their APIs need.
    pub sandboxes: tokio::sync::Mutex<HashMap<String, Value>>,
    /// A helper's session id to the session whose sandbox it works in (see the `helper` tool).
    pub sandbox_of: Mutex<HashMap<String, String>>,
    /// When each running session's current run began, so a helper can be held to its parent's deadline.
    pub run_started: Mutex<HashMap<String, std::time::Instant>>,
    /// `egin step`: how far one session's run may go before it pauses.
    pub budget: Mutex<Option<Budget>>,
}

/// What a run of `session` may still do: `calls` more model calls, none of them started after `until`.
pub struct Budget {
    pub session: String,
    pub calls: u64,
    pub until: Option<std::time::Instant>,
}

impl App {
    /// Configured from the environment: YDC_API_KEY, EGIN_BASE_URL, EGIN_SEARCH_URL.
    pub fn new(db: &Path, approve: Approve) -> anyhow::Result<Arc<App>> {
        let key = std::env::var("YDC_API_KEY").unwrap_or_default();
        let base = crate::setting("BASE_URL").unwrap_or_else(|| "https://api.you.com/v1".into());
        let search = crate::setting("SEARCH_URL").unwrap_or_else(|| "https://ydc-index.io/v1".into());
        App::with(db, approve, &key, &base, &search)
    }

    pub fn with(db: &Path, approve: Approve, key: &str, base_url: &str, search_url: &str) -> anyhow::Result<Arc<App>> {
        App::with_store(store::Store::open(db)?, approve, key, base_url, search_url)
    }

    pub fn with_store(store: store::Store, approve: Approve, key: &str, base_url: &str, search_url: &str) -> anyhow::Result<Arc<App>> {
        Ok(Arc::new(App {
            store,
            runner: new_id(),
            http: reqwest::Client::new(),
            events: broadcast::channel(4096).0,
            approve,
            key: key.into(),
            base_url: base_url.trim_end_matches('/').into(),
            search_url: search_url.trim_end_matches('/').into(),
            pending: Mutex::new(HashMap::new()),
            hooks: Mutex::new(HashMap::new()),
            vms: tokio::sync::Mutex::new(HashMap::new()),
            daytona: tokio::sync::Mutex::new(HashMap::new()),
            mcp: tokio::sync::Mutex::new(HashMap::new()),
            log_json: crate::setting("LOG").is_some_and(|v| v == "json"),
            otlp: crate::telemetry::Exporter::from_env(),
            models: OnceCell::new(),
            gateway_models: OnceCell::new(),
            sandboxes: tokio::sync::Mutex::new(HashMap::new()),
            sandbox_of: Mutex::new(HashMap::new()),
            run_started: Mutex::new(HashMap::new()),
            budget: Mutex::new(None),
        }))
    }

    /// Configured from the environment like [`App::new`], with a store that lives only in memory:
    /// for `egin step`, whose host keeps the session.
    pub fn stateless(approve: Approve) -> anyhow::Result<Arc<App>> {
        let key = std::env::var("YDC_API_KEY").unwrap_or_default();
        let base = crate::setting("BASE_URL").unwrap_or_else(|| "https://api.you.com/v1".into());
        let search = crate::setting("SEARCH_URL").unwrap_or_else(|| "https://ydc-index.io/v1".into());
        App::with_store(store::Store::memory()?, approve, &key, &base, &search)
    }

    /// Record an event in the session log and broadcast it to live listeners.
    pub fn emit(&self, session: &str, kind: &str, mut body: Value) {
        // Tool-call argument deltas only tell a capped model call that its stream is alive; nothing shows them.
        if kind == "tool_args" {
            return;
        }
        body["type"] = json!(kind);
        // When it happened, for traces: a tool call's time is from its tool_call to its tool_result.
        body["at"] = json!((crate::telemetry::nanos() / 1_000_000) as u64);
        let id = match kind {
            "token" | "reasoning" => None,
            _ => self.store.push_event(session, &body).ok(),
        };
        if id.is_some() {
            if let Some(tx) = self.hooks.lock().unwrap().get(session) {
                let _ = tx.send(body.clone());
            }
        }
        if self.log_json && id.is_some() {
            let ms = crate::telemetry::nanos() / 1_000_000;
            eprintln!("{}", json!({"ts_ms": ms as u64, "session": session, "event_id": id, "event": body}));
        }
        let _ = self.events.send(Event { session: session.into(), id, body });
    }

    /// Context limit and pricing from the router's model list, fetched once.
    pub async fn model_info(&self, model: &str) -> Option<model::ModelInfo> {
        let (provider, id) = model::route(model);
        if let Some(info) = model::is_claude(&id).then(|| model::claude_info(&id)).flatten() {
            return Some(info);
        }
        // Gateway ids are priced from the Gateway's list, and so are OpenAI and Gemini models, which
        // list no prices of their own (the Gateway and Bedrock charge the providers' rates).
        let gateway_id = match provider {
            "gateway" => Some(id.clone()),
            "openai" => Some(format!("openai/{id}")),
            "gemini" => Some(format!("google/{id}")),
            "bedrock" => id.strip_prefix("openai.").map(|m| format!("openai/{m}")),
            _ => None,
        };
        if let Some(gid) = gateway_id {
            let all = self.gateway_models.get_or_init(|| async { model::fetch_gateway_models(self).await.unwrap_or_default() }).await;
            return all.get(&gid).cloned();
        }
        let all = self.models.get_or_init(|| async { model::fetch_models(self).await.unwrap_or_default() }).await;
        all.get(model).cloned()
    }

    /// A model that takes audio input: `model` itself if it does, else the router's cheapest one.
    pub async fn hearing_model(&self, model: &str) -> Option<String> {
        let hears = |i: &model::ModelInfo| i.inputs.iter().any(|x| x == "audio");
        if self.model_info(model).await.is_some_and(|i| hears(&i)) {
            return Some(model.to_string());
        }
        let all = self.models.get_or_init(|| async { model::fetch_models(self).await.unwrap_or_default() }).await;
        all.iter().filter(|(_, i)| hears(i)).min_by(|a, b| a.1.input.total_cmp(&b.1.input).then(a.0.cmp(b.0))).map(|(m, _)| m.clone())
    }
}

/// Load KEY=VALUE lines from .env.local and .env without overriding the real environment.
/// Call before starting any threads (it sets process environment variables).
pub fn load_env() {
    for file in [".env.local", ".env"] {
        for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
            if let Some((k, v)) = line.trim().trim_start_matches("export ").split_once('=') {
                if !k.starts_with('#') && std::env::var(k).is_err() {
                    std::env::set_var(k.trim(), v.trim().trim_matches(['"', '\'']));
                }
            }
        }
    }
}

/// A short, time-ordered id: hex microseconds, a counter, and four digits drawn once per process, so
/// processes sharing a store do not hand out the same id in the same microsecond.
pub fn new_id() -> String {
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    static PROCESS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let process = PROCESS.get_or_init(|| std::collections::hash_map::RandomState::new().hash_one(std::process::id()) & 0xffff);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    format!("{:x}{:02x}{:04x}", t, N.fetch_add(1, Ordering::Relaxed) % 256, process)
}
