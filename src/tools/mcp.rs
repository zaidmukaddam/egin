//! MCP servers as tools. connections/<name>.toml holds either
//!   command = ["npx", "-y", "some-mcp"]   (stdio, spawned on the host) plus optional env
//! or
//!   url = "https://host/mcp"              (streamable HTTP) plus optional headers
//! A server that signs in with OAuth takes `auth = "oauth"` and `egin connect <name>` (see oauth.rs).
//! `${VAR}` in env and header values reads the host environment. Tools appear to the
//! model as `<connection>__<tool>`; list read-only ones in `safe = [...]` to skip approval.

use crate::{agent::Agent, App};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub safe: Vec<String>,
    /// "oauth": sign in with `egin connect <name>`; the token is sent on every request.
    pub auth: Option<String>,
    /// A client registered by hand, for an authorization server without dynamic registration.
    pub client_id: Option<String>,
}

impl Connection {
    pub fn oauth(&self) -> Result<bool> {
        match self.auth.as_deref() {
            None => Ok(false),
            Some("oauth") if self.url.is_some() => Ok(true),
            Some("oauth") => bail!("connection {}: auth = \"oauth\" needs a `url`; a stdio server takes its credentials from `env`", self.name),
            Some(other) => bail!("connection {}: unknown auth {other:?}; the only one is \"oauth\"", self.name),
        }
    }
}

#[allow(clippy::large_enum_variant)] // one per connection; boxing buys nothing
enum Transport {
    Stdio {
        _child: Child,
        stdin: ChildStdin,
        lines: Lines<BufReader<ChildStdout>>,
    },
    /// `oauth` is the token file when the connection signs in.
    Http {
        url: String,
        headers: Vec<(String, String)>,
        session: Option<String>,
        oauth: Option<PathBuf>,
    },
}

/// POST one message, with the OAuth token when there is one; a 401 refreshes it and retries once.
async fn post(app: &App, url: &str, headers: &[(String, String)], session: &Option<String>, oauth: &Option<PathBuf>, msg: &Value) -> Result<reqwest::Response> {
    let mut refused = false;
    loop {
        let mut r = app.http.post(url).json(msg).header("accept", "application/json, text/event-stream").timeout(Duration::from_secs(120));
        for (k, v) in headers {
            r = r.header(k, v);
        }
        if let Some(s) = session {
            r = r.header("mcp-session-id", s.as_str());
        }
        if let Some(file) = oauth {
            r = r.bearer_auth(super::oauth::access_token(&app.http, file, refused).await?);
        }
        let resp = r.send().await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && oauth.is_some() && !refused {
            refused = true;
            continue;
        }
        return Ok(resp);
    }
}

pub struct Client {
    t: Mutex<(Transport, u64)>,
    pub tools: Vec<Value>,
}

/// `${VAR}` → host env value.
pub fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out += &rest[..i];
        match rest[i..].find('}') {
            Some(j) => {
                out += &std::env::var(&rest[i + 2..i + j]).unwrap_or_default();
                rest = &rest[i + j + 1..];
            }
            None => {
                out += &rest[i..];
                rest = "";
            }
        }
    }
    out + rest
}

impl Client {
    async fn connect(app: &App, agent_dir: &Path, c: &Connection) -> Result<Client> {
        let t = if let Some(url) = &c.url {
            let headers = c.headers.iter().map(|(k, v)| (k.clone(), expand(v))).collect();
            let oauth = c.oauth()?.then(|| super::oauth::file(agent_dir, &c.name));
            // Some servers take their key in the URL, so `${VAR}` works there too.
            Transport::Http { url: expand(url), headers, session: None, oauth }
        } else if let Some((cmd, args)) = c.command.split_first() {
            let mut child = tokio::process::Command::new(cmd)
                .args(args)
                .envs(c.env.iter().map(|(k, v)| (k, expand(v))))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("starting MCP server {}", c.name))?;
            let stdin = child.stdin.take().unwrap();
            let lines = BufReader::new(child.stdout.take().unwrap()).lines();
            Transport::Stdio { _child: child, stdin, lines }
        } else {
            bail!("connection {} needs `command` or `url`", c.name);
        };
        let mut client = Client { t: Mutex::new((t, 0)), tools: vec![] };
        let init = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "egin", "version": env!("CARGO_PKG_VERSION")}});
        client.request(app, "initialize", init).await?;
        client.notify(app, "notifications/initialized").await?;
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
            let page = client.request(app, "tools/list", params).await?;
            client.tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            match page["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.into()),
                _ => break,
            }
        }
        Ok(client)
    }

    async fn notify(&self, app: &App, method: &str) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        let mut g = self.t.lock().await;
        match &mut g.0 {
            Transport::Stdio { stdin, .. } => {
                stdin.write_all(format!("{msg}\n").as_bytes()).await?;
            }
            Transport::Http { url, headers, session, oauth } => {
                post(app, url, headers, session, oauth, &msg).await?;
            }
        }
        Ok(())
    }

    pub async fn request(&self, app: &App, method: &str, params: Value) -> Result<Value> {
        let mut g = self.t.lock().await;
        g.1 += 1;
        let id = g.1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = match &mut g.0 {
            Transport::Stdio { stdin, lines, .. } => {
                stdin.write_all(format!("{msg}\n").as_bytes()).await?;
                let wait = async {
                    while let Some(line) = lines.next_line().await? {
                        if let Ok(v) = serde_json::from_str::<Value>(&line) {
                            if v["id"] == json!(id) {
                                return Ok(v);
                            }
                        }
                    }
                    Err(anyhow!("MCP server closed its output"))
                };
                tokio::time::timeout(Duration::from_secs(120), wait).await.map_err(|_| anyhow!("MCP request timed out"))??
            }
            Transport::Http { url, headers, session, oauth } => {
                let resp = post(app, url, headers, session, oauth, &msg).await?;
                if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                    *session = Some(s.to_string());
                }
                let status = resp.status();
                let sse = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("text/event-stream"));
                let text = resp.text().await?;
                if !status.is_success() {
                    bail!("MCP {method}: {status}: {text}");
                }
                if sse {
                    text.lines()
                        .filter_map(|l| l.strip_prefix("data:"))
                        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                        .find(|v| v["id"] == json!(id))
                        .ok_or_else(|| anyhow!("MCP {method}: no response in event stream"))?
                } else {
                    serde_json::from_str(&text)?
                }
            }
        };
        if let Some(e) = reply.get("error") {
            bail!("MCP {method}: {e}");
        }
        Ok(reply["result"].clone())
    }
}

/// Connect to every server in connections/ once per process, then reuse the clients.
pub async fn clients(app: &App, agent: &Agent) -> Vec<(String, Arc<Client>)> {
    let mut out = vec![];
    for c in &agent.connections {
        let key = format!("{}#{}", agent.dir.display(), c.name);
        let mut map = app.mcp.lock().await;
        if !map.contains_key(&key) {
            match Client::connect(app, &agent.dir, c).await {
                Ok(client) => {
                    map.insert(key.clone(), Arc::new(client));
                }
                Err(e) => {
                    eprintln!("egin: MCP connection {} failed: {e:#}", c.name);
                    continue;
                }
            }
        }
        out.push((c.name.clone(), map[&key].clone()));
    }
    out
}

/// Every connected tool as a function declaration named `<connection>__<tool>`.
async fn all_tools(app: &App, agent: &Agent) -> Vec<Value> {
    let mut v = vec![];
    for (conn, client) in clients(app, agent).await {
        for t in &client.tools {
            v.push(json!({"type": "function", "function": {
                "name": format!("{conn}__{}", t["name"].as_str().unwrap_or("")),
                "description": t["description"].as_str().unwrap_or(""),
                "parameters": t.get("inputSchema").cloned().unwrap_or(json!({"type": "object", "properties": {}})),
            }}));
        }
    }
    v
}

const AUTO_DEFER_ABOVE: usize = 20;

pub async fn exists(app: &App, agent: &Agent, name: &str) -> bool {
    all_tools(app, agent).await.iter().any(|t| t["function"]["name"] == name)
}

pub async fn deferred(app: &App, agent: &Agent) -> bool {
    match agent.cfg.mcp_tools.as_str() {
        "listed" => false,
        "deferred" => !agent.connections.is_empty(),
        _ => all_tools(app, agent).await.len() > AUTO_DEFER_ABOVE,
    }
}

/// Tool declarations: every MCP tool, or just tool_search and tool_call when deferred.
pub async fn specs(app: &App, agent: &Agent) -> Vec<Value> {
    let tools = all_tools(app, agent).await;
    if !deferred(app, agent).await {
        return tools;
    }
    let names: Vec<&str> = agent.connections.iter().map(|c| c.name.as_str()).collect();
    let about = format!("{} tools from the connections {} are not listed here.", tools.len(), names.join(", "));
    vec![
        json!({"type": "function", "function": {"name": "tool_search",
            "description": format!("{about} Search them by name, connection or keywords; results give each tool's name, description and parameters. Call a result with tool_call."),
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}}, "required": ["query"]}}}),
        json!({"type": "function", "function": {"name": "tool_call",
            "description": "Call a tool found with tool_search, by its full name (<connection>__<tool>), with its input.",
            "parameters": {"type": "object", "properties": {"name": {"type": "string"}, "input": {"type": "object"}}, "required": ["name"]}}}),
    ]
}

/// Ranked like eve's catalog: exact name, name after `__`, a connection's tools, prefix, then keywords.
pub async fn search(app: &App, agent: &Agent, query: &str, limit: usize) -> String {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return "error: query is empty".into();
    }
    let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.len() > 1).collect();
    let tools = all_tools(app, agent).await;
    let mut scored: Vec<(u32, &Value)> = tools
        .iter()
        .filter_map(|t| {
            let f = &t["function"];
            let name = f["name"].as_str().unwrap_or("").to_lowercase();
            let (conn, short) = name.split_once("__").unwrap_or(("", &name));
            let hay = format!(
                "{name} {} {}",
                f["description"].as_str().unwrap_or("").to_lowercase(),
                f["parameters"]["properties"].as_object().map(|p| p.keys().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default()
            );
            let score = if name == q {
                1000
            } else if short == q {
                900
            } else if conn == q || q == format!("{conn}__") {
                800
            } else if name.starts_with(&q) || short.starts_with(&q) {
                700
            } else {
                let hits = words.iter().filter(|w| hay.contains(*w)).count() as u32;
                if hits == 0 {
                    return None;
                }
                hits * 10
            };
            Some((score, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1["function"]["name"].as_str().cmp(&b.1["function"]["name"].as_str())));
    let total = scored.len();
    let lines: Vec<String> = scored
        .iter()
        .take(limit.clamp(1, 50))
        .map(|(_, t)| {
            let f = &t["function"];
            json!({"name": f["name"], "description": f["description"], "parameters": f["parameters"]}).to_string()
        })
        .collect();
    if lines.is_empty() {
        return format!("no tools match {query:?} ({} tools searched)", tools.len());
    }
    format!("{}\n({} of {total} matches shown; call one with tool_call)", lines.join("\n"), lines.len())
}

pub fn is_safe(agent: &Agent, name: &str) -> bool {
    name.split_once("__").is_some_and(|(conn, tool)| agent.connections.iter().any(|c| c.name == conn && c.safe.iter().any(|s| s == tool)))
}

/// Run `<connection>__<tool>`; None when the name is not an MCP tool.
pub async fn call(app: &App, agent: &Agent, name: &str, args: &Value) -> Option<String> {
    let (conn, tool) = name.split_once("__")?;
    agent.connections.iter().find(|c| c.name == conn)?;
    let client = clients(app, agent).await.into_iter().find(|(n, _)| n == conn)?.1;
    Some(match client.request(app, "tools/call", json!({"name": tool, "arguments": args})).await {
        Err(e) => format!("error: {e:#}"),
        Ok(r) => {
            let text: Vec<String> =
                r["content"].as_array().into_iter().flatten().map(|c| c["text"].as_str().map(String::from).unwrap_or_else(|| c.to_string())).collect();
            let body = if text.is_empty() { r.to_string() } else { text.join("\n") };
            if r["isError"].as_bool() == Some(true) {
                format!("error: {body}")
            } else {
                body
            }
        }
    })
}
