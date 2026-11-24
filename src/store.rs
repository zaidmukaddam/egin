//! Sessions and their logs: `messages` is the model context, `events` is the stream log.
//!
//! The store is a SQLite file in the agent folder, or, with `EGIN_STORE_URL` set, a libSQL database
//! reached over HTTP. Every egin process pointed at that database serves the same sessions, so a
//! session does not belong to the process that started it: another one can stream it, cancel it, or
//! take it over when its runner is gone. Both speak the same SQL.
// ponytail: one statement at a time (a mutex around the file, one worker thread for the remote
// store), and callers block on it from the async runtime. Use a pool if many sessions write at once.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{mpsc, Mutex},
    time::Duration,
};

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub model_calls: u64,
    pub tool_calls: u64,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub wall_ms: u64,
}

pub struct Store(Db);

enum Db {
    File(Mutex<Connection>),
    Remote(Remote),
}

/// A value bound to a statement or read from a row.
#[derive(Clone, Debug, PartialEq)]
enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Val {
    fn text(&self) -> Option<String> {
        match self {
            Val::Text(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// A stored JSON body.
    fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.text().context("the store returned no body")?)?)
    }
}

impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::Text(s.into())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::Text(s)
    }
}
impl From<i64> for Val {
    fn from(i: i64) -> Val {
        Val::Int(i)
    }
}
impl From<usize> for Val {
    fn from(i: usize) -> Val {
        Val::Int(i as i64)
    }
}
impl From<Option<&str>> for Val {
    fn from(s: Option<&str>) -> Val {
        s.map_or(Val::Null, Val::from)
    }
}

impl rusqlite::ToSql for Val {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Val::Null => ValueRef::Null,
            Val::Int(i) => ValueRef::Integer(*i),
            Val::Real(f) => ValueRef::Real(*f),
            Val::Text(s) => ValueRef::Text(s.as_bytes()),
        }))
    }
}

/// What one statement returned. `changed` means something only after a write.
struct Out {
    rows: Vec<Vec<Val>>,
    changed: u64,
    last_id: i64,
}

fn run_file(db: &Connection, sql: &str, args: &[Val]) -> rusqlite::Result<Out> {
    use rusqlite::types::ValueRef;
    let mut st = db.prepare(sql)?;
    let n = st.column_count();
    let mut q = st.query(rusqlite::params_from_iter(args))?;
    let mut rows = vec![];
    while let Some(r) = q.next()? {
        let mut row = Vec::with_capacity(n);
        for i in 0..n {
            row.push(match r.get_ref(i)? {
                ValueRef::Integer(v) => Val::Int(v),
                ValueRef::Real(v) => Val::Real(v),
                ValueRef::Text(t) => Val::Text(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Null | ValueRef::Blob(_) => Val::Null,
            });
        }
        rows.push(row);
    }
    drop(q);
    drop(st);
    Ok(Out { rows, changed: db.changes(), last_id: db.last_insert_rowid() })
}

/// A libSQL database over its HTTP pipeline API (`POST /v2/pipeline`): Turso, or any libSQL server.
/// One worker thread sends the requests, so the store's blocking calls work from any thread, inside
/// an async runtime or not.
struct Remote {
    jobs: mpsc::Sender<(Value, mpsc::Sender<Result<Value>>)>,
}

impl Remote {
    fn open(url: &str, token: &str) -> Result<Remote> {
        let base = url.trim().trim_end_matches('/');
        let base = base.strip_prefix("libsql://").or(base.strip_prefix("turso://")).map_or(base.to_string(), |host| format!("https://{host}"));
        if !base.starts_with("http://") && !base.starts_with("https://") {
            bail!("EGIN_STORE_URL must be a libsql://, https:// or http:// URL, got {url:?}");
        }
        let (url, token) = (format!("{base}/v2/pipeline"), token.trim().to_string());
        let (jobs, rx) = mpsc::channel::<(Value, mpsc::Sender<Result<Value>>)>();
        std::thread::Builder::new().name("egin-store".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("the store's runtime");
            let http = reqwest::Client::new();
            for (body, reply) in rx {
                let _ = reply.send(rt.block_on(Self::post(&http, &url, &token, &body)));
            }
        })?;
        Ok(Remote { jobs })
    }

    async fn post(http: &reqwest::Client, url: &str, token: &str, body: &Value) -> Result<Value> {
        let mut tries = 0;
        loop {
            let mut req = http.post(url).timeout(Duration::from_secs(30)).json(body);
            if !token.is_empty() {
                req = req.bearer_auth(token);
            }
            match req.send().await {
                // Only a request that never left is sent again: a write that timed out may have landed.
                Err(e) if e.is_connect() && tries < 3 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(200 * tries)).await;
                }
                Err(e) => return Err(e).context("the session store did not answer"),
                Ok(r) => {
                    let (status, text) = (r.status(), r.text().await?);
                    if !status.is_success() {
                        bail!("the session store answered {status}: {}", text.chars().take(300).collect::<String>());
                    }
                    return Ok(serde_json::from_str(&text)?);
                }
            }
        }
    }

    /// Statements in one request, in order, each in its own transaction. Stops at the first error.
    fn pipeline(&self, stmts: &[(&str, &[Val])]) -> Result<Vec<Out>> {
        let arg = |v: &Val| match v {
            Val::Null => json!({"type": "null"}),
            // Integers travel as strings: JSON numbers lose precision past 2^53.
            Val::Int(i) => json!({"type": "integer", "value": i.to_string()}),
            Val::Real(f) => json!({"type": "float", "value": f}),
            Val::Text(s) => json!({"type": "text", "value": s}),
        };
        let mut requests: Vec<Value> =
            stmts.iter().map(|(sql, args)| json!({"type": "execute", "stmt": {"sql": sql, "args": args.iter().map(arg).collect::<Vec<_>>()}})).collect();
        requests.push(json!({"type": "close"}));
        let (reply, answer) = mpsc::channel();
        self.jobs.send((json!({"requests": requests}), reply)).map_err(|_| anyhow!("the session store's worker stopped"))?;
        let wait = || answer.recv().map_err(|_| anyhow!("the session store's worker stopped"))?;
        // On a multi-thread runtime, say so: the other tasks on this worker move to another one.
        let v = match tokio::runtime::Handle::try_current() {
            Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(wait),
            _ => wait(),
        }?;
        let cell = |c: &Value| match c["type"].as_str() {
            Some("integer") => Val::Int(c["value"].as_str().and_then(|s| s.parse().ok()).or(c["value"].as_i64()).unwrap_or(0)),
            Some("float") => Val::Real(c["value"].as_f64().unwrap_or(0.0)),
            Some("text") => Val::Text(c["value"].as_str().unwrap_or("").into()),
            _ => Val::Null,
        };
        let mut out = vec![];
        for (i, _) in stmts.iter().enumerate() {
            let r = &v["results"][i];
            if r["type"] != "ok" {
                bail!("the session store: {}", r["error"]["message"].as_str().unwrap_or("no result for a statement"));
            }
            let res = &r["response"]["result"];
            let id = &res["last_insert_rowid"];
            out.push(Out {
                rows: res["rows"].as_array().into_iter().flatten().map(|row| row.as_array().into_iter().flatten().map(cell).collect()).collect(),
                changed: res["affected_row_count"].as_u64().unwrap_or(0),
                last_id: id.as_str().and_then(|s| s.parse().ok()).or(id.as_i64()).unwrap_or(0),
            });
        }
        Ok(out)
    }
}

fn is_busy(e: &anyhow::Error) -> bool {
    e.downcast_ref::<rusqlite::Error>()
        .is_some_and(|e| matches!(e.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)))
}

const TABLES: [&str; 5] = [
    // `parent` is the session a subagent's session was started from; `runner` is the process running
    // a session and `lease` when it last said it still is (unix seconds).
    "CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, agent TEXT, status TEXT, stats TEXT, created INTEGER, owner TEXT, settings TEXT, parent TEXT, lease INTEGER, runner TEXT)",
    "CREATE TABLE IF NOT EXISTS messages(session TEXT, seq INTEGER, body TEXT, PRIMARY KEY(session, seq))",
    "CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT, session TEXT, body TEXT)",
    "CREATE TABLE IF NOT EXISTS threads(key TEXT PRIMARY KEY, session TEXT)",
    "CREATE TABLE IF NOT EXISTS inbox(id INTEGER PRIMARY KEY AUTOINCREMENT, session TEXT, body TEXT)",
];

impl Store {
    /// The store for an agent: the database `EGIN_STORE_URL` names (with `EGIN_STORE_TOKEN`), or the
    /// file at `path`.
    pub fn open(path: &Path) -> Result<Store> {
        match crate::setting("STORE_URL").filter(|u| !u.trim().is_empty()) {
            Some(url) => Store::remote(&url, &crate::setting("STORE_TOKEN").unwrap_or_default()),
            None => Store::file(path),
        }
    }

    pub fn file(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path)?;
        // Several egin processes can share one agent folder's store (evals split per model). A writer
        // waits for the lock instead of failing after rusqlite's default 5 s, which aborted whole evals.
        db.busy_timeout(Duration::from_secs(60))?;
        let store = Store(Db::File(Mutex::new(db)));
        // Switching to WAL and WAL recovery can return BUSY without consulting the busy handler, so
        // processes opening the store at the same moment retry the setup themselves.
        let start = std::time::Instant::now();
        loop {
            match store.setup() {
                Err(e) if is_busy(&e) && start.elapsed() < Duration::from_secs(60) => std::thread::sleep(Duration::from_millis(100)),
                r => break r?,
            }
        }
        Ok(store)
    }

    /// A libSQL database every process serving the agent shares: `libsql://<db>-<org>.turso.io`, or
    /// the `http(s)://` address of a libSQL server.
    pub fn remote(url: &str, token: &str) -> Result<Store> {
        let store = Store(Db::Remote(Remote::open(url, token)?));
        store.setup().context("setting up the session store at EGIN_STORE_URL")?;
        Ok(store)
    }

    /// A store that lasts as long as the process: for `egin step`, whose host keeps the session and
    /// hands it in with every call.
    pub fn memory() -> Result<Store> {
        let store = Store(Db::File(Mutex::new(Connection::open_in_memory()?)));
        store.setup()?;
        Ok(store)
    }

    fn setup(&self) -> Result<()> {
        match &self.0 {
            Db::File(db) => {
                let db = db.lock().unwrap();
                db.execute_batch("PRAGMA journal_mode=WAL")?;
                db.execute_batch(&TABLES.join(";"))?;
            }
            Db::Remote(r) => {
                r.pipeline(&TABLES.map(|t| (t, &[] as &[Val])))?;
            }
        }
        // A store made by an earlier release lacks the columns added since: asking for the newest one
        // fails there, and the store gains whichever it is missing. Another process may add one
        // between the check and the ALTER.
        if self.run("SELECT runner FROM sessions LIMIT 0", &[]).is_ok() {
            return Ok(());
        }
        for (column, kind) in [("owner", "TEXT"), ("settings", "TEXT"), ("parent", "TEXT"), ("lease", "INTEGER"), ("runner", "TEXT")] {
            match self.run(&format!("ALTER TABLE sessions ADD COLUMN {column} {kind}"), &[]) {
                Err(e) if format!("{e:#}").contains("duplicate column") => {}
                r => drop(r?),
            }
        }
        Ok(())
    }

    fn run(&self, sql: &str, args: &[Val]) -> Result<Out> {
        match &self.0 {
            Db::File(db) => Ok(run_file(&db.lock().unwrap(), sql, args)?),
            Db::Remote(r) => Ok(r.pipeline(&[(sql, args)])?.remove(0)),
        }
    }

    /// The first row, if the statement returned one.
    fn one(&self, sql: &str, args: &[Val]) -> Result<Option<Vec<Val>>> {
        Ok(self.run(sql, args)?.rows.into_iter().next())
    }

    /// The user a session belongs to: None for no such session, Some(None) for one without an owner.
    pub fn owner(&self, id: &str) -> Result<Option<Option<String>>> {
        Ok(self.one("SELECT owner FROM sessions WHERE id=?1", &[id.into()])?.map(|r| r[0].text()))
    }

    pub fn set_owner(&self, id: &str, owner: &str) -> Result<()> {
        self.run("UPDATE sessions SET owner=?2 WHERE id=?1", &[id.into(), owner.into()])?;
        Ok(())
    }

    pub fn set_parent(&self, id: &str, parent: &str) -> Result<()> {
        self.run("UPDATE sessions SET parent=?2 WHERE id=?1", &[id.into(), parent.into()])?;
        Ok(())
    }

    /// The session a subagent's session was started from; None for a top-level session.
    pub fn parent(&self, id: &str) -> Result<Option<String>> {
        Ok(self.one("SELECT parent FROM sessions WHERE id=?1", &[id.into()])?.and_then(|r| r[0].text()))
    }

    /// The subagent sessions started from this one.
    pub fn children(&self, id: &str) -> Result<Vec<String>> {
        Ok(self.run("SELECT id FROM sessions WHERE parent=?1", &[id.into()])?.rows.iter().filter_map(|r| r[0].text()).collect())
    }

    /// `runner` runs the session from now on, whoever did before: the caller has decided to run it.
    pub fn claim(&self, id: &str, runner: &str) -> Result<()> {
        self.run("UPDATE sessions SET status='running', runner=?2, lease=unixepoch() WHERE id=?1", &[id.into(), runner.into()])?;
        Ok(())
    }

    /// Start a run of a session that has none. False if one is already running, here or in another
    /// process: one statement, so two processes handed a message at once cannot both start.
    pub fn start_run(&self, id: &str, runner: &str) -> Result<bool> {
        let out =
            self.run("UPDATE sessions SET status='running', runner=?2, lease=unixepoch() WHERE id=?1 AND status!='running'", &[id.into(), runner.into()])?;
        Ok(out.changed == 1)
    }

    /// Take over a running session whose runner has not renewed its lease for `stale_secs`: its
    /// process is gone. False if the lease is fresh or another process took it first.
    pub fn take_over(&self, id: &str, runner: &str, stale_secs: i64) -> Result<bool> {
        let out = self.run(
            "UPDATE sessions SET runner=?2, lease=unixepoch() WHERE id=?1 AND status='running' AND (lease IS NULL OR unixepoch() - lease >= ?3)",
            &[id.into(), runner.into(), stale_secs.into()],
        )?;
        Ok(out.changed == 1)
    }

    /// Say that `runner` is still running the session. False once it no longer is: another process
    /// took the session over, or it was cancelled or deleted there.
    pub fn renew_lease(&self, id: &str, runner: &str) -> Result<bool> {
        let out = self.run("UPDATE sessions SET lease=unixepoch() WHERE id=?1 AND runner=?2 AND status='running'", &[id.into(), runner.into()])?;
        Ok(out.changed == 1)
    }

    /// Seconds since a process last said it was running the session; None if none ever did.
    pub fn lease_age(&self, id: &str) -> Result<Option<i64>> {
        Ok(self.one("SELECT unixepoch() - lease FROM sessions WHERE id=?1", &[id.into()])?.and_then(|r| r[0].int()))
    }

    /// The channel thread a session answers, such as `slack:<channel>:<ts>` or `webhook:<thread>`.
    pub fn thread_of(&self, session: &str) -> Result<Option<String>> {
        Ok(self.one("SELECT key FROM threads WHERE session=?1 LIMIT 1", &[session.into()])?.and_then(|r| r[0].text()))
    }

    /// agent.toml overrides a client chose for this session (`model`, `reasoning_effort`), if any.
    pub fn settings(&self, id: &str) -> Result<Option<toml::Table>> {
        let text = self.one("SELECT settings FROM sessions WHERE id=?1", &[id.into()])?.and_then(|r| r[0].text());
        Ok(text.map(|t| toml::from_str(&t)).transpose()?)
    }

    pub fn set_settings(&self, id: &str, settings: &toml::Table) -> Result<()> {
        self.run("UPDATE sessions SET settings=?2 WHERE id=?1", &[id.into(), toml::to_string(settings)?.into()])?;
        Ok(())
    }

    /// Sessions newest first, each with its status, creation time and first user message.
    /// With an owner, only that user's sessions.
    pub fn list_sessions(&self, owner: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        let out = self.run(
            "SELECT s.id, s.status, s.created,
                    (SELECT json_extract(e.body, '$.text') FROM events e
                      WHERE e.session = s.id AND json_extract(e.body, '$.type') = 'user' ORDER BY e.id LIMIT 1)
               FROM sessions s WHERE ?1 IS NULL OR s.owner = ?1 ORDER BY s.created DESC, s.rowid DESC LIMIT ?2",
            &[owner.into(), limit.into()],
        )?;
        Ok(out.rows.iter().map(|r| json!({"id": r[0].text(), "status": r[1].text(), "created": r[2].int(), "title": r[3].text()})).collect())
    }

    pub fn create_session(&self, id: &str, agent: &str) -> Result<()> {
        self.run("INSERT INTO sessions(id, agent, status, stats, created) VALUES(?1, ?2, 'idle', '{}', unixepoch())", &[id.into(), agent.into()])?;
        Ok(())
    }

    pub fn status(&self, id: &str) -> Result<Option<String>> {
        Ok(self.one("SELECT status FROM sessions WHERE id=?1", &[id.into()])?.and_then(|r| r[0].text()))
    }

    pub fn set_status(&self, id: &str, status: &str) -> Result<()> {
        self.run("UPDATE sessions SET status=?2 WHERE id=?1", &[id.into(), status.into()])?;
        Ok(())
    }

    pub fn with_status(&self, status: &str) -> Result<Vec<String>> {
        Ok(self.run("SELECT id FROM sessions WHERE status=?1", &[status.into()])?.rows.iter().filter_map(|r| r[0].text()).collect())
    }

    pub fn stats(&self, id: &str) -> Result<Stats> {
        let row = self.one("SELECT stats FROM sessions WHERE id=?1", &[id.into()])?.with_context(|| format!("no session {id}"))?;
        Ok(serde_json::from_str(&row[0].text().unwrap_or_default())?)
    }

    pub fn put_stats(&self, id: &str, stats: &Stats) -> Result<()> {
        self.run("UPDATE sessions SET stats=?2 WHERE id=?1", &[id.into(), serde_json::to_string(stats)?.into()])?;
        Ok(())
    }

    pub fn push_message(&self, session: &str, body: &Value) -> Result<()> {
        self.run("INSERT INTO messages SELECT ?1, COALESCE(MAX(seq), -1) + 1, ?2 FROM messages WHERE session=?1", &[session.into(), body.to_string().into()])?;
        Ok(())
    }

    pub fn messages(&self, session: &str) -> Result<Vec<(i64, Value)>> {
        let out = self.run("SELECT seq, body FROM messages WHERE session=?1 ORDER BY seq", &[session.into()])?;
        out.rows.iter().map(|r| Ok((r[0].int().unwrap_or(0), r[1].json()?))).collect()
    }

    pub fn update_message(&self, session: &str, seq: i64, body: &Value) -> Result<()> {
        self.run("UPDATE messages SET body=?3 WHERE session=?1 AND seq=?2", &[session.into(), seq.into(), body.to_string().into()])?;
        Ok(())
    }

    /// Replace messages with from <= seq < to by one message (used by summarisation).
    pub fn replace_range(&self, session: &str, from: i64, to: i64, body: &Value) -> Result<()> {
        // The replacement first, then the rest of the range: two statements, and stopping between
        // them leaves the summary beside messages it covers, never a gap in the conversation.
        self.run("INSERT OR REPLACE INTO messages VALUES(?1, ?2, ?3)", &[session.into(), from.into(), body.to_string().into()])?;
        self.run("DELETE FROM messages WHERE session=?1 AND seq>?2 AND seq<?3", &[session.into(), from.into(), to.into()])?;
        Ok(())
    }

    pub fn push_event(&self, session: &str, body: &Value) -> Result<i64> {
        Ok(self.run("INSERT INTO events(session, body) VALUES(?1, ?2)", &[session.into(), body.to_string().into()])?.last_id)
    }

    /// Park a message that arrived while the session was running.
    pub fn enqueue(&self, session: &str, body: &Value) -> Result<()> {
        self.run("INSERT INTO inbox(session, body) VALUES(?1, ?2)", &[session.into(), body.to_string().into()])?;
        Ok(())
    }

    /// Take up to `limit` parked messages, oldest first. Each is handed out once, whoever asks.
    pub fn take_inbox(&self, session: &str, limit: usize) -> Result<Vec<Value>> {
        // One statement, so two processes sharing the store can't both take a message.
        let out = self.run(
            "DELETE FROM inbox WHERE id IN (SELECT id FROM inbox WHERE session=?1 ORDER BY id LIMIT ?2) RETURNING id, body",
            &[session.into(), limit.into()],
        )?;
        let mut rows: Vec<(i64, &Val)> = out.rows.iter().map(|r| (r[0].int().unwrap_or(0), &r[1])).collect();
        rows.sort_by_key(|r| r.0);
        rows.into_iter().map(|(_, b)| b.json()).collect()
    }

    /// Whether a call to `tool` was already approved in this session.
    pub fn tool_approved(&self, session: &str, tool: &str) -> Result<bool> {
        let sql = "SELECT 1 FROM events WHERE session=?1 AND json_extract(body,'$.type')='approval' AND json_extract(body,'$.tool')=?2 AND json_extract(body,'$.allow')=1 LIMIT 1";
        Ok(self.one(sql, &[session.into(), tool.into()])?.is_some())
    }

    /// A page of a session's log, oldest first: up to `limit` events before event `before` (all the
    /// latest with none), widened back to a `user` event so the page starts at a turn.
    pub fn events_page(&self, session: &str, before: Option<i64>, limit: usize) -> Result<Vec<(i64, Value)>> {
        let before = before.unwrap_or(i64::MAX);
        let first = |sql: &str, args: &[Val]| -> Result<Option<i64>> { Ok(self.one(sql, args)?.and_then(|r| r[0].int())) };
        let from = first(
            "SELECT MIN(id) FROM (SELECT id FROM events WHERE session=?1 AND id<?2 ORDER BY id DESC LIMIT ?3)",
            &[session.into(), before.into(), limit.into()],
        )?;
        let Some(from) = from else { return Ok(vec![]) };
        // Back to the user message that opened the turn the page would otherwise start inside.
        let turn = first("SELECT MAX(id) FROM events WHERE session=?1 AND id<=?2 AND json_extract(body,'$.type')='user'", &[session.into(), from.into()])?;
        let out = self.run(
            "SELECT id, body FROM events WHERE session=?1 AND id>=?2 AND id<?3 ORDER BY id",
            &[session.into(), turn.unwrap_or(from).into(), before.into()],
        )?;
        out.rows.iter().map(|r| Ok((r[0].int().unwrap_or(0), r[1].json()?))).collect()
    }

    /// Every stored event of one type, oldest first.
    pub fn events_of(&self, session: &str, kind: &str) -> Result<Vec<Value>> {
        let out = self.run("SELECT body FROM events WHERE session=?1 AND json_extract(body,'$.type')=?2 ORDER BY id", &[session.into(), kind.into()])?;
        out.rows.iter().map(|r| r[0].json()).collect()
    }

    /// The latest stored event of `kind` for a tool call.
    pub fn find_event(&self, session: &str, kind: &str, call_id: &str) -> Result<Option<Value>> {
        let sql = "SELECT body FROM events WHERE session=?1 AND json_extract(body,'$.type')=?2 AND json_extract(body,'$.call_id')=?3 ORDER BY id DESC LIMIT 1";
        self.one(sql, &[session.into(), kind.into(), call_id.into()])?.map(|r| r[0].json()).transpose()
    }

    /// The session a channel thread maps to, if any.
    pub fn thread(&self, key: &str) -> Result<Option<String>> {
        Ok(self.one("SELECT session FROM threads WHERE key=?1", &[key.into()])?.and_then(|r| r[0].text()))
    }

    /// Map a thread to a session; returns false if the key was already taken (a duplicate delivery).
    pub fn claim_thread(&self, key: &str, session: &str) -> Result<bool> {
        Ok(self.run("INSERT OR IGNORE INTO threads VALUES(?1, ?2)", &[key.into(), session.into()])?.changed == 1)
    }

    /// The content fingerprint the session last saw for a file (read_file, write_file, edit_file).
    pub fn file_seen(&self, session: &str, path: &str) -> Result<Option<String>> {
        let sql = "SELECT json_extract(body,'$.hash') FROM events WHERE session=?1 AND json_extract(body,'$.type')='file_seen' AND json_extract(body,'$.path')=?2 ORDER BY id DESC LIMIT 1";
        Ok(self.one(sql, &[session.into(), path.into()])?.and_then(|r| r[0].text()))
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        for table in ["sessions WHERE id", "messages WHERE session", "events WHERE session", "inbox WHERE session"] {
            self.run(&format!("DELETE FROM {table}=?1"), &[id.into()])?;
        }
        Ok(())
    }

    pub fn events_after(&self, session: &str, after: i64) -> Result<Vec<(i64, Value)>> {
        let out = self.run("SELECT id, body FROM events WHERE session=?1 AND id>?2 ORDER BY id", &[session.into(), after.into()])?;
        out.rows.iter().map(|r| Ok((r[0].int().unwrap_or(0), r[1].json()?))).collect()
    }
}

#[cfg(test)]
#[test]
fn a_write_waits_out_another_process_holding_the_lock() {
    let path = std::env::temp_dir().join(format!("egin-store-lock-{}.db", std::process::id()));
    let store = Store::file(&path).unwrap();
    // Another process's write transaction, held past rusqlite's default 5 s busy timeout.
    let other = Connection::open(&path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE; INSERT INTO threads VALUES ('k', 's');").unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        other.execute_batch("COMMIT").unwrap();
    });
    store.push_message("s1", &serde_json::json!({"role": "user", "content": "hi"})).unwrap();
    holder.join().unwrap();
    let _ = std::fs::remove_file(&path);
}

#[cfg(test)]
#[test]
fn many_processes_can_open_a_new_store_at_once() {
    // Evals split per model start together on one agent folder; one of three used to fail to open.
    let path = std::env::temp_dir().join(format!("egin-store-race-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let opens: Vec<_> = (0..12)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || Store::file(&path).map(|s| s.create_session(&uuid(), "a")))
        })
        .collect();
    for o in opens {
        o.join().unwrap().unwrap().unwrap();
    }
    assert_eq!(Store::file(&path).unwrap().list_sessions(None, 100).unwrap().len(), 12);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }

    fn uuid() -> String {
        format!("{:?}", std::thread::current().id())
    }
}

#[cfg(test)]
#[test]
fn a_store_from_before_owners_gains_the_column_and_keeps_its_sessions() {
    let path = std::env::temp_dir().join(format!("egin-store-owner-{}.db", std::process::id()));
    let old = Connection::open(&path).unwrap();
    old.execute_batch(
        "CREATE TABLE sessions(id TEXT PRIMARY KEY, agent TEXT, status TEXT, stats TEXT, created INTEGER);
         INSERT INTO sessions VALUES('old', 'a', 'done', '{}', 1);",
    )
    .unwrap();
    drop(old);
    let store = Store::file(&path).unwrap();
    assert_eq!(store.owner("old").unwrap(), Some(None));
    store.create_session("new", "a").unwrap();
    store.set_owner("new", "alice").unwrap();
    assert_eq!(store.list_sessions(Some("alice"), 10).unwrap().len(), 1);
    assert_eq!(store.list_sessions(None, 10).unwrap().len(), 2);
    drop(store);
    Store::file(&path).unwrap(); // opening again does not try to add the column twice
    let _ = std::fs::remove_file(&path);
}

#[cfg(test)]
#[test]
fn only_one_process_starts_or_takes_over_a_run_and_the_old_runner_finds_out() {
    let path = std::env::temp_dir().join(format!("egin-store-lease-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (a, b) = (Store::file(&path).unwrap(), Store::file(&path).unwrap());
    a.create_session("s", "agent").unwrap();
    assert!(a.start_run("s", "A").unwrap(), "an idle session starts");
    assert!(!b.start_run("s", "B").unwrap(), "a second process does not start it again");
    assert!(!b.take_over("s", "B", 30).unwrap(), "a fresh lease is not taken over");
    assert!(a.renew_lease("s", "A").unwrap());
    assert!(b.take_over("s", "B", 0).unwrap(), "a stale one is");
    assert!(!a.take_over("s", "A", 30).unwrap(), "and only once");
    assert!(!a.renew_lease("s", "A").unwrap(), "the old runner learns it lost the session");
    assert!(b.renew_lease("s", "B").unwrap());
    b.set_status("s", "cancelled").unwrap();
    assert!(!b.renew_lease("s", "B").unwrap(), "a cancelled session's runner learns to stop");
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}
