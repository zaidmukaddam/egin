//! The agent loop. It is driven only by the stored messages, so resuming a
//! killed session is the same code path as running one: no model call repeats.

use crate::{agent::Agent, model, tools, App, Approve};
use anyhow::{bail, Result};
use futures_util::future::{join_all, BoxFuture};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc, time::Instant};

pub fn new_session(app: &App, agent: &Agent) -> Result<String> {
    let sid = crate::new_id();
    app.store.create_session(&sid, &agent.dir.to_string_lossy())?;
    app.store.push_message(&sid, &json!({"role": "system", "content": agent.system_prompt()}))?;
    Ok(sid)
}

pub fn send(app: &App, sid: &str, text: &str) -> Result<()> {
    send_with(app, sid, text, &[], vec![])
}

/// A user message with uploaded files (`{name, path}`) and the `image_url` parts of the ones the
/// model should see directly. The model gets the paths after the text; the event keeps the files
/// apart so a UI can show them as attachments.
pub fn send_with(app: &App, sid: &str, text: &str, files: &[Value], images: Vec<Value>) -> Result<()> {
    let (message, event) = user_message(text, files, images);
    app.store.push_message(sid, &message)?;
    app.emit(sid, "user", event);
    Ok(())
}

/// A message for a session that is running: parked in its inbox, and handed to the model by the
/// run itself (see `mid_run`). The `queued` event lets a UI show it as pending until its `user` event.
pub fn enqueue(app: &App, sid: &str, text: &str, files: &[Value], images: Vec<Value>) -> Result<()> {
    let (message, event) = user_message(text, files, images);
    app.store.enqueue(sid, &json!({"message": message, "event": event}))?;
    app.emit(sid, "queued", json!({"text": text}));
    Ok(())
}

/// Move up to `limit` parked messages into the conversation. Returns how many moved.
pub fn deliver_inbox(app: &App, sid: &str, limit: usize) -> Result<usize> {
    let parked = app.store.take_inbox(sid, limit)?;
    for p in &parked {
        app.store.push_message(sid, &p["message"])?;
        app.emit(sid, "user", p["event"].clone());
    }
    Ok(parked.len())
}

fn user_message(text: &str, files: &[Value], images: Vec<Value>) -> (Value, Value) {
    let mut content = text.to_string();
    if !files.is_empty() {
        content += "\n\nAttached files:";
        for f in files {
            content += &format!("\n- {}", f["path"].as_str().unwrap_or_default());
        }
    }
    let message = if images.is_empty() {
        json!({"role": "user", "content": content})
    } else {
        let mut parts = vec![json!({"type": "text", "text": content})];
        parts.extend(images);
        json!({"role": "user", "content": parts})
    };
    let mut event = json!({"text": text});
    if !files.is_empty() {
        event["files"] = json!(files);
    }
    (message, event)
}

/// How often a running session's lease is renewed, and how old a lease must be before another process
/// may take the session over: its runner is then gone.
pub const LEASE_RENEW: std::time::Duration = std::time::Duration::from_secs(10);
pub const LEASE_STALE_SECS: i64 = 30;

/// The run stopped at its budget with work left (see `App::budget`), or at a tool call that waits on
/// the session's host: not a failure. The next run picks up from the session's stored messages.
#[derive(Debug)]
pub struct Paused;
impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("the run reached its model-call budget")
    }
}
impl std::error::Error for Paused {}

/// Renews a session's lease until dropped, which happens when its run ends or is aborted.
struct Lease(tokio::task::AbortHandle);

impl Lease {
    /// `lost` fires once the store says this process no longer runs the session: another process took
    /// it over (this one was frozen past the lease, as a serverless instance is between requests), or
    /// it was cancelled or deleted through another process.
    fn hold(app: &Arc<App>, sid: &str, lost: tokio::sync::oneshot::Sender<()>) -> Lease {
        let (app, sid) = (app.clone(), sid.to_string());
        Lease(
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(LEASE_RENEW).await;
                    // A store that cannot be reached says nothing either way: keep running.
                    if let Ok(false) = app.store.renew_lease(&sid, &app.runner) {
                        let _ = lost.send(());
                        return;
                    }
                }
            })
            .abort_handle(),
        )
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run a session until the model answers without tool calls. Returns the answer.
pub fn run_session(app: Arc<App>, agent: Arc<Agent>, sid: String) -> BoxFuture<'static, Result<String>> {
    Box::pin(async move {
        app.store.claim(&sid, &app.runner)?;
        let (lost, mut lease_lost) = tokio::sync::oneshot::channel();
        let _lease = Lease::hold(&app, &sid, lost);
        let started = Instant::now();
        app.run_started.lock().unwrap().insert(sid.clone(), started);
        let span = crate::Span::start(&sid, None, "session.run");
        let _hooks = super::hooks::watch(&app, &agent, &sid);
        // Boxed: a helper's run nests inside its parent's, and the loop's future is too large to
        // hold twice over on one stack.
        let mut work = Box::pin(async {
            let mut continues = 0;
            loop {
                let out = drive(&app, &agent, &sid, &span.id, started).await;
                // hooks/done can send the session on: what it prints is the next message.
                if let (Ok(text), true) = (&out, continues < super::hooks::MAX_CONTINUES) {
                    if let Some(next) = super::hooks::on_done(&agent, &sid, text).await {
                        continues += 1;
                        app.emit(&sid, "hook", json!({"hook": "done", "message": next}));
                        send(&app, &sid, &next)?;
                        continue;
                    }
                }
                break out;
            }
        });
        let out = tokio::select! {
            out = &mut work => out,
            // The session is someone else's now: stop without touching its status or its log, which
            // are theirs to write.
            Ok(()) = &mut lease_lost => {
                app.run_started.lock().unwrap().remove(&sid);
                anyhow::bail!("session {sid} is no longer run by this process: another one took it over, or it was cancelled there");
            }
        };
        drop(work);
        app.run_started.lock().unwrap().remove(&sid);
        let mut stats = app.store.stats(&sid)?;
        stats.wall_ms += started.elapsed().as_millis() as u64;
        app.store.put_stats(&sid, &stats)?;
        span.end(
            &app,
            &agent.exporters,
            json!({"egin.agent": agent.dir.to_string_lossy(), "egin.ok": out.is_ok(), "egin.model_calls": stats.model_calls, "egin.cost_usd": stats.cost}),
        );
        match &out {
            Ok(text) => {
                app.store.set_status(&sid, "done")?;
                let mut done = json!({"text": text, "stats": stats});
                if super::output::schema(&app, &sid).is_some() {
                    if let Ok(v) = serde_json::from_str::<Value>(text) {
                        done["output"] = v;
                    }
                }
                app.emit(&sid, "done", done);
            }
            Err(e) if e.is::<Paused>() => {}
            Err(e) => {
                app.store.set_status(&sid, "error")?;
                app.emit(&sid, "error", json!({"message": format!("{e:#}")}));
            }
        }
        out
    })
}

async fn drive(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, run_span: &str, started: Instant) -> Result<String> {
    let cfg = &agent.cfg;
    let mcp_specs = crate::tools::mcp::specs(app, agent).await;
    let schema = super::output::schema(app, sid);
    loop {
        // Rebuilt every step: a tool or skill may have unlocked more tools.
        let mut specs = tools::specs_in(app, agent, sid);
        specs.extend(mcp_specs.iter().cloned());
        if let Some(s) = &schema {
            specs.push(super::output::spec(s));
        }
        let msgs = app.store.messages(sid)?;
        if let Some(a) = msgs.iter().rposition(|(_, m)| m["role"] == "assistant") {
            let answered: HashSet<&str> = msgs[a + 1..].iter().filter_map(|(_, m)| m["tool_call_id"].as_str()).collect();
            let pending: Vec<Value> =
                msgs[a].1["tool_calls"].as_array().into_iter().flatten().filter(|c| !answered.contains(c["id"].as_str().unwrap_or(""))).cloned().collect();
            if !pending.is_empty() {
                run_tools(app, agent, sid, &pending, run_span).await?;
                continue;
            }
            // A final_answer that fit the schema ends the run with its value.
            for c in msgs[a].1["tool_calls"].as_array().into_iter().flatten().filter(|c| c["function"]["name"] == super::output::TOOL) {
                if let Some(e) = app.store.find_event(sid, "output", c["id"].as_str().unwrap_or(""))? {
                    return Ok(e["output"].to_string());
                }
            }
        }
        // Messages sent mid-run join here, between steps, never between a call and its result.
        // "steer" hands over all of them before the next model call; "queue" one at a time, once
        // the model has answered.
        let answered = msgs.last().is_some_and(|(_, m)| m["role"] == "assistant");
        let steer = agent.cfg.mid_run == "steer";
        if (steer || answered) && deliver_inbox(app, sid, if steer { 1000 } else { 1 })? > 0 {
            continue;
        }
        if answered {
            // A tool call the provider failed to parse arrives as text in the model's own call syntax.
            // Ending there would take that text as the answer and drop the work it was about to do.
            if leaked_tool_call(msgs.last().unwrap().1["content"].as_str().unwrap_or("")) {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                if !msgs[turn..].iter().any(|(_, m)| m["egin_leak_nudge"] == true) {
                    let note = "Your last message contained a tool call written out as text, so it was not run. If you meant to call a tool, call it again as a tool call. Otherwise give your final answer.";
                    app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_leak_nudge": true}))?;
                    continue;
                }
            }
            // An empty reply is not an answer. Most are reasoning models that planned their next step in their
            // reasoning and then emitted neither text nor a tool call ("Let me continue. I found a strong
            // signal…"); that reasoning isn't sent back, so a bare nudge made them recap from scratch and stall
            // again. Hand the end of the thought back and ask a few times before failing the run: stopping at
            // the second empty reply ended 19 Terminal-Bench runs that pi, which keeps going, finished.
            let last = &msgs.last().unwrap().1;
            if last["content"].as_str().unwrap_or("").trim().is_empty() && last["tool_calls"].as_array().is_none_or(|c| c.is_empty()) {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                if msgs[turn..].iter().filter(|(_, m)| m["egin_empty_nudge"] == true).count() >= EMPTY_NUDGES {
                    bail!("the model returned an empty reply {} times", EMPTY_NUDGES + 1);
                }
                let thought = last["reasoning_content"].as_str().unwrap_or("").trim();
                let note = if thought.is_empty() {
                    "Your last reply was empty. Continue the task with tool calls, or give your final answer.".to_string()
                } else {
                    let tail: String = thought.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
                    format!(
                        "Your last reply had reasoning but no message and no tool call, so nothing was done. Your reasoning ended with:\n\n{tail}\n\nAct on it now: make the next tool call, or give your final answer."
                    )
                };
                app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_empty_nudge": true}))?;
                continue;
            }
            // Tests that existed before the run, newly marked skipped or expected to fail: graders run the original
            // tests, so this hides a regression. gpt-6.2-sol acted on a wrong review finding, broke 17 W3C cases on
            // DeepSWE, then marked them xfail in the repository's conftest and reported a green suite.
            if cfg.review && schema.is_none() {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                let task = msgs.get(turn).map(|(_, m)| m["content"].as_str().unwrap_or("").to_lowercase()).unwrap_or_default();
                if !msgs[turn..].iter().any(|(_, m)| m["egin_test_guard"] == true) && !["skip", "xfail", "disable"].iter().any(|w| task.contains(w)) {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let found = silenced_tests(app, agent, sid, now.saturating_sub(started.elapsed().as_secs() + 120)).await;
                    if !found.is_empty() {
                        let note = format!(
                            "You marked tests that existed before your work as skipped or expected to fail:\n{found}\n\nThe original tests are what decide whether the work is done, so this hides a failure rather than fixing it. Remove those markers and change the code until the original tests pass; where a change you made conflicts with them, the existing tests win. Then give your final answer."
                        );
                        app.emit(sid, "test_guard", json!({"found": found}));
                        app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_test_guard": true}))?;
                        continue;
                    }
                }
            }
            // A script the run wrote that needs a Python package the run installed works here, and fails or takes
            // an untested path wherever that package is missing. On FrontierHarness three of four Kimi runs built an
            // HTML filter on html5lib they had pip-installed; the tests ran it with only what the image came with.
            if cfg.review && schema.is_none() {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                if !msgs[turn..].iter().any(|(_, m)| m["egin_deps_guard"] == true) {
                    let task = msgs.get(turn).map(|(_, m)| m["content"].as_str().unwrap_or("").to_lowercase()).unwrap_or_default();
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let all = added_deps(app, agent, sid, now.saturating_sub(started.elapsed().as_secs() + 120)).await;
                    // A package the task names is one it asked for.
                    let found: Vec<&str> =
                        all.lines().filter(|l| l.rsplit(": ").next().is_some_and(|package| !task.contains(&package.to_lowercase()))).collect();
                    if !found.is_empty() {
                        let found = found.join("\n");
                        let note = format!(
                            "These files need a Python package that was not here when you started (you installed it during this task):\n{found}\n\nWhoever runs your work next, a test or a person, may use a Python that has only what was installed at the start, and there it fails or takes a path you never ran. If something already installed does the job, use that instead. If you keep the package as an optional extra, run your checks again with it unavailable. If the work truly needs it, leave it. Then give your final answer."
                        );
                        app.emit(sid, "deps_guard", json!({"found": found}));
                        app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_deps_guard": true}))?;
                        continue;
                    }
                }
            }
            // A command that outlived its wait keeps running after the run ends, and can hold a lock the next
            // process needs. On Terminal-Bench an `apt-get` left running kept the dpkg lock, so the verifier
            // could not install its own tools and correct answers scored zero.
            if cfg.review && cfg.bash_background {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                if !msgs[turn..].iter().any(|(_, m)| m["egin_jobs_nudge"] == true) {
                    let running = tools::running_jobs(app, agent, sid).await;
                    if !running.is_empty() {
                        let note = format!(
                            "These commands you started are still running in the background:\n{running}\n\nA process left running can hold a lock (a package manager's, a database's) or change files after you finish. If the task needs one running, such as a server it asked you to start, leave it. Otherwise wait for it or stop it, then give your final answer."
                        );
                        app.emit(sid, "jobs_nudge", json!({"running": running}));
                        app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_jobs_nudge": true}))?;
                        continue;
                    }
                }
            }
            // Before a task run ends, one review pass. Most benchmark failures were runs that stopped early
            // and confidently: own checks green, an edge case of the spec untested, a difference they saw
            // and explained away, stray files left behind, or a summary of work never done.
            if cfg.review && schema.is_none() {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                let late = cfg.deadline > 0 && started.elapsed().as_secs_f64() >= 0.85 * cfg.deadline as f64;
                if !late && !msgs[turn..].iter().any(|(_, m)| m["egin_review"] == true) {
                    let changed = msgs[turn..].iter().any(|(_, m)| {
                        m["tool_calls"].as_array().into_iter().flatten().any(|c| match c["function"]["name"].as_str() {
                            Some("write_file" | "edit_file" | "replace") => true,
                            Some("bash") => {
                                let cmd = serde_json::from_str::<Value>(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or_default()["command"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                [">", "sed -i", "tee ", "patch", "git apply", "git commit", "cp ", "mv ", "rm "].iter().any(|w| cmd.contains(w))
                            }
                            _ => false,
                        })
                    });
                    let task = msgs.get(turn).map(|(_, m)| m["content"].as_str().unwrap_or("").to_string()).unwrap_or_default();
                    let answer = msgs.last().unwrap().1["content"].as_str().unwrap_or("").to_string();
                    // What the files the task names hold right now, read by egin rather than asking the model to
                    // look: a run once left an early guess in its output file while its final answer was right.
                    let files = named_files(app, agent, sid, &task).await;
                    let note = if !changed {
                        Some(format!("You have not changed anything yet. If the task asks for a change, make it now, then give your final answer.{files}"))
                    } else {
                        // A reviewer in a fresh context, with the task and the work but none of the agent's reasoning:
                        // self-review in the same context mostly re-confirms what the agent already believes.
                        let left = (cfg.deadline > 0).then(|| (cfg.deadline as f64 - started.elapsed().as_secs_f64()).max(0.0));
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                        let since = now.saturating_sub(started.elapsed().as_secs() + 120);
                        match independent_review(app, agent, sid, &task, &answer, &files, left, since).await {
                            Ok(None) => None,
                            Ok(Some(found)) => Some(format!(
                                "An independent review of your work against the task found these problems:\n{found}\n\nThe reviewer can be wrong. Confirm each one against the task text or by running the code before you change anything, fix those that hold up, then give your final answer.{files}"
                            )),
                            Err(e) => Some(format!(
                                "Before you finish, check your work against the task: run a concrete check for each requirement it states, including the exact names, formats and edge cases it mentions, then give your final answer. (The automatic review could not run: {e:#}){files}"
                            )),
                        }
                    };
                    app.emit(sid, "review", json!({"issues": note.is_some()}));
                    if let Some(note) = note {
                        app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_review": true}))?;
                        continue;
                    }
                }
            }
            // With a schema, an answer in plain text is sent back, twice at most, for final_answer.
            if schema.is_some() {
                let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
                let nudged = msgs[turn..].iter().filter(|(_, m)| m["egin_output_nudge"] == true).count();
                if nudged < 2 {
                    let note = "Give your answer by calling final_answer with the structured value, not as text.";
                    app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_output_nudge": true}))?;
                    continue;
                }
            }
            return Ok(msgs.last().unwrap().1["content"].as_str().unwrap_or("").to_string());
        }

        // At a turn or spend limit the session gets one last call, with tools off, to answer from what
        // it found: a limit should cut the work short, not throw it all away.
        let stats = app.store.stats(sid)?;
        let over = if stats.model_calls >= cfg.max_turns {
            Some(format!("turn limit reached ({})", cfg.max_turns))
        } else if cfg.max_cost > 0.0 && stats.cost >= cfg.max_cost {
            Some(format!("spend limit reached (${:.4})", cfg.max_cost))
        } else {
            None
        };
        let note_at = msgs.iter().position(|(_, m)| m["egin_wrap_up"] == true);
        let wrapping = note_at.is_some();
        if let Some(reason) = &over {
            // The last call already happened and still ended in tool calls (a provider that ignored
            // tool_choice): stop here rather than loop.
            if note_at.is_some_and(|i| msgs[i..].iter().any(|(_, m)| m["role"] == "assistant")) {
                bail!("{reason}");
            }
            if !wrapping {
                let note = format!(
                    "You have reached this task's limit ({reason}). Do not call any more tools. From what you have found so far, give your best final answer now, in the exact format the task asks for."
                );
                app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_wrap_up": true}))?;
                app.emit(sid, "wrap_up", json!({"reason": reason}));
                continue;
            }
        }
        // Near a wall-clock deadline someone else enforces, say so while there is still time: at 60% to
        // get a working result in place, at 85% to save and wrap up. A run cut off mid-edit loses work it
        // never wrote down or committed. On Terminal-Bench, runs that were cut off while still changing
        // things were the gap to eve: a note at 80% alone still left them making edits when time ran out.
        if over.is_none() && cfg.deadline > 0 {
            let frac = started.elapsed().as_secs_f64() / cfg.deadline as f64;
            let turn = msgs.iter().rposition(|(_, m)| from_person(m)).unwrap_or(0);
            let said = |kind: &str| msgs[turn..].iter().any(|(_, m)| m["egin_deadline"] == kind);
            let kind = if frac >= 0.85 && !said("save") {
                Some("save")
            } else if frac >= 0.6 && !said("save") && !said("check") {
                Some("check")
            } else {
                None
            };
            if let Some(kind) = kind {
                let left = (cfg.deadline as f64 - started.elapsed().as_secs_f64()).max(0.0);
                let left_text = if left >= 120.0 { format!("{} minutes", (left / 60.0).round()) } else { format!("{} seconds", left.round()) };
                let note = if kind == "save" {
                    format!("About {left_text} left before this task's time limit stops you. Save your work now: finish the change you are on, make sure what works is written to disk (and committed, if the task asks for commits), then wrap up.")
                } else {
                    format!("About {left_text} left before this task's time limit stops you. If what the task asks for is not in place yet, put a working version of it in place now (written to disk, and committed if the task asks for commits), then keep improving it.")
                };
                app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_deadline": kind}))?;
                app.emit(sid, "deadline", json!({"left_secs": left.round(), "kind": kind}));
                continue;
            }
        }
        // A long run is asked, every `check_in` model calls, whether it is done. On FrontierHarness the same
        // models made 72 calls under egin on tasks they went on to fail, against 51 under pi and 53 under eve:
        // nothing was stuck, they kept testing work that was already in place.
        if over.is_none() && cfg.check_in > 0 && stats.model_calls > 0 && stats.model_calls % cfg.check_in == 0 {
            let calls = stats.model_calls;
            if !msgs.iter().any(|(_, m)| m["egin_check_in"] == json!(calls)) {
                let note = format!(
                    "You have made {calls} model calls on this task. Take stock. If what the task asks for is in place and your checks of it pass, give your final answer now instead of testing further. If a stated requirement is still unmet, name it and work on that only."
                );
                app.store.push_message(sid, &json!({"role": "user", "content": note, "egin_check_in": calls}))?;
                app.emit(sid, "check_in", json!({"calls": calls}));
                continue;
            }
        }
        // `egin step` runs a bounded number of model calls, or for a bounded time, and hands the
        // session back to its host.
        if matches!(&*app.budget.lock().unwrap(), Some(b) if b.session == sid && (b.calls == 0 || b.until.is_some_and(|t| std::time::Instant::now() >= t))) {
            return Err(anyhow::Error::new(Paused));
        }
        let info = app.model_info(&cfg.model).await;
        // Context management works on at most `context_focus` tokens of a larger window: on DeepSWE, models with
        // 1M windows ran to 260k-token prompts with nothing ever elided, and capping at 256k won 3 of 40 back.
        let window = cfg.context_limit.or(info.as_ref().map(|i| i.context).filter(|c| *c > 0)).unwrap_or(128_000);
        let limit = if cfg.context_focus > 0 { window.min(cfg.context_focus) } else { window };
        let msgs = shrink(app, agent, sid, msgs, limit, info.as_ref()).await?;
        let mut stats = app.store.stats(sid)?; // shrink may have added a summarisation call

        let mut body = json!({
            "model": cfg.model,
            "messages": msgs.iter().map(|(_, m)| m).collect::<Vec<_>>(),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if !specs.is_empty() {
            body["tools"] = json!(specs);
            body["parallel_tool_calls"] = json!(cfg.batching);
            // The tools stay declared, since the history holds calls to them, but none may be called.
            if over.is_some() {
                body["tool_choice"] = json!("none");
            }
        }
        if let Some(e) = &cfg.reasoning_effort {
            body["reasoning_effort"] = json!(e);
        }
        if let Some(n) = cfg.max_tokens {
            body["max_tokens"] = json!(n);
        }
        let span = crate::Span::start(sid, Some(run_span), "model.call");
        let ep = model::Endpoint::resolve(app, &cfg.model, Some(cfg))?;
        // Under a deadline one model call may not take more than ~30% of the time left: on Terminal-Bench
        // single calls ran 15-30 minutes of reasoning. Past the cap it is asked again at low effort.
        let cap = (cfg.deadline > 0).then(|| {
            let left = (cfg.deadline as f64 - started.elapsed().as_secs_f64()).max(0.0);
            std::time::Duration::from_secs_f64((0.3 * left).max(180.0))
        });
        let started = std::time::Instant::now();
        let reply = match chat_once(app, &ep, &body, sid, cap).await {
            // A provider that refuses images (the router lists deepseek-v4.1-flash as taking them; its
            // provider answered 400) would refuse every later call too, since the image stays in the history.
            Err(e) if format!("{e:#}").contains("400") && has_images(&body["messages"]) => {
                app.emit(sid, "images_dropped", json!({"error": format!("{e:#}")}));
                for (seq, m) in app.store.messages(sid)? {
                    if has_images(&json!([m])) {
                        let mut m = m;
                        strip_images(&mut m);
                        app.store.update_message(sid, seq, &m)?;
                    }
                }
                let mut plain = body.clone();
                if let Some(ms) = plain["messages"].as_array_mut() {
                    ms.iter_mut().for_each(strip_images);
                }
                chat_once(app, &ep, &plain, sid, cap).await?
            }
            r => r?,
        };
        let ms = started.elapsed().as_millis() as u64;
        if let Some(b) = app.budget.lock().unwrap().as_mut() {
            if b.session == sid {
                b.calls = b.calls.saturating_sub(1);
            }
        }
        app.store.push_message(sid, &reply.message)?;
        add_usage(&mut stats, &reply.usage, info.as_ref());
        app.store.put_stats(sid, &stats)?;
        let u = &reply.usage;
        span.end(app, &agent.exporters, json!({"gen_ai.request.model": cfg.model, "gen_ai.usage.input_tokens": u.prompt, "gen_ai.usage.output_tokens": u.completion, "egin.cached_tokens": u.cached}));
        app.emit(
            sid,
            "assistant",
            json!({
                "content": reply.message["content"],
                "reasoning": reply.message["reasoning_content"],
                "tool_calls": reply.message["tool_calls"],
                "usage": {"prompt": u.prompt, "cached": u.cached, "completion": u.completion, "reasoning": u.reasoning},
                "web_results": reply.web_results,
                "model": cfg.model,
                "cost": info.as_ref().map_or(0.0, |i| model::cost(i, u)),
                "ms": ms,
            }),
        );
    }
}

/// Empty replies answered with a nudge in one turn before the run fails.
const EMPTY_NUDGES: usize = 3;

/// Streamed output (about 16k tokens) past which a call over its time cap counts as runaway reasoning.
const RUNAWAY_CHARS: usize = 64_000;
/// How long a call over its time cap may stream nothing before it counts as stalled.
const STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// A model call cut short (`None`) only once it is past `cap` and is either reasoning without end (past
/// `RUNAWAY_CHARS` of output) or has stalled. A plain time cap cut ordinary replies on a slow server: on one GPU
/// Inkling 2 Mini streamed ~30 tokens/s, so a normal 4k-token reply passed the 180 s floor, and the
/// low-effort retry started it again from nothing (26 cuts in 87 Terminal-Bench runs, most of them in runs that
/// then ran out of time).
async fn capped_chat(app: &App, ep: &model::Endpoint, body: &Value, sid: &str, cap: std::time::Duration) -> Option<Result<model::Reply>> {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let streamed = AtomicUsize::new(0);
    let mut on = |kind: &str, data: Value| {
        if kind == "token" || kind == "reasoning" {
            streamed.fetch_add(data["text"].as_str().map_or(0, str::len), Relaxed);
        }
        app.emit(sid, kind, data)
    };
    let call = model::chat(app, ep, body, &mut on);
    let cut = async {
        tokio::time::sleep(cap).await;
        let (mut seen, mut quiet) = (streamed.load(Relaxed), std::time::Duration::ZERO);
        let tick = std::time::Duration::from_secs(5);
        loop {
            let now = streamed.load(Relaxed);
            if now >= RUNAWAY_CHARS || quiet >= STALL {
                return;
            }
            quiet = if now == seen { quiet + tick } else { std::time::Duration::ZERO };
            seen = now;
            tokio::time::sleep(tick).await;
        }
    };
    tokio::select! {
        r = call => Some(r),
        () = cut => None,
    }
}

/// One model call in a fresh context that reads the task, the agent's answer, what it changed and the files the
/// task names, and lists the ways the work fails the task as written. `None` when it finds nothing.
#[allow(clippy::too_many_arguments)] // one call site; each argument is a different fact about the run
async fn independent_review(
    app: &Arc<App>,
    agent: &Agent,
    sid: &str,
    task: &str,
    answer: &str,
    files: &str,
    left: Option<f64>,
    since: u64,
) -> Result<Option<String>> {
    let cfg = &agent.cfg;
    // The work as it is on disk: the git diff and new files in a repository, else the files changed during the run,
    // in full. Given only their names (FrontierHarness tasks in /app, no git), reviewers on deepseek and GLM wrote
    // tool calls out as text to read them, and Kimi answered OK about code it never saw. The diff
    // starts before the run's first commit ($2 is the run's start, unix seconds): a task that says "commit when
    // done" otherwise left the reviewer an empty `git diff HEAD` and it reported that nothing had changed.
    let script = r###"cd "$1" 2>/dev/null || exit 0
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  first=$(git log --since="@$2" --format=%H 2>/dev/null | tail -1)
  base=HEAD; [ -n "$first" ] && base=$(git rev-parse --verify -q "$first^" || git hash-object -t tree /dev/null)
  if [ -z "$(git status --porcelain 2>/dev/null)" ] && git diff --quiet "$base" 2>/dev/null; then echo EGIN_NO_DIFF; exit 0; fi
  echo "## git status"; git status --short --untracked-files=all | head -60
  [ "$base" != HEAD ] && { echo; echo "## commits made in this run"; git log --oneline "$base..HEAD" | head -20; }
  echo; echo "## git diff (from before this run's commits)"; git diff "$base" 2>/dev/null | head -c 30000
  git status --short --untracked-files=all | sed -n 's/^?? //p' | head -6 | while read -r f; do [ -f "$f" ] && { printf '\n## new file %s\n' "$f"; head -c 2000 "$f"; }; done
else
  m=$(( ($(date +%s) - $2) / 60 + 1 ))
  files=$(find . -type f -mmin -$m -not -path '*/.git/*' -not -path '*/node_modules/*' -not -path '*/__pycache__/*' -not -path '*/.pytest_cache/*' -not -name '*.pyc' 2>/dev/null | head -60)
  echo "## files changed during this run"; echo "$files"
  echo "$files" | while read -r f; do [ -f "$f" ] && [ "$(wc -c < "$f")" -le 200000 ] && grep -Iq . "$f" 2>/dev/null && { printf '\n## %s\n' "$f"; cat "$f"; }; done | head -c 40000
fi"###;
    let argv: Vec<String> = ["sh", "-c", script, "sh", &crate::sandbox::workdir(agent), &since.to_string()].iter().map(|s| s.to_string()).collect();
    let work = crate::sandbox::exec(app, agent, sid, &argv, None, 60).await.text;
    // The agent changed files but egin sees no difference here (work outside this repository, or a base it got
    // wrong): a reviewer shown "nothing changed" told deepseek its work was gone, and it spent its last turns on
    // branches. Better no review than a false one.
    if work.contains("EGIN_NO_DIFF") {
        app.emit(sid, "independent_review", json!({"skipped": "no diff visible"}));
        return Ok(None);
    }
    let system = "You review another agent's work on a task. You get the task, the agent's final answer, its changes on disk, and the current contents of files the task names. \
                  You have no tools: you cannot run commands or open other files, so judge from what is below. \
                  List only concrete ways the work fails the task as written:\n\
                  - a requirement not met, or met only for the simplest form of the input;\n\
                  - an edge case, category or message the task names that the work would get wrong (try it in your head with inputs worded exactly as the task words them);\n\
                  - a requirement that can reasonably be read another way, under which the work fails;\n\
                  - an output file whose contents do not match the final answer or the required format.\n\
                  Each problem is one line starting with \"- \" and must quote, in double quotes, the exact words of the task it fails. \
                  A problem you cannot tie to words in the task is not a problem: leave it out. Data the task supplies (sequences, \
                  files, reference values, examples) is correct as given: never flag the work for using it as given. No style advice, nothing speculative. \
                  If the work meets the task, reply with exactly: OK";
    let user = format!("# Task\n{task}\n\n# Agent's final answer\n{answer}\n\n# Changes on disk\n{work}\n{files}");
    let mut body = json!({"model": cfg.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}], "stream": true, "stream_options": {"include_usage": true}});
    if let Some(e) = &cfg.reasoning_effort {
        body["reasoning_effort"] = json!(e);
    }
    let ep = model::Endpoint::resolve(app, &cfg.model, Some(cfg))?;
    let cap = std::time::Duration::from_secs_f64(left.map_or(600.0, |l| (0.2 * l).clamp(60.0, 600.0)));
    let reply = tokio::time::timeout(cap, model::chat(app, &ep, &body, &mut |_, _| {}))
        .await
        .map_err(|_| anyhow::anyhow!("the review call ran past {}s", cap.as_secs()))??;
    let info = app.model_info(&cfg.model).await;
    let mut stats = app.store.stats(sid)?;
    add_usage(&mut stats, &reply.usage, info.as_ref());
    app.store.put_stats(sid, &stats)?;
    let text = reply.message["content"].as_str().unwrap_or("").trim().to_string();
    // A finding counts only if it quotes words that are really in the task. Unanchored findings ("main.cpp was not
    // requested", "doom.wad was modified") made agents delete or revert correct work on Terminal-Bench. Only "- " lines
    // are findings: a GLM reviewer's preamble ("Let me start by reading...") quoted the task and went back as one.
    let kept: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with("- ") && quotes_task(l, task)).collect();
    app.emit(sid, "independent_review", json!({"text": text, "kept": kept.len()}));
    Ok((!kept.is_empty()).then(|| kept.join("\n")))
}

/// Lines the run added to test files that existed before it which skip a test or expect it to fail, as `file: line`.
async fn silenced_tests(app: &Arc<App>, agent: &Agent, sid: &str, since: u64) -> String {
    let script = r###"cd "$1" 2>/dev/null || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0
first=$(git log --since="@$2" --format=%H 2>/dev/null | tail -1)
base=HEAD; [ -n "$first" ] && base=$(git rev-parse --verify -q "$first^") || true
[ -n "$base" ] || exit 0
git diff "$base" --diff-filter=M --name-only 2>/dev/null \
 | grep -E '(^|/)(tests?|spec|__tests__)/|(^|/)(test_[^/]*|[^/]*_test\.[a-z]+|[^/]*\.(test|spec)\.[a-z]+|conftest\.py)$' \
 | while read -r f; do
  git diff "$base" -U0 -- "$f" | grep -E '^\+' | grep -v '^+++' \
   | grep -E 'pytest\.mark\.(xfail|skip)|pytest\.(xfail|skip)\(|unittest\.(skip|expectedFailure)|@skip|\.skip\(|\bx(it|describe|test)\(|@Ignore|@Disabled|t\.Skip\(|#\[ignore\]|\bxfail\b' \
   | head -5 | cut -c2-200 | sed "s|^|$f: |"
done | head -20"###;
    let argv: Vec<String> = ["sh", "-c", script, "sh", &crate::sandbox::workdir(agent), &since.to_string()].iter().map(|s| s.to_string()).collect();
    let out = crate::sandbox::exec(app, agent, sid, &argv, None, 60).await;
    if out.ok {
        out.text.trim().to_string()
    } else {
        String::new()
    }
}

/// Python files the run wrote that need a package installed during the run, as `file: package`: one they import,
/// or name in a string (`BeautifulSoup(html, "html5lib")` imports nothing). Empty where there is no python3.
async fn added_deps(app: &Arc<App>, agent: &Agent, sid: &str, since: u64) -> String {
    let script = r###"cd "$1" 2>/dev/null || exit 0
command -v python3 >/dev/null 2>&1 || exit 0
python3 - "$2" <<'PY' 2>/dev/null
import ast, importlib.util, os, sys
since = float(sys.argv[1])
skip = {".git", "node_modules", "__pycache__", "site-packages", "dist-packages", ".venv", "venv", ".tox"}
found, seen = [], 0
for root, dirs, files in os.walk("."):
    dirs[:] = [] if root.count(os.sep) >= 3 else [d for d in dirs if d not in skip]
    for name in files:
        path = os.path.join(root, name)
        if not name.endswith(".py") or os.path.getmtime(path) < since or seen >= 40:
            continue
        seen += 1
        try:
            tree = ast.parse(open(path, errors="replace").read())
        except Exception:
            continue
        names = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                names.update(a.name.split(".")[0] for a in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
                names.add(node.module.split(".")[0])
            elif isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value.isidentifier():
                names.add(node.value)
        for module in sorted(names):
            try:
                # pip writes a package's files when it installs them, so their mtime is the install time;
                # an image's own packages keep the time they were built.
                origin = importlib.util.find_spec(module).origin or ""
                if ("site-packages" in origin or "dist-packages" in origin) and os.path.getmtime(origin) >= since:
                    found.append(f"{path[2:]}: {module}")
            except Exception:
                pass
print("\n".join(found[:10]))
PY"###;
    let argv: Vec<String> = ["sh", "-c", script, "sh", &crate::sandbox::workdir(agent), &since.to_string()].iter().map(|s| s.to_string()).collect();
    let out = crate::sandbox::exec(app, agent, sid, &argv, None, 60).await;
    if out.ok {
        out.text.trim().to_string()
    } else {
        String::new()
    }
}

/// Whether `line` quotes, in double quotes, at least 8 characters that appear in `task` (case and spacing aside).
fn quotes_task(line: &str, task: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let task = norm(task);
    line.split(['"', '“', '”']).skip(1).step_by(2).map(norm).any(|q| q.chars().count() >= 8 && task.contains(&q))
}

/// Whether a message is one a person sent, so a turn starts there: not a note egin added (`egin_*` markers, the
/// summary) and not the images a tool call returned (`for_calls`). Counting those as turns once gave the reviewer
/// an empty task after every image read.
fn from_person(m: &Value) -> bool {
    m["role"] == "user" && m.get("for_calls").is_none() && !m.as_object().is_some_and(|o| o.keys().any(|k| k.starts_with("egin_")))
}

/// Absolute file paths a task names (`/app/out.txt`, "/app/primers.fasta"), at most five.
fn named_paths(task: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in task.split(|c: char| c.is_whitespace() || "`'\"()[]<>,;".contains(c)) {
        let p = w.trim_end_matches(['.', ':', '!', '?']);
        let file = p.rsplit('/').next().unwrap_or("");
        if p.starts_with('/') && p.len() > 2 && file.contains('.') && !file.starts_with('.') && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out.truncate(5);
    out
}

/// The current contents of the files the task names, for the review note; empty when none exists yet.
async fn named_files(app: &Arc<App>, agent: &Agent, sid: &str, task: &str) -> String {
    let paths = named_paths(task);
    if paths.is_empty() {
        return String::new();
    }
    let script = "for p in \"$@\"; do [ -f \"$p\" ] || continue; printf '\\n--- %s (%s bytes) ---\\n' \"$p\" \"$(wc -c < \"$p\")\"; head -c 600 \"$p\"; done";
    let mut argv: Vec<String> = ["sh", "-c", script, "sh"].iter().map(|s| s.to_string()).collect();
    argv.extend(paths);
    let out = crate::sandbox::exec(app, agent, sid, &argv, None, 30).await;
    if !out.ok || out.text.trim().is_empty() {
        return String::new();
    }
    format!("\n\nThe files the task names hold this right now (first 600 bytes each). Check each against your final answer:{}", out.text.trim_end())
}

/// Whether `text` holds a model's raw tool-call syntax: the special tokens Kimi, GLM and Qwen
/// templates use, or MiniMax's. Plain prose and code about tool calls do not match.
fn leaked_tool_call(text: &str) -> bool {
    ["<|open|>argument", "<|close|>argument", "<|tool_call_begin|>", "<|tool_calls_section_begin|>", "<tool_call>\n{\"name\"", "]<]minimax[>[<invoke"]
        .iter()
        .any(|t| text.contains(t))
}

pub(crate) fn add_usage(stats: &mut crate::store::Stats, u: &model::Usage, info: Option<&model::ModelInfo>) {
    stats.model_calls += 1;
    stats.prompt_tokens += u.prompt;
    stats.cached_tokens += u.cached;
    stats.completion_tokens += u.completion;
    stats.reasoning_tokens += u.reasoning;
    stats.cost += info.map_or(0.0, |i| model::cost(i, u));
}

async fn run_tools(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, calls: &[Value], run_span: &str) -> Result<()> {
    let name = |c: &Value| c["function"]["name"].as_str().unwrap_or("").to_string();
    let record = |c: &Value, out: String| -> Result<()> {
        app.store.push_message(sid, &json!({"role": "tool", "tool_call_id": c["id"], "content": out}))?;
        let mut stats = app.store.stats(sid)?;
        stats.tool_calls += 1;
        app.store.put_stats(sid, &stats)
    };
    let parsed = |c: &Value| serde_json::from_str::<Value>(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or_default();
    let (mut images, mut owners) = (vec![], vec![]);
    let mut keep = |c: &Value, out: tools::Output| {
        owners.extend(out.images.iter().map(|_| c["id"].clone()));
        images.extend(out.images);
    };
    // A call with no result yet is waiting on the session's host for an approval or an answer
    // (`Approve::Host`). It stays without a result, so the next run takes it up again.
    let mut waiting = false;
    if agent.cfg.batching && calls.len() > 1 && calls.iter().all(|c| tools::call_is_safe(agent, &name(c), &parsed(c))) {
        let outs = join_all(calls.iter().map(|c| one_tool(app, agent, sid, c, run_span))).await;
        for (c, out) in calls.iter().zip(outs) {
            let Some(out) = out else {
                waiting = true;
                continue;
            };
            record(c, out.text.clone())?;
            keep(c, out);
        }
    } else {
        // Each result is stored as it lands, so a crash only touches the call in flight.
        for c in calls {
            let Some(out) = one_tool(app, agent, sid, c, run_span).await else {
                waiting = true;
                break;
            };
            record(c, out.text.clone())?;
            keep(c, out);
        }
    }
    // Chat-completions tool messages carry text only, so images a tool read follow as one user
    // message. `for_calls` names the call behind each image, so the Messages API can put each one
    // inside its own tool_result instead.
    if !images.is_empty() {
        let mut content = vec![json!({"type": "text", "text": "Images from the tool calls above, in order."})];
        content.extend(images);
        app.store.push_message(sid, &json!({"role": "user", "content": content, "for_calls": owners}))?;
    }
    if waiting {
        app.store.set_status(sid, "waiting")?;
        return Err(anyhow::Error::new(Paused));
    }
    Ok(())
}

/// One model call, under the deadline's cap when there is one.
async fn chat_once(app: &App, ep: &model::Endpoint, body: &Value, sid: &str, cap: Option<std::time::Duration>) -> Result<model::Reply> {
    match cap {
        None => model::chat(app, ep, body, &mut |kind, data| app.emit(sid, kind, data)).await,
        Some(cap) => match capped_chat(app, ep, body, sid, cap).await {
            Some(r) => r,
            None => {
                // The retry runs uncapped: ending the run on a second slow call threw away work that the
                // task's own time limit would still have allowed (two DeepSWE runs, muse-spark).
                app.emit(sid, "model_timeout", json!({"secs": cap.as_secs()}));
                let mut low = body.clone();
                low["reasoning_effort"] = json!("low");
                model::chat(app, ep, &low, &mut |kind, data| app.emit(sid, kind, data)).await
            }
        },
    }
}

fn has_images(messages: &Value) -> bool {
    messages.as_array().into_iter().flatten().any(|m| m["content"].as_array().is_some_and(|p| p.iter().any(|p| p["type"] == "image_url")))
}

/// Replace a message's images with a note saying they were left out.
fn strip_images(m: &mut Value) {
    if let Some(parts) = m["content"].as_array_mut() {
        for p in parts.iter_mut().filter(|p| p["type"] == "image_url") {
            *p = json!({"type": "text", "text": "[image left out: the model's provider refused it; inspect the file with bash instead]"});
        }
    }
}

/// A message's size in estimated tokens: chars/4, with each image counted as a flat 1,500 tokens
/// rather than by the length of its base64.
fn tokens(m: &Value) -> f64 {
    match m["content"].as_array() {
        Some(parts) if parts.iter().any(|p| p["type"] == "image_url") => {
            let images = parts.iter().filter(|p| p["type"] == "image_url").count();
            let text: usize = parts.iter().filter_map(|p| p["text"].as_str()).map(str::len).sum();
            images as f64 * 1500.0 + text as f64 / 4.0
        }
        _ => m.to_string().len() as f64 / 4.0,
    }
}

/// One tool call's result, or None while it waits on the session's host for an approval or an answer.
async fn one_tool(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, call: &Value, run_span: &str) -> Option<tools::Output> {
    let mut images = vec![];
    let text = one_tool_text(app, agent, sid, call, run_span, &mut images).await?;
    Some(tools::Output { text, images })
}

async fn one_tool_text(app: &Arc<App>, agent: &Arc<Agent>, sid: &str, call: &Value, run_span: &str, images: &mut Vec<Value>) -> Option<String> {
    let id = call["id"].as_str().unwrap_or("");
    let name = call["function"]["name"].as_str().unwrap_or("");
    let raw = call["function"]["arguments"].as_str().unwrap_or("{}");
    let args: Value = match serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw }) {
        Ok(v) => v,
        Err(e) => return Some(format!("error: arguments are not valid JSON: {e}")),
    };
    let safe = tools::call_is_safe(agent, name, &args);
    // A side-effecting call that was running when the process died is not run again
    // blindly: the model is told and can check the state before retrying.
    // A durable tool is run again instead: its step journal lets it skip what it already did.
    let durable = agent.tools.iter().any(|t| t.name == name && t.durable);
    if !safe && !durable && app.store.find_event(sid, "tool_started", id).ok().flatten().is_some() {
        let out = "error: egin restarted while this call was running, so its effects are unknown. Check the current state before running it again.".to_string();
        app.emit(sid, "tool_result", json!({"call_id": id, "tool": name, "output": out, "interrupted": true}));
        return Some(out);
    }
    // A call taken up again after it waited for an answer was announced the first time.
    if app.store.find_event(sid, "tool_call", id).ok().flatten().is_none() {
        app.emit(sid, "tool_call", json!({"call_id": id, "tool": name, "args": args}));
    }
    let span = crate::Span::start(sid, Some(run_span), "tool.call");
    let out = if name == "ask_user" {
        ask_user(app, sid, id, &args).await?
    } else if name == super::output::TOOL {
        match super::output::schema(app, sid).map(|s| super::output::take(&s, &args)) {
            Some(Ok(v)) => {
                app.emit(sid, "output", json!({"call_id": id, "output": v}));
                "Recorded.".into()
            }
            Some(Err(e)) => format!("error: {e}. Call final_answer again with a value that fits the schema."),
            None => "error: this session has no output schema".into(),
        }
    } else if !approved(app, agent, sid, id, name, &args, safe).await? {
        "error: the user denied this tool call".to_string()
    } else if let Some(reason) = tools::middleware(agent, name, &args).await {
        format!("error: blocked by middleware: {reason}")
    } else {
        if !safe {
            app.emit(sid, "tool_started", json!({"call_id": id}));
        }
        let out = tools::CALL_ID.scope(id.to_string(), tools::execute_full(app, agent, sid, name, &args)).await;
        *images = out.images;
        compact(app, agent, sid, id, name, &args, out.text).await
    };
    span.end(app, &agent.exporters, json!({"egin.tool": name, "egin.output_chars": out.len()}));
    app.emit(sid, "tool_result", json!({"call_id": id, "tool": name, "output": out}));
    Some(out)
}

/// Whether the call may run. None while the session's host has yet to say (`Approve::Host`).
async fn approved(app: &App, agent: &Agent, sid: &str, id: &str, name: &str, args: &Value, safe: bool) -> Option<bool> {
    let policy = agent.cfg.approval.as_str();
    if policy == "never" || safe {
        return Some(true);
    }
    // A decision made before a restart still stands.
    if let Ok(Some(e)) = app.store.find_event(sid, "approval", id) {
        return Some(e["allow"].as_bool().unwrap_or(false));
    }
    // "once": a tool approved earlier in this session needs no second yes.
    if policy == "once" && app.store.tool_approved(sid, name).unwrap_or(false) {
        return Some(true);
    }
    let request = json!({"call_id": id, "tool": name, "args": args, "policy": policy});
    let allow = match app.approve {
        Approve::Auto => return Some(true),
        Approve::Stdin => {
            let line = stdin_line(format!("\nallow {name} {args}? [y/N] ")).await;
            line.trim().eq_ignore_ascii_case("y")
        }
        // The request is re-sent after a restart (the loop finds the unanswered call); the answer
        // is stored before it is acted on.
        Approve::Api => {
            let (allow, logged) = wait_for(app, sid, id, "approval_request", request).await;
            if logged {
                return Some(allow.as_bool().unwrap_or(false));
            }
            allow.as_bool().unwrap_or(false)
        }
        Approve::Host => return ask_host(app, sid, id, "approval_request", request),
    };
    app.emit(sid, "approval", json!({"call_id": id, "tool": name, "allow": allow}));
    Some(allow)
}

/// `Approve::Host`: put the request in the log, once, and leave the call without a result. The run
/// stops there; the host stores the answer and runs the session again, which takes the call up.
fn ask_host<T>(app: &App, sid: &str, id: &str, kind: &str, request: Value) -> Option<T> {
    if app.store.find_event(sid, kind, id).ok().flatten().is_none() {
        app.emit(sid, kind, request);
    }
    None
}

async fn stdin_line(prompt: String) -> String {
    eprint!("{prompt}");
    tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).ok();
        s
    })
    .await
    .unwrap_or_default()
}

/// Emit a request (an approval or a question) and wait for its answer over the HTTP API. The answer
/// may reach another process serving the same store, which stores it: the store is asked every few
/// seconds too. Returns the answer and whether it is already in the log.
async fn wait_for(app: &App, sid: &str, id: &str, kind: &str, request: Value) -> (Value, bool) {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.pending.lock().unwrap().insert(id.to_string(), tx);
    app.emit(sid, kind, request);
    let (stored, field) = if kind == "question" { ("answer", "answer") } else { ("approval", "allow") };
    let every = std::time::Duration::from_secs(3);
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    loop {
        tokio::select! {
            v = &mut rx => return (v.unwrap_or_default(), false),
            _ = poll.tick() => {
                if let Ok(Some(e)) = app.store.find_event(sid, stored, id) {
                    app.pending.lock().unwrap().remove(id);
                    return (e[field].clone(), true);
                }
            }
        }
    }
}

/// The `ask_user` tool: put a question to whoever is driving the session and wait for the answer.
/// Asked again after a restart, unless the answer was already stored. None while the session's host
/// has yet to answer (`Approve::Host`).
async fn ask_user(app: &App, sid: &str, id: &str, args: &Value) -> Option<String> {
    if let Ok(Some(e)) = app.store.find_event(sid, "answer", id) {
        return Some(e["answer"].as_str().unwrap_or("").to_string());
    }
    let question = args["question"].as_str().unwrap_or("").to_string();
    let options: Vec<String> = args["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(String::from)).collect();
    let mut logged = false;
    let answer = match app.approve {
        // Nobody is there to answer: an eval, or `egin run --yes`.
        Approve::Auto => return Some("No one is available to answer. Decide for yourself and say what you assumed.".into()),
        Approve::Stdin => {
            let choices = if options.is_empty() { String::new() } else { format!(" [{}]", options.join(" / ")) };
            stdin_line(format!("\n{question}{choices}\n> ")).await.trim().to_string()
        }
        Approve::Api => {
            let request = json!({"call_id": id, "question": question, "options": options});
            let (answer, stored) = wait_for(app, sid, id, "question", request).await;
            logged = stored;
            answer.as_str().unwrap_or("").to_string()
        }
        Approve::Host => return ask_host(app, sid, id, "question", json!({"call_id": id, "question": question, "options": options})),
    };
    if !logged {
        app.emit(sid, "answer", json!({"call_id": id, "answer": answer}));
    }
    Some(if answer.is_empty() { "The question was left unanswered. Decide for yourself and say what you assumed.".into() } else { answer })
}

/// Compaction on entry. A page or document (read_file, web_fetch) is read from the top, so it is
/// shown a part at a time with the offset that continues it. Other output keeps its head and tail.
/// Either way the full text is parked in the sandbox, where grep can search it.
async fn compact(app: &App, agent: &Agent, sid: &str, id: &str, name: &str, args: &Value, out: String) -> String {
    let page = matches!(name, "read_file" | "web_fetch" | "you_web_extract");
    let max = if page || name.ends_with("web_search") { agent.cfg.page_max } else { agent.cfg.tool_output_max };
    if !agent.cfg.compaction || out.len() <= max {
        return out;
    }
    // Parked outside the working folder: inside it, the files showed up in every `git status` of the task's repo
    // (78 of 86 DeepSWE runs), models spent turns deleting them, and a `git add -A` would commit them.
    let clean = |s: &str| s.replace(|c: char| !c.is_alphanumeric(), "_");
    let file = format!("/tmp/.egin/out/{}/{}.txt", clean(sid), clean(id));
    let saved = tools::write(app, agent, sid, &file, out.as_bytes()).await.is_ok();
    if page {
        let limit = out.floor_char_boundary(max);
        let end = out[..limit].rfind('\n').filter(|i| *i > max / 2).unwrap_or(limit);
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        // read_file counts its offset in lines, web_fetch in characters of the page.
        let next = if name == "read_file" { offset + out[..end].lines().count() } else { offset + end };
        let grep = if saved { format!(", or search the full text with grep in {file}") } else { String::new() };
        return format!("{}\n[{} more characters. To continue, call {name} again with offset={next}{grep}.]", &out[..end], out.len() - end);
    }
    let cut = |i: usize, up: bool| (0..=4).map(|d| if up { i + d } else { i.saturating_sub(d) }).find(|i| out.is_char_boundary(*i)).unwrap_or(0);
    let (head, tail) = (cut(max * 6 / 10, false), cut(out.len() - max * 4 / 10, true));
    // Says where the gap starts, so the model fetches just that part. A bare path sent it to `cat` the saved file,
    // which came back cut the same way: on FrontierHarness, cut outputs were followed by 40% more reads.
    let note = if saved {
        let line = out[..head].matches('\n').count();
        format!("; read them with read_file path={file} offset={line}, or grep that file")
    } else {
        String::new()
    };
    format!("{}\n[... {} chars omitted{note} ...]\n{}", &out[..head], tail - head, &out[tail..])
}

/// Elide old tool output, then summarise old turns, when the context nears the limit.
async fn shrink(
    app: &Arc<App>,
    agent: &Agent,
    sid: &str,
    mut msgs: Vec<(i64, Value)>,
    limit: u64,
    info: Option<&model::ModelInfo>,
) -> Result<Vec<(i64, Value)>> {
    const KEEP: usize = 8;
    let cfg = &agent.cfg;
    // ponytail: chars/4 token estimate; use the router's reported prompt_tokens if this misfires.
    let size = |m: &[(i64, Value)]| m.iter().map(|(_, v)| tokens(v)).sum::<f64>();

    if cfg.elision && size(&msgs) > cfg.elide_at * limit as f64 {
        let tool_idx: Vec<usize> = (0..msgs.len()).filter(|i| msgs[*i].1["role"] == "tool").collect();
        let mut count = 0;
        // Images outside the most recent messages are replaced by a note; the file can be read again.
        let recent = msgs.len().saturating_sub(cfg.keep_recent);
        for (seq, m) in msgs.iter_mut().take(recent) {
            if let Some(parts) = m["content"].as_array() {
                if parts.iter().any(|p| p["type"] == "image_url") {
                    let kept: Vec<Value> = parts
                        .iter()
                        .map(|p| {
                            if p["type"] == "image_url" {
                                json!({"type": "text", "text": "[image elided to save context; read the file again to see it]"})
                            } else {
                                p.clone()
                            }
                        })
                        .collect();
                    m["content"] = json!(kept);
                    app.store.update_message(sid, *seq, m)?;
                    count += 1;
                }
            }
        }
        // Eliding rewrites the prompt from the first stubbed output on, so the provider's cache misses everything
        // after it. Old outputs go only in batches worth a tenth of the limit: one at a time, as soon as the context
        // sat over the threshold, 1,333 of 1,568 FrontierHarness elisions stubbed a single output and each re-sent
        // ~24k uncached tokens to save ~2.5k.
        let old: Vec<usize> = tool_idx[..tool_idx.len().saturating_sub(cfg.keep_recent)]
            .iter()
            .copied()
            .filter(|&i| msgs[i].1["content"].as_str().map_or(0, str::len) > 200)
            .collect();
        let freed: usize = old.iter().map(|&i| msgs[i].1["content"].as_str().map_or(0, str::len)).sum();
        if freed as f64 / 4.0 >= 0.1 * limit as f64 {
            for i in old {
                let len = msgs[i].1["content"].as_str().map_or(0, str::len);
                msgs[i].1["content"] = json!(format!("[output elided to save context: {len} chars]"));
                app.store.update_message(sid, msgs[i].0, &msgs[i].1)?;
                count += 1;
            }
        }
        if count > 0 {
            app.emit(sid, "elided", json!({"count": count}));
        }
    }

    // The task message stays word for word: a summary of it loses the exact names and edge cases it is graded on.
    let first = if msgs.get(1).is_some_and(|(_, m)| m["role"] == "user") { 2 } else { 1 };
    if cfg.summarisation && size(&msgs) > cfg.summarise_at * limit as f64 && msgs.len() > KEEP + first + 1 {
        // Cut on a non-tool message so no tool result is separated from its call.
        let mut cut = msgs.len() - KEEP;
        while cut > first && msgs[cut].1["role"] == "tool" {
            cut -= 1;
        }
        if cut > first {
            let transcript: String = msgs[first..cut]
                .iter()
                .map(|(_, m)| {
                    let calls = m.get("tool_calls").map(|c| c.to_string()).unwrap_or_default();
                    let text: String = format!("{} {calls}", m["content"].as_str().unwrap_or("")).chars().take(2000).collect();
                    format!("{}: {text}\n", m["role"].as_str().unwrap_or(""))
                })
                .collect();
            let ask = format!("Summarise this agent transcript so the work can continue from the summary alone. Keep the task, decisions, file paths, facts found and what is left to do.\n\n{transcript}");
            let body = json!({"model": cfg.model, "messages": [{"role": "user", "content": ask}], "stream": true, "stream_options": {"include_usage": true}});
            let ep = model::Endpoint::resolve(app, &cfg.model, Some(cfg))?;
            let reply = model::chat(app, &ep, &body, &mut |_, _| {}).await?;
            let mut stats = app.store.stats(sid)?;
            add_usage(&mut stats, &reply.usage, info);
            app.store.put_stats(sid, &stats)?;
            let summary = format!("[Summary of earlier work]\n{}", reply.message["content"].as_str().unwrap_or(""));
            app.store.replace_range(sid, msgs[first].0, msgs[cut].0, &json!({"role": "user", "content": summary, "egin_summary": true}))?;
            app.emit(sid, "summarised", json!({"messages": cut - first}));
            msgs = app.store.messages(sid)?;
        }
    }
    Ok(msgs)
}
