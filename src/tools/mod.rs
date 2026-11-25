//! Built-in tools plus the agent's own executables (JSON on stdin, result on stdout).

pub mod docs;
pub mod index;
pub mod mcp;
pub mod oauth;
pub mod web;

use crate::{
    agent::{run, Agent},
    sandbox, App,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn func(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required}}})
}

/// Tool declarations sent to the model, shaped by the agent's switches.
pub fn specs(agent: &Agent) -> Vec<Value> {
    let cfg = &agent.cfg;
    let s = json!({"type": "string"});
    let bash_desc = if cfg.bash_background {
        format!("Run a shell command in the working directory. Returns stdout, stderr and the exit code. Waits up to timeout_secs (default {}, max {}); a command still running then keeps running in the background, and the result says where its output and exit code will be.", cfg.bash_wait, cfg.bash_timeout)
    } else {
        format!(
            "Run a shell command in the working directory. Returns stdout, stderr and the exit code. Killed after timeout_secs (default {}, max {}).",
            cfg.bash_wait, cfg.bash_timeout
        )
    };
    let mut v = vec![func("bash", &bash_desc, json!({"command": s, "timeout_secs": {"type": "integer"}}), &["command"])];
    if cfg.tool_style == "typed" {
        v.push(func(
            "read_file",
            "Read a file. Text comes back as is. PDF, Word (.docx), PowerPoint (.pptx) and spreadsheets (.xlsx, .xls, .ods) come back as text. Images (.png, .jpg, .gif, .webp) are shown to you when your model can see images. Audio (.mp3, .wav, .m4a, .ogg, .flac, .aac) comes back as a transcript. Use offset and limit (in lines) for long results.",
            json!({"path": s, "offset": {"type": "integer"}, "limit": {"type": "integer"}}),
            &["path"],
        ));
        v.push(func("write_file", "Create or overwrite a file.", json!({"path": s, "content": s}), &["path", "content"]));
        v.push(func(
            "edit_file",
            "Replace one exact occurrence of `old` with `new` in a file.",
            json!({"path": s, "old": s, "new": s}),
            &["path", "old", "new"],
        ));
    }
    match cfg.web.as_str() {
        "client" => {
            v.push(func("web_search", "Search the web. Returns ranked results with relevant passages.", json!({"query": s, "count": {"type": "integer"}, "freshness": {"type": "string", "description": "day, week, month, year or YYYY-MM-DDtoYYYY-MM-DD"}}), &["query"]));
            v.push(func(
                "web_fetch",
                "Fetch a web page as markdown. A long page comes a part at a time: pass the offset the result gives to continue. format \"html\" returns the page's HTML as served, for when the markdown drops links, tables or attributes you need.",
                json!({"url": s, "offset": {"type": "integer"}, "format": {"type": "string", "enum": ["markdown", "html"]}}),
                &["url"],
            ));
        }
        "server" => {
            // The router runs these itself and only hands back turns that also call our functions.
            let mut search = json!({"type": "web_search", "count": cfg.search_count});
            if let Some(f) = &cfg.freshness {
                search["freshness"] = json!(f);
            }
            if !cfg.include_domains.is_empty() {
                search["include_domains"] = json!(cfg.include_domains);
            } else if !cfg.exclude_domains.is_empty() {
                search["exclude_domains"] = json!(cfg.exclude_domains);
            }
            v.push(search);
            v.push(json!({"type": "web_extract", "formats": ["markdown"]}));
        }
        _ => {}
    }
    if cfg.ask {
        v.push(func(
            "ask_user",
            "Ask the user a question and wait for the answer. Use it when you need a decision or a fact only they have, not to confirm routine steps. With `options`, the answer is one of them.",
            json!({"question": s, "options": {"type": "array", "items": s}}),
            &["question"],
        ));
    }
    if cfg.planning {
        v.push(func("plan", "Record or update the plan for this task.", json!({"steps": {"type": "array", "items": {"type": "object", "properties": {"step": s, "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}}, "required": ["step", "status"]}}}), &["steps"]));
    }
    if cfg.memory_max > 0 {
        v.push(func("memory", "Edit long-term memory (memory/notes.md), shown to you at the start of future sessions. `add` appends a line; `replace` rewrites the whole file, for pruning when it is full.", json!({"action": {"type": "string", "enum": ["add", "replace"]}, "text": s}), &["action", "text"]));
    }
    if cfg.index {
        v.push(func("search_files", "Search the workspace's files by content and title. Returns paths with size, word count, title and a matching snippet. An empty query lists the most recently changed files.", json!({"query": s, "limit": {"type": "integer"}}), &["query"]));
    }
    if !agent.skills.is_empty() {
        v.push(func("skill", "Load a skill's instructions by name.", json!({"name": s}), &["name"]));
    }
    if !agent.subagents.is_empty() {
        v.push(func(
            "task",
            "Hand a self-contained task to a subagent and get its final answer.",
            json!({"agent": {"type": "string", "enum": agent.subagents}, "prompt": s}),
            &["agent", "prompt"],
        ));
    }
    if cfg.helper {
        v.push(func(
            "helper",
            "Hand a self-contained question or piece of work to a fresh copy of yourself, working on the same files. It starts \
             with none of this conversation, so the prompt must say everything it needs; you get its final answer back. Use it \
             for an independent second analysis of something hard to get right, or a side investigation you want kept apart.",
            json!({"prompt": s}),
            &["prompt"],
        ));
    }
    for t in &agent.tools {
        v.push(json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}));
    }
    v
}

/// Safety of a concrete call: tool_call is as safe as the tool it calls.
pub fn call_is_safe(agent: &Agent, name: &str, args: &Value) -> bool {
    if name == "tool_call" {
        return is_safe(agent, args["name"].as_str().unwrap_or(""));
    }
    is_safe(agent, name)
}

/// Safe tools have no side effects: they skip approval and may run concurrently.
pub fn is_safe(agent: &Agent, name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "web_search"
            | "web_fetch"
            | "you_web_search"
            | "you_web_extract"
            | "skill"
            | "plan"
            | "search_files"
            | "tool_search"
            | "ask_user"
            | "final_answer"
    ) || agent.tools.iter().any(|t| t.name == name && t.safe)
        || mcp::is_safe(agent, name)
}

/// middleware/pre_tool runs on the host and sees every call as {"tool", "args"} on stdin; a non-zero exit blocks it.
pub async fn middleware(agent: &Agent, name: &str, args: &Value) -> Option<String> {
    let exe = agent.dir.join("middleware/pre_tool");
    if !exe.exists() {
        return None;
    }
    let input = json!({"tool": name, "args": args}).to_string().into_bytes();
    let out = sandbox::host(&exe.to_string_lossy(), &[], &agent.workspace.to_string_lossy(), Some(input), 30).await;
    match out {
        Ok(o) if o.ok => None,
        Ok(o) => Some(o.text),
        Err(e) => Some(format!("{e:#}")),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Shell for the model's commands: bash when the image has it, sh otherwise.
const SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash -c \"$0\"; else exec sh -c \"$0\"; fi";

/// Runs $1 as a job in its own process group (set -m), output to files in $2, and waits up to
/// $3 tenths of a second. Finished: prints its output and exits with its code, like a plain run.
/// Still running: prints the output so far and how to check on it or stop it.
// The wrapper's own stderr is dropped: job control can print a harmless setpgid race warning
// (parent and child both set the group); the job's output is read from its files.
const JOB: &str = r#"exec 2>/dev/null; set -m
d="$2"; mkdir -p "$d"; printf '%s' "$1" > "$d/cmd"
if command -v bash >/dev/null 2>&1; then s=bash; else s=sh; fi
( "$s" -c "$1" > "$d/stdout" 2> "$d/stderr" < /dev/null; echo $? > "$d/exit.tmp"; mv "$d/exit.tmp" "$d/exit" ) > /dev/null 2>&1 < /dev/null &
pid=$!; echo $pid > "$d/pid"
set +m  # no job notices in the output; the job keeps its own process group
n=0; while [ ! -f "$d/exit" ] && [ $n -lt "$3" ]; do sleep 0.1; n=$((n+1)); done
if [ -f "$d/exit" ]; then
  cat "$d/stdout" "$d/stderr"; c=$(cat "$d/exit"); rm -rf "$d"; exit "$c"
fi
tail -c 4000 "$d/stdout"; tail -c 2000 "$d/stderr"
printf '\n[still running in the background as process group %s after %ss. Output so far is above; full output goes to %s/stdout and %s/stderr, and the exit code to %s/exit when it finishes. Check with: tail %s/stdout; cat %s/exit. Stop with: kill -- -%s]' "$pid" "$(($3 / 10))" "$d" "$d" "$d" "$d" "$d" "$pid"
"#;

/// The session's commands that outlived their wait and are still running, one per line with how to stop it.
/// A job the model stopped leaves no exit file, so the process group is what says it is alive.
pub async fn running_jobs(app: &Arc<App>, agent: &Agent, sid: &str) -> String {
    let script = r#"for d in /tmp/.egin/jobs/"$1"-*/; do
  [ -f "$d/pid" ] && [ ! -f "$d/exit" ] || continue
  p=$(cat "$d/pid"); kill -0 "-$p" 2>/dev/null || continue
  printf '%s  (stop with: kill -- -%s)\n' "$(head -c 160 "$d/cmd" 2>/dev/null | tr '\n' ' ')" "$p"
done 2>/dev/null | head -8"#;
    let out = sandbox::exec(app, agent, sid, &argv(&["sh", "-c", script, "sh", sid]), None, 30).await;
    if out.ok {
        out.text.trim().to_string()
    } else {
        String::new()
    }
}

/// Read a file through the sandbox.
/// A tool's result: text for the tool message, and images to show the model after it.
pub struct Output {
    pub text: String,
    /// `image_url` content parts.
    pub images: Vec<Value>,
}

/// read_file handles files up to this size; past it the agent should use bash.
const MAX_FILE: u64 = 20 * 1024 * 1024;
/// Larger images are refused rather than sent: providers cap image size at about this.
const MAX_IMAGE: usize = 5 * 1024 * 1024;

/// Whether the session's model can see images: the agent's `vision` switch, else the router's word.
pub async fn sees_images(app: &App, agent: &Agent) -> bool {
    match agent.cfg.vision {
        Some(v) => v,
        None => app.model_info(&agent.cfg.model).await.is_some_and(|i| i.inputs.iter().any(|x| x == "image")),
    }
}

/// A file's bytes from the sandbox. They travel as base64, since the exec channel carries text.
async fn read_bytes(app: &App, agent: &Agent, sid: &str, path: &str) -> Result<Vec<u8>, String> {
    let size = sandbox::exec(app, agent, sid, &argv(&["sh", "-c", "wc -c < \"$1\"", "sh", path]), None, 30).await;
    let n: u64 = size.text.trim().parse().map_err(|_| size.text.trim().to_string())?;
    if n > MAX_FILE {
        return Err(format!("{path} is {} MB; read_file handles files up to 20 MB. Work with it through bash instead.", n / (1024 * 1024)));
    }
    // stdin keeps this portable: GNU and BSD base64 take file arguments differently.
    let o = sandbox::exec(app, agent, sid, &argv(&["sh", "-c", "base64 < \"$1\"", "sh", path]), None, 60).await;
    if !o.ok {
        return Err(o.text);
    }
    use base64::Engine;
    let clean: String = o.text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).map_err(|e| format!("could not read {path}: {e}"))
}

/// read_file for images and documents. Text files take the plain path in `execute_text`.
async fn read_document(app: &App, agent: &Agent, sid: &str, path: &str, kind: docs::Kind) -> Result<Output, String> {
    if matches!(kind, docs::Kind::Image(_)) && !sees_images(app, agent).await {
        return Err(format!(
            "{path} is an image and {} cannot see images. Tell the user, or work with it through bash (for example its size or metadata).",
            agent.cfg.model
        ));
    }
    let bytes = read_bytes(app, agent, sid, path).await?;
    if let docs::Kind::Audio(format) = kind {
        return hear(app, agent, sid, path, format, &bytes).await.map(|text| Output { text, images: vec![] });
    }
    if let docs::Kind::Image(mime) = kind {
        if bytes.len() > MAX_IMAGE {
            return Err(format!(
                "{path} is {} MB; images over 5 MB are not sent to the model. Shrink it first, for example with Python's PIL.",
                bytes.len() / (1024 * 1024)
            ));
        }
        use base64::Engine;
        let url = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));
        return Ok(Output {
            text: format!("{path} ({} KB) is attached as an image in the next message.", bytes.len() / 1024),
            images: vec![json!({"type": "image_url", "image_url": {"url": url}})],
        });
    }
    let text = tokio::task::spawn_blocking(move || docs::extract(&kind, &bytes)).await.map_err(|e| e.to_string())?;
    text.map(|text| Output { text, images: vec![] }).map_err(|e| format!("{path}: {e:#}"))
}

/// A transcript of an audio file, from a model that hears. Its cost counts toward the session's.
async fn hear(app: &App, agent: &Agent, sid: &str, path: &str, format: &str, bytes: &[u8]) -> Result<String, String> {
    let model = match &agent.cfg.audio_model {
        Some(m) => m.clone(),
        None => app
            .hearing_model(&agent.cfg.model)
            .await
            .ok_or_else(|| format!("{path} is audio and no available model takes audio. Set audio_model in agent.toml, or work with it through bash."))?,
    };
    use base64::Engine;
    let ask = "Transcribe this audio verbatim. Write only what is said, a new line for each speaker turn; put music and other sounds in [brackets].";
    let audio = json!({"type": "input_audio", "input_audio": {"data": base64::engine::general_purpose::STANDARD.encode(bytes), "format": format}});
    let body = json!({"model": model, "messages": [{"role": "user", "content": [{"type": "text", "text": ask}, audio]}], "stream": true, "stream_options": {"include_usage": true}});
    // The agent's provider settings apply only to its own model.
    let cfg = (model == agent.cfg.model).then_some(&agent.cfg);
    let ep = crate::model::Endpoint::resolve(app, &model, cfg).map_err(|e| e.to_string())?;
    let reply = crate::model::chat(app, &ep, &body, &mut |_, _| {}).await.map_err(|e| format!("could not transcribe {path} with {model}: {e:#}"))?;
    if let Ok(mut stats) = app.store.stats(sid) {
        run::add_usage(&mut stats, &reply.usage, app.model_info(&model).await.as_ref());
        let _ = app.store.put_stats(sid, &stats);
    }
    let text = reply.message["content"].as_str().unwrap_or("").trim();
    if text.is_empty() {
        return Err(format!("{model} returned no transcript for {path}"));
    }
    Ok(format!("Transcript of {path} ({} KB of {format} audio), by {model}:\n\n{text}", bytes.len() / 1024))
}

pub async fn read(app: &App, agent: &Agent, sid: &str, path: &str) -> Result<String, String> {
    let o = sandbox::exec(app, agent, sid, &argv(&["cat", "--", path]), None, 60).await;
    if o.ok {
        Ok(o.text)
    } else {
        Err(o.text)
    }
}

/// Write a file through the sandbox, creating parent directories.
pub async fn write(app: &App, agent: &Agent, sid: &str, path: &str, content: &[u8]) -> Result<(), String> {
    let script = "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"";
    let o = sandbox::exec(app, agent, sid, &argv(&["sh", "-c", script, "sh", path]), Some(content.to_vec()), 60).await;
    if o.ok {
        Ok(())
    } else {
        Err(o.text)
    }
}

fn fingerprint(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Absolute form of a path as the sandbox resolves it, so "a.txt" and "/workspace/a.txt" match.
fn abs(agent: &Agent, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", sandbox::workdir(agent).trim_end_matches('/'), path.trim_start_matches("./"))
    }
}

fn saw(app: &App, sid: &str, agent: &Agent, path: &str, content: &str) {
    app.emit(sid, "file_seen", json!({"path": abs(agent, path), "hash": fingerprint(content)}));
}

tokio::task_local! {
    /// The id of the tool call being run, set by the loop: durable tools key their step journal on it.
    pub static CALL_ID: String;
}

/// Tools this session has unlocked (see `CustomTool::locked`).
pub fn unlocked(app: &App, sid: &str) -> std::collections::HashSet<String> {
    let events = app.store.events_of(sid, "unlocked").unwrap_or_default();
    events.iter().flat_map(|e| e["tools"].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_str().map(String::from)).collect()
}

/// `specs` minus the locked tools this session has not unlocked.
pub fn specs_in(app: &App, agent: &Agent, sid: &str) -> Vec<Value> {
    let open = unlocked(app, sid);
    let hidden: Vec<&str> = agent.tools.iter().filter(|t| t.locked && !open.contains(&t.name)).map(|t| t.name.as_str()).collect();
    let mut v = specs(agent);
    v.retain(|s| !hidden.contains(&s["function"]["name"].as_str().unwrap_or("")));
    v
}

fn unlock(app: &App, sid: &str, by: &str, tools: &[String]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    app.emit(sid, "unlocked", json!({"by": by, "tools": tools}));
    format!("\n(Now available: {})", tools.join(", "))
}

pub async fn execute(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, name: &str, args: &Value) -> String {
    execute_full(app, agent, sid, name, args).await.text
}

/// Run a tool. Only read_file on an image returns images.
pub async fn execute_full(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, name: &str, args: &Value) -> Output {
    if name == "read_file" {
        let path = args["path"].as_str().unwrap_or("");
        let kind = docs::kind(path);
        if !matches!(kind, docs::Kind::Text) {
            return match read_document(app, agent, sid, path, kind).await {
                Err(e) => Output { text: format!("error: {e}"), images: vec![] },
                Ok(mut out) => {
                    out.text = lines(out.text, args);
                    out
                }
            };
        }
    }
    Output { text: execute_text(app, agent, sid, name, args).await, images: vec![] }
}

/// `offset` and `limit` (in lines) from read_file's arguments.
/// A shell command that rewrites published git history or drops unreachable objects for good.
fn destroys_git_history(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    let bfg = c
        .split(|ch: char| ch.is_whitespace() || ch == '/' || ch == ';' || ch == '&' || ch == '|')
        .any(|w| w == "bfg" || w.starts_with("bfg-") || w == "bfg.jar");
    bfg || ["filter-repo", "filter-branch", "reflog expire", "--prune=now", "gc --prune"].iter().any(|w| c.contains(w))
}

/// Whether the session's first user message (the task) uses any of `words`.
fn task_mentions(app: &App, sid: &str, words: &[&str]) -> bool {
    let msgs = app.store.messages(sid).unwrap_or_default();
    let task = msgs.iter().find(|(_, m)| m["role"] == "user").map(|(_, m)| m["content"].to_string().to_lowercase()).unwrap_or_default();
    words.iter().any(|w| task.contains(w))
}

fn lines(s: String, args: &Value) -> String {
    let (offset, limit) = (args["offset"].as_u64().unwrap_or(0) as usize, args["limit"].as_u64().unwrap_or(u64::MAX) as usize);
    if offset == 0 && limit == usize::MAX {
        return s;
    }
    s.lines().skip(offset).take(limit).collect::<Vec<_>>().join("\n")
}

async fn execute_text(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, name: &str, args: &Value) -> String {
    if name.contains("__") {
        if let Some(out) = mcp::call(app, agent, name, args).await {
            return out;
        }
    }
    if name == "tool_search" {
        return mcp::search(app, agent, args["query"].as_str().unwrap_or(""), args["limit"].as_u64().unwrap_or(10) as usize).await;
    }
    if name == "tool_call" {
        let target = args["name"].as_str().unwrap_or("");
        let input = args.get("input").cloned().unwrap_or(json!({}));
        if !mcp::exists(app, agent, target).await {
            return format!("error: no connected tool named {target:?}; find it with tool_search");
        }
        return match mcp::call(app, agent, target, &input).await {
            Some(out) => out,
            None => format!("error: no connected tool named {target:?}; find it with tool_search"),
        };
    }
    let str_arg = |k: &str| args[k].as_str().unwrap_or("").to_string();
    let path = str_arg("path");
    match name {
        "bash" => {
            let mut wait = args["timeout_secs"].as_u64().unwrap_or(agent.cfg.bash_wait).clamp(1, agent.cfg.bash_timeout.max(1));
            // Under a deadline, a call that asks to block for many minutes (a download, a training run)
            // left too little time for the rest on Terminal-Bench. Past this cap the command keeps going
            // in the background and the model checks on it.
            if agent.cfg.deadline > 0 && agent.cfg.bash_background {
                wait = wait.min(agent.cfg.bash_wait.max(180));
            }
            // Unattended, nobody can undo a destroyed git history. Rewriting and pruning it runs only when the
            // task itself talks about history; otherwise the original commits stay reachable. On Terminal-Bench,
            // runs that purged history to "remove secrets from the repository" lost the commit the checker needed.
            if agent.cfg.review {
                let cmd = str_arg("command");
                if destroys_git_history(&cmd) && !task_mentions(app, sid, &["history", "rewrite", "purge"]) {
                    return "error: not run. This command rewrites or prunes git history, which cannot be undone, and the task does not ask \
                            for a history change. Make the change in the working tree (and a normal commit, if the task wants commits) instead."
                        .into();
                }
            }
            if agent.cfg.bash_background {
                let dir = format!("/tmp/.egin/jobs/{sid}-{}", crate::new_id());
                let tenths = (wait * 10).to_string();
                sandbox::exec(app, agent, sid, &argv(&["sh", "-c", JOB, "sh", &str_arg("command"), &dir, &tenths]), None, wait + 30).await.text
            } else {
                sandbox::exec(app, agent, sid, &argv(&["sh", "-c", SHELL, &str_arg("command")]), None, wait).await.text
            }
        }
        "read_file" => match read(app, agent, sid, &path).await {
            Err(e) => format!("error: {e}"),
            Ok(s) => {
                saw(app, sid, agent, &path, &s);
                // A binary file read as text decodes to replacement characters: say what it is instead.
                if s.chars().take(4096).filter(|c| *c == '\u{FFFD}').count() > 8 {
                    return format!("error: {path} is a binary file that read_file cannot show. Inspect it with bash (for example `file {path}`).");
                }
                lines(s, args)
            }
        },
        "write_file" => {
            let content = str_arg("content");
            if agent.cfg.read_before_write {
                // Only an existing regular file needs a prior read; `test -f` keeps directories out.
                let exists = sandbox::exec(app, agent, sid, &argv(&["test", "-f", &path]), None, 30).await.ok;
                if exists {
                    let current = read(app, agent, sid, &path).await.unwrap_or_default();
                    match app.store.file_seen(sid, &abs(agent, &path)).ok().flatten() {
                        None => return format!("error: {path} already exists and you have not read it in this session. Read it with read_file first (or use edit_file for a small change)."),
                        Some(h) if h != fingerprint(&current) => return format!("error: {path} changed since you last read it. Read it again with read_file before overwriting it."),
                        Some(_) => {}
                    }
                }
            }
            match write(app, agent, sid, &path, content.as_bytes()).await {
                Ok(()) => {
                    saw(app, sid, agent, &path, &content);
                    format!("wrote {} bytes", content.len())
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "edit_file" => {
            let old = str_arg("old");
            match read(app, agent, sid, &path).await {
                Err(e) => format!("error: {e}"),
                Ok(_) if old.is_empty() => "error: `old` is empty".into(),
                Ok(s) => match s.matches(&old).count() {
                    1 => {
                        let next = s.replacen(&old, &str_arg("new"), 1);
                        match write(app, agent, sid, &path, next.as_bytes()).await {
                            Ok(()) => {
                                saw(app, sid, agent, &path, &next);
                                "edited".into()
                            }
                            Err(e) => format!("error: {e}"),
                        }
                    }
                    n => format!("error: `old` occurs {n} times; it must occur exactly once"),
                },
            }
        }
        // In server-web mode a turn that mixes web and function calls comes back with the
        // web calls named you_web_*; the router expects us to answer them ourselves.
        "web_search" | "you_web_search" => web_search(app, agent, sid, args).await.unwrap_or_else(|e| format!("error: {e:#}")),
        "web_fetch" | "you_web_extract" => web_fetch(app, agent, sid, args).await.unwrap_or_else(|e| format!("error: {e:#}")),
        "plan" => "Plan recorded.".into(),
        "memory" => memory(agent, &str_arg("action"), &str_arg("text")),
        "search_files" => {
            let (dir, ws, q, limit) = (agent.dir.clone(), agent.workspace.clone(), str_arg("query"), args["limit"].as_u64().unwrap_or(10) as usize);
            match tokio::task::spawn_blocking(move || index::search(&dir, &ws, &q, limit)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => format!("error: {e:#}"),
                Err(e) => format!("error: {e}"),
            }
        }
        "skill" => match agent.skills.iter().find(|k| k.name == str_arg("name")) {
            Some(k) => k.body.clone() + &unlock(app, sid, &k.name, &k.unlocks),
            None => format!("error: no skill named {}", str_arg("name")),
        },
        "task" => subagent(app, agent, sid, &str_arg("agent"), &str_arg("prompt")).await.unwrap_or_else(|e| format!("error: {e:#}")),
        "helper" if agent.cfg.helper => helper(app, agent, sid, &str_arg("prompt")).await.unwrap_or_else(|e| format!("error: {e:#}")),
        _ => match agent.tools.iter().find(|t| t.name == name) {
            Some(t) => {
                if t.locked && !unlocked(app, sid).contains(&t.name) {
                    return format!("error: {name} is not available yet");
                }
                // In a VM the agent's tools/ folder is copied up or mounted (sandbox::tools_dir);
                // local and Docker-local runs see it at its host path.
                let exe = match sandbox::tools_dir(agent) {
                    Some(dir) => format!("{dir}/{}", t.exe.file_name().unwrap().to_string_lossy()),
                    None => t.exe.to_string_lossy().into(),
                };
                let mut argv = vec![exe];
                // A durable tool gets a journal file per call, kept in the sandbox's working folder,
                // so a run again after a crash can skip the steps it already did.
                if let (true, Ok(id)) = (t.durable, CALL_ID.try_with(Clone::clone)) {
                    let journal = format!("{}/.egin/steps/{}.jsonl", sandbox::workdir(agent), id.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
                    let mkdir = "mkdir -p \"$(dirname \"$EGIN_STEPS\")\" && exec \"$0\"";
                    argv = vec!["env".into(), format!("EGIN_STEPS={journal}"), "sh".into(), "-c".into(), mkdir.into(), argv.remove(0)];
                }
                let input = Some(args.to_string().into_bytes());
                let o = sandbox::exec(app, agent, sid, &argv, input, agent.cfg.bash_timeout).await;
                let mut text = data_lines(app, sid, name, o.text);
                if o.ok {
                    text += &unlock(app, sid, name, &t.unlocks);
                }
                text
            }
            None => format!("error: unknown tool {name}"),
        },
    }
}

/// A custom tool's `::data <json>` lines are structured output for a UI (a chart, a card, a table):
/// each becomes a `data` event, and is left out of what the model sees.
fn data_lines(app: &App, sid: &str, tool: &str, text: String) -> String {
    if !text.contains("::data ") {
        return text;
    }
    let call = CALL_ID.try_with(Clone::clone).ok();
    let mut kept = vec![];
    for line in text.lines() {
        match line.strip_prefix("::data ").and_then(|j| serde_json::from_str::<Value>(j).ok()) {
            Some(data) => app.emit(sid, "data", json!({"call_id": call, "tool": tool, "data": data})),
            None => kept.push(line),
        }
    }
    kept.join("\n")
}

/// Long-term memory lives in the agent folder, so it outlives sessions and sandboxes.
fn memory(agent: &Agent, action: &str, text: &str) -> String {
    let path = agent.dir.join("memory/notes.md");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let next = match action {
        "add" => format!("{}{}- {}\n", current, if current.is_empty() || current.ends_with('\n') { "" } else { "\n" }, text.trim()),
        "replace" => format!("{}\n", text.trim()),
        _ => return "error: action must be add or replace".into(),
    };
    let max = agent.cfg.memory_max;
    if next.len() > max {
        return format!(
            "error: memory would be {} bytes, over the {max}-byte cap. Use `replace` with a shorter version.\nCurrent memory:\n{current}",
            next.len()
        );
    }
    let saved = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, &next));
    match saved {
        Ok(()) => format!("saved ({} of {max} bytes used); visible from the next session", next.len()),
        Err(e) => format!("error: {e}"),
    }
}

async fn subagent(app: &Arc<App>, parent: &Agent, sid: &str, name: &str, prompt: &str) -> anyhow::Result<String> {
    if !parent.subagents.iter().any(|s| s == name) {
        anyhow::bail!("no subagent named {name}");
    }
    let dir = parent.dir.join("subagents").join(name);
    let mut sub = Agent::load(&dir, &parent.inherited(&dir))?;
    sub.workspace = parent.workspace.clone();
    let sub_sid = run::new_session(app, &sub)?;
    // The subagent's session belongs to whoever owns the parent, so that user can follow it and
    // answer its approvals.
    if let Some(Some(owner)) = app.store.owner(sid)? {
        app.store.set_owner(&sub_sid, &owner)?;
    }
    // Recorded so cancelling the parent reaches it, and a restart leaves it to the parent.
    app.store.set_parent(&sub_sid, sid)?;
    app.emit(sid, "subagent", json!({"agent": name, "session": sub_sid}));
    run::send(app, &sub_sid, prompt)?;
    run::run_session(app.clone(), Arc::new(sub), sub_sid).await
}

/// A fresh copy of the agent in its own session, on the parent's sandbox and files, that answers one prompt. On
/// Terminal-Bench, kimi-k3.1-pro under eve handed work to such a copy in 27 of 78 tasks, 4 of its 8 wins over egin.
/// A copy gets no helper of its own and no review pass. Under a deadline it gets half the parent's time left as its
/// own deadline, and is stopped there: on Terminal-Bench an Inkling 2 Mini copy started 5.6 minutes before the
/// parent's limit was still working when the task was killed, so the parent never wrote its answer.
async fn helper(app: &Arc<App>, parent: &Agent, sid: &str, prompt: &str) -> anyhow::Result<String> {
    let mut copy = parent.clone();
    copy.cfg.helper = false;
    copy.cfg.review = false;
    let budget = (parent.cfg.deadline > 0).then(|| {
        let spent = app.run_started.lock().unwrap().get(sid).map_or(0, |t| t.elapsed().as_secs());
        (parent.cfg.deadline.saturating_sub(spent) / 2).max(30)
    });
    copy.cfg.deadline = budget.unwrap_or(0);
    copy.cfg.max_turns = copy.cfg.max_turns.min(60);
    let sub_sid = run::new_session(app, &copy)?;
    if let Some(Some(owner)) = app.store.owner(sid)? {
        app.store.set_owner(&sub_sid, &owner)?;
    }
    app.store.set_parent(&sub_sid, sid)?;
    let sandbox = app.sandbox_of.lock().unwrap().get(sid).cloned().unwrap_or_else(|| sid.to_string());
    app.sandbox_of.lock().unwrap().insert(sub_sid.clone(), sandbox);
    app.emit(sid, "subagent", json!({"agent": "helper", "session": sub_sid}));
    run::send(app, &sub_sid, prompt)?;
    let run = run::run_session(app.clone(), Arc::new(copy), sub_sid.clone());
    let out = match budget {
        // The copy's own deadline notes ask it to finish first; this is the backstop.
        Some(secs) => match tokio::time::timeout(std::time::Duration::from_secs(secs + 15), run).await {
            Ok(out) => out,
            Err(_) => {
                app.run_started.lock().unwrap().remove(&sub_sid);
                app.store.set_status(&sub_sid, "cancelled")?;
                app.emit(&sub_sid, "cancelled", json!({"reason": "the helper's share of the parent's time ran out"}));
                Ok(format!("The helper was stopped after {secs}s, its share of the time left. Whatever it changed in the files stays; check them before relying on it."))
            }
        },
        None => run.await,
    };
    app.sandbox_of.lock().unwrap().remove(&sub_sid);
    // The copy's tokens and spend count toward the parent's, so its cost (and a spend limit) see all of the work.
    // Its model calls do not count against the parent's turn limit.
    let (sub, mut own) = (app.store.stats(&sub_sid)?, app.store.stats(sid)?);
    own.prompt_tokens += sub.prompt_tokens;
    own.cached_tokens += sub.cached_tokens;
    own.completion_tokens += sub.completion_tokens;
    own.reasoning_tokens += sub.reasoning_tokens;
    own.cost += sub.cost;
    app.store.put_stats(sid, &own)?;
    out
}

async fn web_search(app: &App, agent: &Agent, sid: &str, args: &Value) -> anyhow::Result<String> {
    let cfg = &agent.cfg;
    let withhold = Withhold::new(app, agent, sid);
    let count = args["count"].as_u64().unwrap_or(cfg.search_count as u64);
    let freshness = args["freshness"].as_str().or(cfg.freshness.as_deref());
    let hits = web::search(app, cfg, args["query"].as_str().unwrap_or(""), count, freshness).await?;
    // exclude_domains is also enforced here, not only by the provider, and exclude_terms and
    // exclude_echo only here: an eval's answer key must never reach the model, whichever provider
    // let a result through.
    let kept: Vec<_> = hits.iter().filter(|r| !excluded(cfg, &r.url) && withhold.hides(&format!("{}\n{}\n{}", r.title, r.url, r.text)).is_none()).collect();
    // Every hit gets an equal share of the page, so a long first result cannot crowd out the rest.
    let share = cfg.page_max / kept.len().max(1);
    let mut out = String::new();
    for (n, r) in kept.iter().enumerate() {
        let room = share.saturating_sub(r.title.len() + r.url.len() + 40);
        let text = if r.text.len() > room { format!("{} […]", &r.text[..r.text.floor_char_boundary(room)]) } else { r.text.clone() };
        out += &format!("[{}] {}\n{}\n{}\n\n", n + 1, r.title, r.url, text);
    }
    Ok(if out.is_empty() { "no results".into() } else { out })
}

/// Whether a URL's host is one of `exclude_domains` or under one (a domain covers its subdomains).
pub fn excluded(cfg: &crate::agent::Config, url: &str) -> bool {
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_lowercase)).unwrap_or_default();
    cfg.exclude_domains.iter().map(|d| d.trim_start_matches('.').to_lowercase()).any(|d| host == d || host.ends_with(&format!(".{d}")))
}

/// What must not reach the model from the web: text holding one of `exclude_terms`, and with
/// `exclude_echo` text that quotes the session's task.
struct Withhold<'a> {
    cfg: &'a crate::agent::Config,
    task: std::collections::HashSet<String>,
}

impl<'a> Withhold<'a> {
    fn new(app: &App, agent: &'a Agent, sid: &str) -> Self {
        let mut task = std::collections::HashSet::new();
        if agent.cfg.exclude_echo {
            for (_, m) in app.store.messages(sid).unwrap_or_default() {
                if let (Some("user"), Some(text)) = (m["role"].as_str(), m["content"].as_str()) {
                    // What the task itself puts in quotation marks (a title, a passage to find) is
                    // expected on the pages it is about, so only the task's own words count.
                    let text = text.replace(['“', '”'], "\"");
                    for own in text.split('"').step_by(2) {
                        task.extend(shingles(own));
                    }
                }
            }
        }
        Withhold { cfg: &agent.cfg, task }
    }

    /// Why `text` is withheld, if it is.
    fn hides(&self, text: &str) -> Option<&'static str> {
        if self.cfg.exclude_terms.iter().any(|t| text.contains(t.as_str())) {
            Some("it mentions a term excluded for this agent")
        } else if !self.task.is_empty() && shingles(text).iter().any(|s| self.task.contains(s)) {
            Some("it quotes this task word for word")
        } else {
            None
        }
    }
}

/// Every run of 12 consecutive words, lowercased, punctuation dropped. Two texts that share one
/// quote each other: twelve words in a row do not line up by chance, and a name or a title the
/// task mentions in passing is shorter.
fn shingles(text: &str) -> std::collections::HashSet<String> {
    let clean: String = text.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect();
    let words: Vec<&str> = clean.split_whitespace().collect();
    words.windows(12).map(|w| w.join(" ")).collect()
}

async fn web_fetch(app: &App, agent: &Agent, sid: &str, args: &Value) -> anyhow::Result<String> {
    let withhold = Withhold::new(app, agent, sid);
    let urls: Vec<&str> = match &args["urls"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => args["url"].as_str().into_iter().collect(),
    };
    let (blocked, urls): (Vec<&str>, Vec<&str>) = urls.into_iter().partition(|u| excluded(&agent.cfg, u));
    if urls.is_empty() {
        anyhow::bail!("{} is blocked by this agent's exclude_domains", blocked.join(", "));
    }
    let fetched = web::fetch(app, &agent.cfg, &urls, args["format"] == "html").await?;
    let pages = fetched.iter().map(|p| {
        let page = format!("# {}\n{}\n\n{}", p.title, p.url, p.text);
        match withhold.hides(&page) {
            Some(why) => format!("{}\n(page withheld: {why})", p.url),
            None => page,
        }
    });
    let mut out = pages.collect::<Vec<_>>().join("\n\n---\n\n");
    if out.is_empty() {
        out = "no content".into();
    }
    if !blocked.is_empty() {
        out = format!("(not fetched, blocked by exclude_domains: {})\n\n{out}", blocked.join(", "));
    }
    // ponytail: a later part fetches the page again; cache per session if providers bill it twice.
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    if offset > 0 {
        out = if offset >= out.len() { "no more content at this offset".into() } else { out[out.ceil_char_boundary(offset)..].to_string() };
    }
    Ok(out)
}
