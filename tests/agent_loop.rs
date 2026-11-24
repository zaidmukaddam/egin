//! The agent folder and the loop: streaming, append-only context, batching, context management,
//! limits, durability across crashes and restarts, approvals, middleware, tracing.

mod common;

use axum::{routing::post, Json, Router};
use common::{bad_request, bash, call, fx, policy_block, say};
use egin::{
    agent::{run, Agent},
    App, Approve,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[tokio::test]
async fn loader_reads_the_folder_layout() {
    let f = fx(vec![], Approve::Auto).await;
    f.write("subagents/researcher/instructions.md", "You research.", false);
    f.write("schedules/daily.toml", "every_secs = 86400\nprompt = \"report\"\n", false);
    let a = f.agent("planning = true");
    assert_eq!(a.skills[0].name, "report");
    assert_eq!(a.skills[0].description, "How to write a findings report");
    assert_eq!((a.tools[0].name.as_str(), a.tools[0].safe), ("word_count", true));
    assert_eq!(a.subagents, ["researcher"]);
    assert_eq!((a.schedules[0].name.as_str(), a.schedules[0].every_secs), ("daily", Some(86400)));
    let prompt = a.system_prompt();
    assert!(prompt.contains("- report: How to write") && prompt.contains("Facts worth keeping") && prompt.contains("researcher"));
    let names: Vec<String> = egin::tools::specs(&a).iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["bash", "read_file", "write_file", "edit_file", "web_search", "web_fetch", "plan", "skill", "task", "word_count"]);
    // Unknown keys are an error, not silently ignored.
    assert!(Agent::load(&f.dir, &toml::from_str("compactoin = false").unwrap()).is_err());
}

#[tokio::test]
async fn a_tool_can_be_one_script_that_describes_itself() {
    let f = fx(vec![call(&[("c1", "shout", json!({"text": "hi"}))]), say("ok")], Approve::Auto).await;
    // Saved without the executable bit, as an editor would: loading sets it.
    f.write(
        "tools/shout.sh",
        "#!/bin/sh\n# description: Upper-case some text.\n# safe: true\n# param text (string, required): The text\n# param times (integer)\n\ntr a-z A-Z\n",
        false,
    );
    f.write("tools/helper.sh", "#!/bin/sh\n# a helper the tools call, not a tool\necho hi\n", false);
    let a = f.agent("");
    let shout = a.tools.iter().find(|t| t.name == "shout").expect("the script is a tool");
    assert_eq!((shout.description.as_str(), shout.safe), ("Upper-case some text.", true));
    assert_eq!(
        shout.parameters,
        json!({"type": "object", "properties": {"text": {"type": "string", "description": "The text"}, "times": {"type": "integer"}}, "required": ["text"]})
    );
    assert!(!a.tools.iter().any(|t| t.name == "helper"), "a script with no description is not a tool");
    let (sid, _) = f.go(&a, "go").await;
    assert_eq!(f.tool_outputs(&sid), [r#"{"TEXT":"HI"}"#]);

    // A misspelt key in a tool's JSON is an error, not a tool with no parameters.
    f.write("tools/bad.json", r#"{"description": "x", "input_schema": {}}"#, false);
    f.write("tools/bad.sh", "#!/bin/sh\ncat\n", true);
    let err = Agent::load(&f.dir, &toml::Table::new()).expect_err("rejected").to_string();
    assert!(err.contains("input_schema"), "{err}");
}

#[tokio::test]
async fn model_profiles_set_defaults_that_agent_toml_overrides() {
    let f = fx(vec![], Approve::Auto).await;
    f.write("profiles/mock.toml", "batching = false\nmax_turns = 7\n", false);
    f.write("profiles/mock.md", "Prefer short commands.", false);
    let a = f.agent("");
    assert_eq!((a.cfg.batching, a.cfg.max_turns), (false, 7));
    assert!(a.system_prompt().contains("Prefer short commands."));
    assert_eq!(f.agent("max_turns = 9").cfg.max_turns, 9, "explicit settings win over the profile");
    let off = f.agent("profile = false");
    assert!(off.cfg.batching && !off.system_prompt().contains("Prefer short"), "profile = false ignores it");
    f.write("profiles/mock.toml", "batchign = false\n", false);
    assert!(Agent::load(&f.dir, &toml::Table::new()).is_err(), "a bad profile fails to load");
}

#[tokio::test]
async fn cron_schedules_compute_the_next_run() {
    use chrono::TimeZone;
    let s = egin::agent::Schedule { name: "daily".into(), cron: Some("30 9 * * *".into()), every_secs: None, prompt: "x".into() };
    let now = chrono::Local.with_ymd_and_hms(2026, 11, 15, 9, 0, 0).unwrap();
    assert_eq!(s.next(now).unwrap(), Duration::from_secs(30 * 60));
    let later = chrono::Local.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap();
    assert_eq!(s.next(later).unwrap(), Duration::from_secs(23 * 3600 + 30 * 60));
    let f = fx(vec![], Approve::Auto).await;
    f.write("schedules/bad.toml", "cron = \"not a cron\"\nprompt = \"x\"\n", false);
    assert!(Agent::load(&f.dir, &toml::Table::new()).is_err(), "a bad cron fails at load time");
}

#[tokio::test]
async fn reply_streams_tokens_and_records_usage_and_cost() {
    let f = fx(vec![say("hello world")], Approve::Auto).await;
    let mut rx = f.app.events.subscribe();
    let (sid, out) = f.go(&f.agent(""), "hi").await;
    assert_eq!(out.unwrap(), "hello world");
    let mut tokens = String::new();
    while let Ok(e) = rx.try_recv() {
        if e.body["type"] == "token" {
            assert!(e.id.is_none(), "token deltas are not persisted");
            tokens += e.body["text"].as_str().unwrap();
        }
    }
    assert_eq!(tokens, "hello world");
    let s = f.app.store.stats(&sid).unwrap();
    assert_eq!((s.model_calls, s.prompt_tokens, s.cached_tokens, s.completion_tokens), (1, 100, 40, 10));
    assert!((s.cost - 84e-6).abs() < 1e-12, "60 fresh + 40 cached in, 10 out at mock prices, got {}", s.cost);
    assert_eq!(f.app.store.status(&sid).unwrap().unwrap(), "done");
}

#[tokio::test]
async fn tool_loop_keeps_the_context_append_only() {
    let script = vec![
        bash("c1", "echo hi-from-bash"),
        call(&[("c2", "write_file", json!({"path": "a.txt", "content": "one two"}))]),
        call(&[("c3", "edit_file", json!({"path": "a.txt", "old": "two", "new": "three"}))]),
        call(&[("c4", "read_file", json!({"path": "a.txt"}))]),
        say("done"),
    ];
    let f = fx(script, Approve::Auto).await;
    let (sid, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    assert_eq!(f.tool_outputs(&sid), ["hi-from-bash\n", "wrote 7 bytes", "edited", "one three"]);
    // Each request must extend the previous one byte for byte, or the prompt cache is lost.
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 5);
    for pair in reqs.windows(2) {
        let (prev, next) = (pair[0]["messages"].as_array().unwrap(), pair[1]["messages"].as_array().unwrap());
        assert_eq!(&next[..prev.len()], &prev[..]);
        assert_eq!(pair[0]["tools"], pair[1]["tools"]);
    }
}

#[tokio::test]
async fn safe_tool_calls_in_one_turn_run_concurrently() {
    let three = call(&[("a", "slow", json!({"n": 1})), ("b", "slow", json!({"n": 2})), ("c", "slow", json!({"n": 3}))]);
    let f = fx(vec![three.clone(), say("ok"), three, say("ok")], Approve::Auto).await;
    f.write("tools/slow.json", r#"{"description": "slow echo", "safe": true}"#, false);
    f.write("tools/slow.sh", "#!/bin/sh\nsleep 0.5\ncat\n", true);

    let t = Instant::now();
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert!(t.elapsed() < Duration::from_millis(1200), "three 0.5s calls took {:?}", t.elapsed());
    assert_eq!(f.tool_outputs(&sid), [r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":3}"#]);

    let t = Instant::now();
    f.go(&f.agent("batching = false"), "go").await.1.unwrap();
    assert!(t.elapsed() > Duration::from_millis(1400), "batching off must run them one by one");
    assert_eq!(f.mock.reqs().last().unwrap()["parallel_tool_calls"], false);
}

#[tokio::test]
async fn long_tool_output_is_compacted_on_entry() {
    // 60,000 chars on 500 lines, past the 50,000 default: 30,000 kept from the start, so the gap starts on line 250.
    let big = "for i in $(seq 500); do head -c 119 /dev/zero | tr '\\0' x; echo; done";
    let f = fx(vec![bash("c1", big), say("ok"), bash("c2", big), say("ok")], Approve::Auto).await;
    let a = f.agent("");
    let (sid, _) = f.go(&a, "go").await;
    let out = &f.tool_outputs(&sid)[0];
    let parked = format!("/tmp/.egin/out/{}/c1.txt", sid.replace(|c: char| !c.is_alphanumeric(), "_"));
    assert!(out.len() < 50300 && out.contains(&format!("chars omitted; read them with read_file path={parked} offset=250,")), "{}", &out[29_900..30_200]);
    assert_eq!(std::fs::read_to_string(&parked).unwrap().len(), 60000);
    assert!(!a.workspace.join(".egin").exists(), "nothing of egin's is left in the working folder");

    let (sid, _) = f.go(&f.agent("compaction = false"), "go").await;
    assert_eq!(f.tool_outputs(&sid)[0].len(), 60000);
}

#[tokio::test]
async fn old_tool_output_is_elided_near_the_limit() {
    let big = "head -c 4000 /dev/zero | tr '\\0' x";
    let f = fx(vec![bash("c1", big), bash("c2", big), say("ok")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("context_limit = 2000\nkeep_recent = 1\nsummarisation = false"), "go").await;
    out.unwrap();
    let reqs = f.mock.reqs();
    let tool = |req: usize, i: usize| {
        let msgs = reqs[req]["messages"].as_array().unwrap();
        msgs.iter().filter(|m| m["role"] == "tool").nth(i).unwrap()["content"].as_str().unwrap().to_string()
    };
    assert_eq!(tool(1, 0).len(), 4000, "nothing to elide while it is the most recent output");
    assert_eq!(tool(2, 0), "[output elided to save context: 4000 chars]");
    assert_eq!(tool(2, 1).len(), 4000);
}

#[tokio::test]
async fn old_tool_output_is_elided_in_batches_not_one_at_a_time() {
    // Over the threshold from the start (a 4,400-char task in a 2,000-token limit), each output a twentieth of it:
    // one old output is not worth a cache miss, two (a tenth of the limit) are.
    let small = "head -c 400 /dev/zero | tr '\\0' x";
    let f = fx(vec![bash("c1", small), bash("c2", small), bash("c3", small), say("ok")], Approve::Auto).await;
    let task = "x".repeat(4400);
    f.go(&f.agent("context_limit = 2000\nkeep_recent = 1\nsummarisation = false"), &task).await.1.unwrap();
    let reqs = f.mock.reqs();
    let tool = |req: usize, i: usize| {
        let msgs = reqs[req]["messages"].as_array().unwrap();
        msgs.iter().filter(|m| m["role"] == "tool").nth(i).unwrap()["content"].as_str().unwrap().to_string()
    };
    assert_eq!(tool(2, 0).len(), 400, "a single old output is left in place");
    assert_eq!(tool(3, 0), "[output elided to save context: 400 chars]");
    assert_eq!(tool(3, 1), "[output elided to save context: 400 chars]");
    assert_eq!(tool(3, 2).len(), 400);
}

#[tokio::test]
async fn a_huge_window_is_managed_as_context_focus_tokens() {
    let big = "head -c 4000 /dev/zero | tr '\\0' x";
    for (focus, elided) in [(2000, true), (0, false)] {
        let f = fx(vec![bash("c1", big), bash("c2", big), say("ok")], Approve::Auto).await;
        let cfg = format!("context_limit = 1000000\ncontext_focus = {focus}\nkeep_recent = 1\nsummarisation = false");
        f.go(&f.agent(&cfg), "go").await.1.unwrap();
        let first = f.mock.reqs()[2]["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap()["content"].as_str().unwrap().to_string();
        assert_eq!(first.starts_with("[output elided"), elided, "focus {focus}: {}", &first[..first.len().min(60)]);
    }
}

#[tokio::test]
async fn old_turns_are_summarised_when_elision_is_not_enough() {
    let mut script: Vec<Value> = (0..6).map(|i| bash(&format!("c{i}"), "head -c 1000 /dev/zero | tr '\\0' x")).collect();
    script.push(say("end"));
    let f = fx(script, Approve::Auto).await;
    let (sid, out) = f.go(&f.agent("context_limit = 2000\nelision = false"), "go").await;
    assert_eq!(out.unwrap(), "end");
    let msgs: Vec<Value> = f.app.store.messages(&sid).unwrap().into_iter().map(|(_, m)| m).collect();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["content"], "go", "the task message survives word for word");
    assert_eq!(msgs[2]["content"], "[Summary of earlier work]\nSUMMARY");
    // Every surviving tool result still follows the call that produced it.
    for (i, m) in msgs.iter().enumerate().filter(|(_, m)| m["role"] == "tool") {
        let id = &m["tool_call_id"];
        assert!(msgs[..i].iter().any(|p| p["tool_calls"].as_array().is_some_and(|c| c.iter().any(|c| &c["id"] == id))), "orphan tool result {id}");
    }
    assert!(f.app.store.events_after(&sid, 0).unwrap().iter().any(|(_, e)| e["type"] == "summarised"));
}

#[tokio::test]
async fn a_killed_run_resumes_without_repeating_a_model_call() {
    let f = fx(vec![say("finished")], Approve::Auto).await;
    let a = f.agent("");
    // The state a crash leaves behind: the model's tool call is stored, its result is not.
    let sid = run::new_session(&f.app, &a).unwrap();
    run::send(&f.app, &sid, "go").unwrap();
    let pending = json!({"role": "assistant", "content": "", "tool_calls": [
        {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"echo one\"}"}},
        {"id": "c2", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"echo two > resumed.txt\"}"}}]});
    f.app.store.push_message(&sid, &pending).unwrap();
    f.app.store.push_message(&sid, &json!({"role": "tool", "tool_call_id": "c1", "content": "one\n"})).unwrap();
    f.app.store.set_status(&sid, "running").unwrap();

    assert_eq!(run::run_session(f.app.clone(), a.clone(), sid.clone()).await.unwrap(), "finished");
    assert!(a.workspace.join("resumed.txt").exists(), "the unanswered call ran");
    assert_eq!(f.tool_outputs(&sid).len(), 2, "the answered call did not run again");
    assert_eq!(f.mock.reqs().len(), 1, "one model call, made only after both results existed");

    // Running a finished session again is a no-op.
    assert_eq!(run::run_session(f.app.clone(), a, sid).await.unwrap(), "finished");
    assert_eq!(f.mock.reqs().len(), 1);
}

#[tokio::test]
async fn an_interrupted_side_effecting_call_is_not_rerun_blindly() {
    let f = fx(vec![say("checked")], Approve::Auto).await;
    let a = f.agent("");
    let sid = run::new_session(&f.app, &a).unwrap();
    run::send(&f.app, &sid, "go").unwrap();
    let pending = json!({"role": "assistant", "content": "", "tool_calls": [
        {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"echo again >> twice.txt\"}"}},
        {"id": "c2", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"twice.txt\"}"}}]});
    f.app.store.push_message(&sid, &pending).unwrap();
    // The crash hit while c1 ran; c2 (safe) had not started.
    f.app.emit(&sid, "tool_started", json!({"call_id": "c1"}));
    std::fs::write(a.workspace.join("twice.txt"), "again\n").unwrap();
    run::run_session(f.app.clone(), a.clone(), sid.clone()).await.unwrap();
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].starts_with("error: egin restarted while this call was running"), "{}", outs[0]);
    assert_eq!(outs[1], "again\n", "the command did not run a second time");
}

#[tokio::test]
async fn an_approval_survives_a_restart() {
    let f = fx(vec![say("ran")], Approve::Api).await;
    let a = f.agent("approval = \"ask\"");
    let sid = run::new_session(&f.app, &a).unwrap();
    run::send(&f.app, &sid, "go").unwrap();
    let call = json!({"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"touch ok.txt\"}"}}]});
    f.app.store.push_message(&sid, &call).unwrap();
    // The approval was answered, then the process died before the command ran.
    f.app.emit(&sid, "approval", json!({"call_id": "c1", "allow": true}));
    let restarted = App::with(&egin::state_db(&f.dir), Approve::Api, "key", &f.mock.url, &f.mock.url).unwrap();
    let mut rx = restarted.events.subscribe();
    assert_eq!(run::run_session(restarted.clone(), a.clone(), sid).await.unwrap(), "ran");
    assert!(a.workspace.join("ok.txt").exists());
    while let Ok(e) = rx.try_recv() {
        assert_ne!(e.body["type"], "approval_request", "an answered approval is not asked again");
    }
}

#[tokio::test]
async fn a_denied_approval_is_reported_to_the_model() {
    let f = fx(vec![bash("c1", "touch denied.txt"), call(&[("c2", "word_count", json!({"text": "a b"}))]), say("ok")], Approve::Api).await;
    let a = f.agent("approval = \"ask\"");
    let mut rx = f.app.events.subscribe();
    let app = f.app.clone();
    tokio::spawn(async move {
        while let Ok(e) = rx.recv().await {
            if e.body["type"] == "approval_request" {
                assert_eq!(e.body["tool"], "bash", "safe tools must not ask");
                app.pending.lock().unwrap().remove(e.body["call_id"].as_str().unwrap()).unwrap().send(serde_json::json!(false)).unwrap();
            }
        }
    });
    let (sid, out) = f.go(&a, "go").await;
    out.unwrap();
    assert_eq!(f.tool_outputs(&sid), ["error: the user denied this tool call", "2\n"]);
    assert!(!a.workspace.join("denied.txt").exists());
}

#[tokio::test]
async fn middleware_can_block_a_tool_call() {
    let f = fx(vec![bash("c1", "rm -rf /tmp/x"), bash("c2", "echo fine"), say("ok")], Approve::Auto).await;
    f.write("middleware/pre_tool", "#!/bin/sh\nif grep -q 'rm -rf'; then echo 'no deletes'; exit 1; fi\n", true);
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert_eq!(f.tool_outputs(&sid), ["error: blocked by middleware: no deletes\n\n[exit 1]", "fine\n"]);
}

#[tokio::test]
async fn at_the_turn_limit_the_session_answers_from_what_it_found() {
    let f = fx(vec![bash("c1", "true"), bash("c2", "true"), say("best guess")], Approve::Auto).await;
    let (sid, out) = f.go(&f.agent("max_turns = 2"), "go").await;
    assert_eq!(out.unwrap(), "best guess");
    assert_eq!(f.app.store.status(&sid).unwrap().unwrap(), "done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 3, "one extra call to answer");
    // No tools on the answering call: some router models reject tool_choice "none".
    assert!(reqs[2].get("tools").is_none() && reqs[2].get("tool_choice").is_none());
    assert!(reqs[1].get("tools").is_some());
    let last = reqs[2]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().contains("turn limit reached (2)"));
    assert!(last.get("egin_wrap_up").is_none(), "egin's marker never reaches the provider");
}

#[tokio::test]
async fn the_turn_limit_still_stops_a_model_that_keeps_calling_tools() {
    // The mock answers with tool calls even when none are declared, like a provider that ignores that.
    let f = fx((0..5).map(|i| bash(&format!("c{i}"), "true")).collect(), Approve::Auto).await;
    let (sid, out) = f.go(&f.agent("max_turns = 2"), "go").await;
    assert!(out.unwrap_err().to_string().contains("turn limit reached (2)"));
    assert_eq!(f.app.store.status(&sid).unwrap().unwrap(), "error");
    assert_eq!(f.mock.reqs().len(), 3);
}

#[tokio::test]
async fn near_the_deadline_the_session_is_told_to_save_its_work_once() {
    // The first call's command takes the run past 85% of a 2s deadline, so the save note comes at once and
    // the earlier one never does.
    let f = fx(vec![bash("c1", "sleep 2"), bash("c2", "true"), say("committed")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("deadline = 2"), "go").await;
    assert_eq!(out.unwrap(), "committed");
    let reqs = f.mock.reqs();
    let system = reqs[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("time limit of about 1 minute,"), "the time budget is in the system prompt: {system}");
    assert_eq!(reqs.len(), 3, "the warning adds no model call of its own");
    let last = reqs[1]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().contains("time limit"), "{last}");
    assert!(last.get("egin_deadline").is_none(), "egin's marker never reaches the provider");
    assert!(reqs[1].get("tools").is_some(), "tools stay on: the model still has to save its work");
    let warnings = reqs[2]["messages"].as_array().unwrap().iter().filter(|m| m["content"].as_str().is_some_and(|c| c.contains("time limit stops you"))).count();
    assert_eq!(warnings, 1, "said once per run");
    assert!(last["content"].as_str().unwrap().contains("Save your work now"), "{last}");
}

#[tokio::test]
async fn before_the_save_note_the_session_is_told_to_get_a_working_result_in_place() {
    // A 10s deadline: the first command ends past 60%, the second past 85%.
    let f = fx(vec![bash("c1", "sleep 6.5"), bash("c2", "sleep 2.5"), bash("c3", "true"), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("deadline = 10"), "go").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 4, "neither note adds a model call of its own");
    let last = |i: usize| reqs[i]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap_or("").to_string();
    assert!(last(1).contains("put a working version of it in place now"), "{}", last(1));
    assert!(last(2).contains("Save your work now"), "{}", last(2));
    let notes = reqs[3]["messages"].as_array().unwrap().iter().filter(|m| m["content"].as_str().is_some_and(|c| c.contains("time limit stops you"))).count();
    assert_eq!(notes, 2, "each note once");
}

#[tokio::test]
async fn a_long_run_is_asked_to_finish_every_check_in_calls() {
    let f = fx(vec![bash("c1", "true"), bash("c2", "true"), bash("c3", "true"), bash("c4", "true"), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("check_in = 2"), "go").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 5, "the note adds no model call of its own");
    let last = |i: usize| reqs[i]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last(1)["content"].as_str().is_none_or(|c| !c.contains("model calls on this task")), "not before two calls: {}", last(1));
    assert!(last(2)["content"].as_str().unwrap().contains("You have made 2 model calls on this task"), "{}", last(2));
    assert!(last(2).get("egin_check_in").is_none(), "egin's marker never reaches the provider");
    assert!(last(4)["content"].as_str().unwrap().contains("You have made 4 model calls on this task"), "{}", last(4));
    let notes = reqs[4]["messages"].as_array().unwrap().iter().filter(|m| m["content"].as_str().is_some_and(|c| c.contains("model calls on this task"))).count();
    assert_eq!(notes, 2, "once at each multiple");
}

#[tokio::test]
async fn a_content_policy_block_is_retried_but_other_bad_requests_end_the_run() {
    // The router's content policy refused a harmless request mid-run and let the same request through on
    // replay, so the session retries it instead of ending.
    let f = fx(vec![bash("c1", "true"), policy_block(), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1], reqs[2], "the blocked request is sent again unchanged");
    // A 408 (the router timing out a hung call) is retried like a server error.
    let f = fx(vec![bash("c1", "true"), json!({"status": 408}), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    assert_eq!(f.mock.reqs().len(), 3);
    // Any other 400 is the request's own fault and still ends the run on the first answer.
    let f = fx(vec![bad_request("Invalid 'messages[3].content'"), say("never")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert!(out.unwrap_err().to_string().contains("400"));
    assert_eq!(f.mock.reqs().len(), 1);
}

#[tokio::test]
async fn a_tool_call_leaked_as_text_is_sent_back_once_instead_of_ending_the_run() {
    let leaked = "Checking.<|close|>argument<|sep|><|open|>argument key=\"command\" type=\"string\"<|sep|>git commit -am done<|close|>argument";
    let f = fx(vec![say(leaked), bash("c1", "true"), say("done"), say(leaked)], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 3);
    let note = reqs[1]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(note["content"].as_str().unwrap().contains("not run"), "{note}");
    assert!(note.get("egin_leak_nudge").is_none(), "egin's marker never reaches the provider");
    // A second leak in the same turn ends the run as before, rather than looping.
    let f = fx(vec![say(leaked), say(leaked)], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), leaked);
    assert_eq!(f.mock.reqs().len(), 2);
}

#[tokio::test]
async fn with_review_on_a_fresh_reviewer_checks_the_work_before_the_run_ends() {
    // The reviewer (the third call) gets the task, the answer and the work, none of the agent's turns, and
    // its findings go back to the agent once.
    let f = fx(vec![bash("c1", "echo x > f"), say("done"), say("- f holds x, but the task says \"write y to f\""), say("fixed, done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "write y to f").await;
    assert_eq!(out.unwrap(), "fixed, done");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 4);
    assert!(!reqs[0]["messages"][0]["content"].as_str().unwrap().contains("every existing form"), "the checks live in the reviewer, not the agent's prompt");
    let review = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(review.len(), 2, "a fresh context: its own system prompt and one message");
    assert!(review[0]["content"].as_str().unwrap().contains("You review another agent's work"));
    let shown = review[1]["content"].as_str().unwrap();
    assert!(shown.contains("write y to f") && shown.contains("# Agent's final answer\ndone"), "{shown}");
    assert!(shown.contains("## ./f\nx\n"), "outside git, the changed files are shown in full: {shown}");
    let note = reqs[3]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(note["content"].as_str().unwrap().contains("independent review") && note["content"].as_str().unwrap().contains("f holds x"), "{note}");
    assert!(note.get("egin_review").is_none(), "egin's marker never reaches the provider");
    // A reviewer that finds nothing, or nothing it can tie to the task's own words, ends the run without another turn.
    for verdict in [
        "OK",
        "- main.cpp was left in the folder and was not requested",
        "- the task wants \"a different file\"",
        "I'll check \"write y to f\" by reading the changed files first.",
    ] {
        let f = fx(vec![bash("c1", "echo y > f"), say("done"), say(verdict)], Approve::Auto).await;
        let (_, out) = f.go(&f.agent("review = true"), "write y to f").await;
        assert_eq!(out.unwrap(), "done", "{verdict}");
        assert_eq!(f.mock.reqs().len(), 3, "{verdict}");
    }
    // The files the task names go to the reviewer as they are now, so a stale draft is visible.
    let path = std::env::temp_dir().join(format!("egin-review-{}", std::process::id())).join("out.txt");
    let cmd = format!("mkdir -p '{}' && echo '{{gn}}' > '{}'", path.parent().unwrap().display(), path.display());
    let f = fx(
        vec![bash("c1", &cmd), say("flag{x}"), say("- out.txt holds {gn}, not flag{x}; the task says \"write the answer to\""), say("flag{x}")],
        Approve::Auto,
    )
    .await;
    let (_, out) = f.go(&f.agent("review = true"), &format!("Decode it and write the answer to `{}`.", path.display())).await;
    assert_eq!(out.unwrap(), "flag{x}");
    let shown = f.mock.reqs()[2]["messages"][1]["content"].as_str().unwrap().to_string();
    assert!(shown.contains("hold this right now") && shown.contains("{gn}"), "the stale draft is shown: {shown}");
    // A run that changed nothing is told so; off by default, the first answer ends the run.
    let f = fx(vec![say("done"), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "go").await;
    assert_eq!(out.unwrap(), "done");
    assert!(f.mock.reqs()[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("not changed anything"));
    let f = fx(vec![say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    assert_eq!(f.mock.reqs().len(), 1);
}

#[tokio::test]
async fn the_reviewer_sees_work_the_run_has_already_committed() {
    // A repository from before the run (an old commit), then the run's own change, committed as tasks ask.
    let setup = "git init -q . && echo x > f && git add f && GIT_COMMITTER_DATE=2020-01-01T00:00:00 git -c user.email=a@b -c user.name=a commit -qm base";
    let work = "echo y > f && git -c user.email=a@b -c user.name=a commit -qam 'write y' && echo committed";
    let f = fx(vec![bash("c1", setup), bash("c2", work), say("done"), say("OK")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "write y to f and commit").await;
    assert_eq!(out.unwrap(), "done");
    let shown = f.mock.reqs()[3]["messages"][1]["content"].as_str().unwrap().to_string();
    assert!(shown.contains("write y") && shown.contains("+y"), "the committed change is in the diff: {shown}");
}

#[tokio::test]
async fn a_helper_is_a_fresh_copy_on_the_same_files() {
    // The parent asks a helper; the helper's two calls come next in the script, then the parent finishes.
    let ask = call(&[("c1", "helper", json!({"prompt": "What does the file f hold?"}))]);
    let f = fx(vec![ask, bash("h1", "cat f"), say("f holds y"), say("done")], Approve::Auto).await;
    let a = f.agent("helper = true");
    std::fs::write(a.workspace.join("f"), "y\n").unwrap();
    let (sid, out) = f.go(&a, "find out what f holds").await;
    assert_eq!(out.unwrap(), "done");
    assert_eq!(f.tool_outputs(&sid)[0], "f holds y", "the helper's answer is the tool result");
    let reqs = f.mock.reqs();
    let names = |i: usize| reqs[i]["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(names(0).contains(&"helper".to_string()));
    let first = reqs[1]["messages"].as_array().unwrap();
    assert_eq!((first.len(), first[1]["content"].as_str().unwrap()), (2, "What does the file f hold?"), "a fresh context: system + the prompt");
    assert!(!names(1).contains(&"helper".to_string()), "a copy gets no helper of its own");
    assert!(reqs[2]["messages"].as_array().unwrap().iter().any(|m| m["content"].as_str().is_some_and(|c| c.contains('y'))), "it read the parent's file");
    let stats = f.app.store.stats(&sid).unwrap();
    assert_eq!(stats.prompt_tokens, 400, "the helper's two calls count toward the parent's tokens");
    assert!(!names_of(&f.agent("")).contains(&"helper".to_string()), "off by default");
}

fn names_of(a: &egin::agent::Agent) -> Vec<String> {
    let app = egin::App::with(
        &std::env::temp_dir().join(format!("egin-names-{}.db", std::process::id())),
        Approve::Auto,
        "k",
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    )
    .unwrap();
    egin::tools::specs_in(&app, a, "s").iter().map(|t| t["function"]["name"].as_str().unwrap_or("").to_string()).collect()
}

#[tokio::test]
async fn silencing_a_test_that_existed_before_the_run_is_sent_back() {
    let setup = "git init -q . && mkdir tests && printf 'def test_a():\\n    assert f()\\n' > tests/test_a.py && git add -A && GIT_COMMITTER_DATE=2020-01-01T00:00:00 git -c user.email=a@b -c user.name=a commit -qm base";
    let silence = "printf 'import pytest\\n@pytest.mark.xfail\\ndef test_a():\\n    assert f()\\n' > tests/test_a.py && printf 'def test_b(): pass\\n@pytest.mark.skip\\ndef test_c(): pass\\n' > tests/test_new.py";
    let f = fx(vec![bash("c1", setup), bash("c2", silence), say("done"), say("fixed"), say("OK")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "make f work").await;
    assert_eq!(out.unwrap(), "fixed");
    let note = f.mock.reqs()[3]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(note.contains("tests/test_a.py: @pytest.mark.xfail"), "{note}");
    assert!(!note.contains("test_new.py"), "a test file the run created is its own to mark: {note}");
    // Once per turn, and not when the task asks for it.
    let f = fx(vec![bash("c1", setup), bash("c2", silence), say("done"), say("OK")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "skip the flaky test_a").await;
    assert_eq!(out.unwrap(), "done");
}

#[tokio::test]
async fn a_script_that_needs_a_package_the_run_installed_is_sent_back() {
    let last = |f: &common::Fx, i: usize| f.mock.reqs()[i]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap_or("").to_string();
    // "Installing" two packages: files written during the run under a site-packages folder on Python's path. The
    // script imports one and names the other only in a string, as bs4 takes a parser. `json` came with Python.
    let install = "mkdir -p ../libs/site-packages/fakedep ../libs/site-packages/fakeparser && touch ../libs/site-packages/fakedep/__init__.py ../libs/site-packages/fakeparser/__init__.py";
    let write = "printf 'import json\\nimport fakedep\\nparse(\"fakeparser\")\\n' > tool.py";
    let f = fx(vec![bash("c1", install), bash("c2", write), say("done"), say("kept"), say("OK")], Approve::Auto).await;
    std::env::set_var("PYTHONPATH", f.dir.join("libs/site-packages"));
    let (_, out) = f.go(&f.agent("review = true"), "write tool.py").await;
    assert_eq!(out.unwrap(), "kept");
    assert!(last(&f, 3).contains("tool.py: fakedep\ntool.py: fakeparser\n"), "{}", last(&f, 3));
    assert!(!last(&f, 3).contains("json"), "what was there at the start is not named: {}", last(&f, 3));
    // Not when the task asks for the package.
    let f = fx(vec![bash("c1", install), bash("c2", "printf 'import fakedep\\n' > tool.py"), say("done"), say("OK")], Approve::Auto).await;
    std::env::set_var("PYTHONPATH", f.dir.join("libs/site-packages"));
    let (_, out) = f.go(&f.agent("review = true"), "write tool.py with fakedep").await;
    assert_eq!(out.unwrap(), "done");
}

#[tokio::test]
async fn no_review_when_the_repository_shows_no_change() {
    // The agent wrote (here: outside the repository), but the working folder's repository shows no difference:
    // no reviewer call at all, rather than a reviewer told that nothing changed.
    let setup = "git init -q . && echo x > f && git add -A && GIT_COMMITTER_DATE=2020-01-01T00:00:00 git -c user.email=a@b -c user.name=a commit -qm base";
    let f = fx(vec![bash("c1", setup), bash("c2", "echo y > /tmp/egin-outside-repo-$$"), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "write y somewhere").await;
    assert_eq!(out.unwrap(), "done");
    assert_eq!(f.mock.reqs().len(), 3, "no reviewer call");
}

#[tokio::test]
async fn unattended_runs_keep_git_history_unless_the_task_asks_to_change_it() {
    let purge = "git filter-repo --replace-text r.txt && git reflog expire --expire=now --all && git gc --prune=now";
    let f = fx(vec![bash("c1", purge), bash("c2", "echo l-bfgs ok"), say("done"), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "remove the API keys from the repo").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    let result = |i: usize| reqs[i]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap_or("").to_string();
    assert!(result(1).contains("not run") && result(1).contains("git history"), "{}", result(1));
    assert!(result(2).contains("l-bfgs ok"), "an optimizer named bfgs is not the BFG cleaner: {}", result(2));
    // A task about history may rewrite it.
    let f = fx(vec![bash("c1", "git filter-repo --help >/dev/null 2>&1; echo ran"), say("done"), say("OK")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "purge the leaked key from the git history").await;
    assert_eq!(out.unwrap(), "done");
    let reqs = f.mock.reqs();
    assert!(reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("ran"));
}

#[tokio::test]
async fn an_empty_reply_is_asked_again_then_fails_the_run() {
    let f = fx(vec![say(""), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    assert!(f.mock.reqs()[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("was empty"));
    // A reply that only reasoned gets the end of its reasoning back, and a few tries before the run fails.
    let thought = json!({"content": "", "reasoning": "The totals disagree. Next I'll rerun the parser on sample.xls."});
    let f = fx(vec![thought.clone(), thought.clone(), say("done")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "done");
    let nudge = f.mock.reqs()[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(nudge.contains("rerun the parser on sample.xls") && nudge.contains("Act on it"), "{nudge}");
    let f = fx(vec![say(""), say(""), say(""), say("")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent(""), "go").await;
    assert!(out.unwrap_err().to_string().contains("empty reply"));
    assert_eq!(f.mock.reqs().len(), 4);
}

#[tokio::test]
async fn a_command_still_running_at_the_end_is_named_once_before_the_run_ends() {
    let last = |f: &common::Fx, i: usize| f.mock.reqs()[i]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap_or("").to_string();
    let slow = call(&[("c1", "bash", json!({"command": "sleep 6 # held", "timeout_secs": 1}))]);
    let f = fx(vec![slow, say("done"), say("it can stay"), say("final")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "go").await;
    assert_eq!(out.unwrap(), "final");
    assert!(last(&f, 2).contains("still running in the background:\nsleep 6 # held  (stop with: kill -- -"), "{}", last(&f, 2));
    assert!(!last(&f, 3).contains("still running in the background:"), "asked once: {}", last(&f, 3));
    // A command that has finished by then is not mentioned.
    let quick = call(&[("c1", "bash", json!({"command": "sleep 1.3", "timeout_secs": 1}))]);
    let f = fx(vec![quick, bash("c2", "sleep 1"), say("done"), say("final")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("review = true"), "go").await;
    assert_eq!(out.unwrap(), "final");
    assert!((0..f.mock.reqs().len()).all(|i| !last(&f, i).contains("still running in the background:")));
}

#[tokio::test]
async fn a_session_exports_as_an_atif_trajectory() {
    let f = fx(vec![bash("c1", "echo hi"), say("done")], Approve::Auto).await;
    let (sid, out) = f.go(&f.agent(""), "say hi").await;
    assert_eq!(out.unwrap(), "done");
    let t = egin::atif::export(&f.app, &sid).unwrap();
    assert_eq!(t["schema_version"], "ATIF-v1.7");
    assert_eq!(t["agent"]["name"], "egin");
    let steps = t["steps"].as_array().unwrap();
    let ids: Vec<u64> = steps.iter().map(|s| s["step_id"].as_u64().unwrap()).collect();
    assert_eq!(ids, (1..=steps.len() as u64).collect::<Vec<_>>(), "sequential from 1");
    let sources: Vec<&str> = steps.iter().map(|s| s["source"].as_str().unwrap()).collect();
    assert_eq!(sources, ["system", "user", "agent", "agent"]);
    assert_eq!(steps[1]["message"], "say hi");
    // The tool result sits in the observation of the step that made the call, keyed by its id.
    let call = &steps[2]["tool_calls"][0];
    assert_eq!((call["function_name"].as_str(), call["arguments"]["command"].as_str()), (Some("bash"), Some("echo hi")));
    let result = &steps[2]["observation"]["results"][0];
    assert_eq!(result["source_call_id"], call["tool_call_id"]);
    assert!(result["content"].as_str().unwrap().contains("hi"));
    assert_eq!(steps[3]["message"], "done");
    assert!(steps[2]["metrics"]["prompt_tokens"].is_u64());
    // Agent-only fields never appear on other steps; every step has a timestamp.
    for s in &steps[..2] {
        for k in ["tool_calls", "metrics", "reasoning_content", "model_name"] {
            assert!(s.get(k).is_none(), "{k} on a {} step", s["source"]);
        }
    }
    assert!(steps[1..].iter().all(|s| s["timestamp"].as_str().is_some_and(|t| t.ends_with('Z'))));
    assert_eq!(t["final_metrics"]["total_steps"], 4);
    // Left for `python -m harbor.utils.trajectory_validator`, which this test cannot run itself.
    std::fs::write(std::env::temp_dir().join("egin-atif-test.json"), serde_json::to_string_pretty(&t).unwrap()).unwrap();
}

#[tokio::test]
async fn file_edits_export_under_the_names_trajectory_tools_replay() {
    let f = fx(
        vec![
            call(&[("w", "write_file", json!({"path": "a.txt", "content": "hello world"}))]),
            call(&[("e", "edit_file", json!({"path": "a.txt", "old": "hello", "new": "bye"}))]),
            say("ok"),
        ],
        Approve::Auto,
    )
    .await;
    let (sid, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "ok");
    let t = egin::atif::export(&f.app, &sid).unwrap();
    let calls: Vec<&Value> = t["steps"].as_array().unwrap().iter().filter_map(|s| s["tool_calls"].get(0)).collect();
    assert_eq!(calls[0]["function_name"], "write_file");
    assert_eq!(calls[0]["arguments"], json!({"path": "a.txt", "content": "hello world"}));
    assert_eq!(calls[1]["function_name"], "edit");
    assert_eq!(calls[1]["arguments"], json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"}));
    assert_eq!(calls[1]["extra"]["egin_tool"], "edit_file", "egin's own form is kept");
    std::fs::write(std::env::temp_dir().join("egin-atif-edit.json"), serde_json::to_string_pretty(&t).unwrap()).unwrap();
}

#[tokio::test]
async fn at_the_spend_limit_the_session_answers_once_more() {
    // Each mock call costs $0.000084, so the limit is passed after two calls.
    let f = fx(vec![bash("c1", "true"), bash("c2", "true"), say("answer within budget")], Approve::Auto).await;
    let (_, out) = f.go(&f.agent("max_cost = 0.0001"), "go").await;
    assert_eq!(out.unwrap(), "answer within budget");
    assert_eq!(f.mock.reqs().len(), 3);
    assert!(f.mock.reqs()[2].get("tools").is_none());
}

#[tokio::test]
async fn spans_are_exported_over_otlp() {
    let spans = Arc::new(Mutex::new(vec![]));
    let sink = spans.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    let collector = post(move |Json(b): Json<Value>| async move { sink.lock().unwrap().push(b["resourceSpans"][0]["scopeSpans"][0]["spans"][0].clone()) });
    let router = Router::new().route("/v1/traces", collector);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let mut f = fx(vec![bash("c1", "true"), say("ok")], Approve::Auto).await;
    Arc::get_mut(&mut f.app).unwrap().otlp = Some(egin::telemetry::Exporter { endpoint: url, ..Default::default() });
    f.go(&f.agent(""), "go").await.1.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let spans = spans.lock().unwrap().clone();
    let mut names: Vec<&str> = spans.iter().map(|s| s["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["model.call", "model.call", "session.run", "tool.call"]);
    let root = spans.iter().find(|s| s["name"] == "session.run").unwrap();
    assert!(spans.iter().all(|s| s["traceId"] == root["traceId"] && s["traceId"].as_str().unwrap().len() == 32));
    assert!(spans.iter().filter(|s| s["name"] != "session.run").all(|s| s["parentSpanId"] == root["spanId"]));
}

#[tokio::test]
async fn an_instrumentation_file_gets_spans_with_its_headers() {
    let seen = Arc::new(Mutex::new(vec![]));
    let sink = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    let collector = post(move |headers: axum::http::HeaderMap, Json(b): Json<Value>| async move {
        let key = headers.get("x-team").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        sink.lock().unwrap().push((key, b["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"].clone()));
    });
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/v1/traces", collector)).await.unwrap() });

    let f = fx(vec![say("ok")], Approve::Auto).await;
    std::env::set_var("EGIN_TEST_TEAM_KEY", "secret-123");
    f.write("instrumentation/collector.toml", &format!("endpoint = \"{url}\"\nheaders = {{ x-team = \"${{EGIN_TEST_TEAM_KEY}}\" }}\n"), false);
    f.go(&f.agent(""), "go").await.1.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().any(|(_, n)| n == "session.run"), "{seen:?}");
    assert!(seen.iter().all(|(k, _)| k == "secret-123"), "the header's ${{VAR}} is read from the environment: {seen:?}");
}
