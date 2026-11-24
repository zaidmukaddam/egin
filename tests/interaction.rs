//! Talking to a running session: messages that steer or queue, questions to the user, approvals that
//! hold for the session, hooks, and the agent served as an MCP server.

mod common;

use common::{bash, call, fx, say};
use egin::Approve;
use serde_json::{json, Value};
use std::time::Duration;

/// Poll the session's stored events until one of `kind` shows up; returns it.
async fn event(f: &common::Fx, sid: &str, kind: &str) -> Value {
    for _ in 0..200 {
        if let Some((_, e)) = f.app.store.events_after(sid, 0).unwrap().into_iter().find(|(_, e)| e["type"] == kind) {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no {kind} event");
}

async fn finished(f: &common::Fx, sid: &str) -> Value {
    event(f, sid, "done").await
}

fn kinds(f: &common::Fx, sid: &str, kind: &str) -> usize {
    f.app.store.events_after(sid, 0).unwrap().iter().filter(|(_, e)| e["type"] == kind).count()
}

async fn start(base: &str, message: &str) -> String {
    let r: Value = reqwest::Client::new().post(format!("{base}/sessions")).json(&json!({"message": message})).send().await.unwrap().json().await.unwrap();
    r["id"].as_str().unwrap().to_string()
}

async fn post(url: String, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new().post(url).json(&body).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or_default())
}

#[tokio::test]
async fn a_message_sent_mid_run_steers_the_next_step() {
    let f = fx(vec![bash("b1", "sleep 1"), say("done, and noted")], Approve::Auto).await;
    let base = f.serve(f.agent("")).await;
    let sid = start(&base, "do the thing").await;
    event(&f, &sid, "tool_call").await;

    let (status, r) = post(format!("{base}/sessions/{sid}/messages"), json!({"message": "also check the logs"})).await;
    assert_eq!((status, r["queued"].as_bool()), (200, Some(true)), "no 409 while it runs");
    assert_eq!(finished(&f, &sid).await["text"], "done, and noted");

    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 2, "the message joined the running turn instead of starting another");
    let msgs = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(
        (msgs[3]["role"].as_str(), msgs[4]["content"].as_str()),
        (Some("tool"), Some("also check the logs")),
        "after the tool result, never between a call and its result"
    );
    assert_eq!(kinds(&f, &sid, "queued"), 1);
}

#[tokio::test]
async fn with_mid_run_queue_the_message_waits_for_the_answer() {
    let f = fx(vec![bash("b1", "sleep 1"), say("first answer"), say("second answer")], Approve::Auto).await;
    let base = f.serve(f.agent("mid_run = \"queue\"")).await;
    let sid = start(&base, "first").await;
    event(&f, &sid, "tool_call").await;
    post(format!("{base}/sessions/{sid}/messages"), json!({"message": "second"})).await;
    assert_eq!(finished(&f, &sid).await["text"], "second answer");

    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1]["messages"].as_array().unwrap().last().unwrap()["role"], "tool", "the first answer comes before the queued message is seen");
    assert_eq!(reqs[2]["messages"].as_array().unwrap().last().unwrap()["content"], "second");
    assert_eq!(kinds(&f, &sid, "done"), 1, "one run, one done");
}

#[tokio::test]
async fn ask_user_waits_for_an_answer_over_the_api() {
    let ask = call(&[("q1", "ask_user", json!({"question": "Which environment?", "options": ["staging", "production"]}))]);
    let f = fx(vec![ask, say("deploying to staging")], Approve::Api).await;
    let agent = f.agent("ask = true");
    assert!(egin::tools::specs(&agent).iter().any(|s| s["function"]["name"] == "ask_user"));
    let base = f.serve(agent).await;
    let sid = start(&base, "deploy it").await;

    let q = event(&f, &sid, "question").await;
    assert_eq!((q["question"].as_str(), &q["options"]), (Some("Which environment?"), &json!(["staging", "production"])));
    let (status, _) = post(format!("{base}/sessions/{sid}/answers"), json!({"call_id": "nope", "answer": "x"})).await;
    assert_eq!(status, 404);
    let (status, _) = post(format!("{base}/sessions/{sid}/answers"), json!({"call_id": "q1", "answer": "staging"})).await;
    assert_eq!(status, 200);

    assert_eq!(finished(&f, &sid).await["text"], "deploying to staging");
    assert_eq!(f.tool_outputs(&sid), ["staging"]);
    assert_eq!(event(&f, &sid, "answer").await["answer"], "staging", "the answer is stored, so a restart does not ask again");
}

#[tokio::test]
async fn an_answer_given_before_a_restarted_run_resumes_is_kept_for_it() {
    let ask = call(&[("q1", "ask_user", json!({"question": "Which environment?"}))]);
    let f = fx(vec![say("deploying to production")], Approve::Api).await;
    let agent = f.agent("ask = true");
    // A run cut off while it waited on the question: the call and the question are stored.
    let sid = egin::agent::run::new_session(&f.app, &agent).unwrap();
    egin::agent::run::send(&f.app, &sid, "deploy it").unwrap();
    let mut asked = ask.clone();
    asked["role"] = json!("assistant");
    asked["tool_calls"] = json!([{"id": "q1", "type": "function", "function": {"name": "ask_user", "arguments": "{\"question\":\"Which environment?\"}"}}]);
    f.app.store.push_message(&sid, &asked).unwrap();
    f.app.emit(&sid, "question", json!({"call_id": "q1", "question": "Which environment?", "options": []}));
    f.app.store.set_status(&sid, "running").unwrap();

    // The new server has not picked the session up yet, and the answer arrives.
    let (_, srv) = egin::server::router(f.app.clone(), agent.clone(), None);
    let base = f.serve(agent).await;
    let (status, r) = post(format!("{base}/sessions/{sid}/answers"), json!({"call_id": "q1", "answer": "production"})).await;
    assert_eq!((status, r["stored"].as_bool()), (200, Some(true)));
    let (status, _) = post(format!("{base}/sessions/{sid}/answers"), json!({"call_id": "q1", "answer": "staging"})).await;
    assert_eq!(status, 409, "answered once");

    egin::server::resume_all(&srv).unwrap();
    assert_eq!(finished(&f, &sid).await["text"], "deploying to production");
    assert_eq!(f.tool_outputs(&sid), ["production"]);
    assert_eq!(kinds(&f, &sid, "question"), 1, "not asked again");
}

#[tokio::test]
async fn a_durable_tool_cut_off_by_a_crash_resumes_from_its_journal() {
    let f = fx(vec![say("charged and emailed")], Approve::Auto).await;
    f.write(
        "tools/checkout.sh",
        "#!/bin/sh\n# description: Charge the card, then email the receipt.\n# durable: true\ncat > /dev/null\n\
         did() { grep -q \"\\\"$1\\\"\" \"$EGIN_STEPS\" 2>/dev/null; }\n\
         if ! did charge; then echo charged >> charges.log; echo '{\"step\":\"charge\"}' >> \"$EGIN_STEPS\"; fi\n\
         if ! did email; then echo emailed >> emails.log; echo '{\"step\":\"email\"}' >> \"$EGIN_STEPS\"; fi\necho receipt sent\n",
        true,
    );
    let agent = f.agent("");
    // The process died after the charge went through and before the email: the journal says so.
    let sid = egin::agent::run::new_session(&f.app, &agent).unwrap();
    egin::agent::run::send(&f.app, &sid, "check out").unwrap();
    let call =
        json!({"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "checkout", "arguments": "{}"}}]});
    f.app.store.push_message(&sid, &call).unwrap();
    f.app.emit(&sid, "tool_started", json!({"call_id": "c1"}));
    f.write("workspace/charges.log", "charged\n", false);
    f.write("workspace/.egin/steps/c1.jsonl", "{\"step\":\"charge\"}\n", false);

    let out = egin::agent::run::run_session(f.app.clone(), agent.clone(), sid.clone()).await.unwrap();
    assert_eq!(out, "charged and emailed");
    assert_eq!(f.tool_outputs(&sid), ["receipt sent\n"], "run again, not reported as unknown");
    let read = |p: &str| std::fs::read_to_string(agent.workspace.join(p)).unwrap_or_default();
    assert_eq!((read("charges.log"), read("emails.log")), ("charged\n".into(), "emailed\n".into()), "the charge is not repeated");
}

#[tokio::test]
async fn a_locked_tool_appears_once_another_unlocks_it() {
    let script = vec![call(&[("c0", "refund", json!({}))]), call(&[("c1", "verify", json!({}))]), call(&[("c2", "refund", json!({}))]), say("refunded")];
    let f = fx(script, Approve::Auto).await;
    f.write("tools/verify.sh", "#!/bin/sh\n# description: Verify the customer.\n# unlocks: refund\ncat > /dev/null\necho '::data {\"kind\":\"badge\",\"verified\":true}'\necho verified\n", true);
    f.write("tools/refund.sh", "#!/bin/sh\n# description: Refund the order.\n# locked: true\ncat > /dev/null\necho refund issued\n", true);
    let agent = f.agent("");
    let (sid, answer) = f.go(&agent, "refund me").await;
    assert_eq!(answer.unwrap(), "refunded");

    let offered = |i: usize| f.mock.reqs()[i]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "refund");
    assert!(!offered(0) && !offered(1), "refund is hidden until verify succeeds");
    assert!(offered(2) && offered(3));
    let outs = f.tool_outputs(&sid);
    assert_eq!(outs[0], "error: refund is not available yet");
    assert_eq!(outs[1], "verified\n(Now available: refund)", "the data line is not shown to the model");
    assert_eq!(outs[2].trim(), "refund issued");
    let data = event(&f, &sid, "data").await;
    assert_eq!((data["call_id"].as_str(), &data["data"]), (Some("c1"), &json!({"kind": "badge", "verified": true})));

    f.write("tools/oops.sh", "#!/bin/sh\n# description: x\n# unlocks: verify\necho\n", true);
    let err = egin::agent::Agent::load(&f.dir, &toml::Table::new()).err().unwrap().to_string();
    assert!(err.contains("not a locked tool"), "{err}");
}

#[tokio::test]
async fn history_pages_start_at_a_turn_and_reach_the_beginning() {
    let f = fx(vec![say("one"), bash("b1", "echo x"), say("two"), say("three")], Approve::Auto).await;
    let agent = f.agent("");
    let sid = egin::agent::run::new_session(&f.app, &agent).unwrap();
    for text in ["first", "second", "third"] {
        egin::agent::run::send(&f.app, &sid, text).unwrap();
        egin::agent::run::run_session(f.app.clone(), agent.clone(), sid.clone()).await.unwrap();
    }
    let base = f.serve(agent).await;
    let get = |q: String| {
        let url = format!("{base}/sessions/{sid}/history?{q}");
        async move { reqwest::get(url).await.unwrap().json::<Value>().await.unwrap() }
    };
    let types = |page: &Value| page["events"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap().to_string()).collect::<Vec<_>>();

    let last = get("limit=2".into()).await;
    assert_eq!(types(&last).first().map(String::as_str), Some("user"), "widened back to the turn's user message");
    assert_eq!(last["events"][0]["text"], "third");
    let mut before = last["earlier"].clone();
    let mut seen = vec![];
    while let Some(b) = before.as_i64() {
        let page = get(format!("before={b}&limit=2")).await;
        assert_eq!(types(&page)[0], "user");
        seen.push(page["events"][0]["text"].as_str().unwrap().to_string());
        before = page["earlier"].clone();
    }
    assert_eq!(seen, ["second", "first"], "every earlier turn, then null at the start");
}

#[tokio::test]
async fn an_output_schema_ends_the_run_with_a_checked_value() {
    let bad = call(&[("f1", "final_answer", json!({"severity": "mid", "summary": "disk full"}))]);
    let good = call(&[("f2", "final_answer", json!({"severity": "high", "summary": "disk full on db-1"}))]);
    let f = fx(vec![say("It is a high severity disk issue."), bad, good], Approve::Api).await;
    let base = f.serve(f.agent("approval = \"ask\"")).await;
    let http = reqwest::Client::new();
    let schema = json!({"type": "object", "required": ["severity", "summary"],
        "properties": {"severity": {"enum": ["low", "high"]}, "summary": {"type": "string"}}});

    let refused = http.post(format!("{base}/sessions")).json(&json!({"message": "x", "output_schema": "nope"})).send().await.unwrap();
    assert_eq!(refused.status(), 400);
    let r: Value = http
        .post(format!("{base}/sessions"))
        .json(&json!({"message": "triage the alert", "output_schema": schema}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = r["id"].as_str().unwrap().to_string();

    let done = finished(&f, &sid).await;
    assert_eq!(done["output"], json!({"severity": "high", "summary": "disk full on db-1"}));
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 3, "a valid final_answer ends the run without another model call");
    assert!(reqs[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "final_answer"));
    assert!(
        reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("calling final_answer"),
        "a text answer is sent back"
    );
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].starts_with("error: $.severity must be one of"), "{}", outs[0]);
    assert_eq!(kinds(&f, &sid, "approval_request"), 0, "final_answer never waits for approval");
}

#[tokio::test]
async fn ask_user_with_no_one_there_says_so() {
    let ask = call(&[("q1", "ask_user", json!({"question": "Which environment?"}))]);
    let f = fx(vec![ask, say("assumed staging")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("ask = true"), "deploy it").await;
    assert!(f.tool_outputs(&sid)[0].starts_with("No one is available to answer"));
}

#[tokio::test]
async fn approval_once_asks_the_first_time_a_tool_is_called() {
    let f = fx(vec![bash("b1", "echo one"), bash("b2", "echo two"), say("both ran")], Approve::Api).await;
    let base = f.serve(f.agent("approval = \"once\"")).await;
    let sid = start(&base, "run both").await;
    let req = event(&f, &sid, "approval_request").await;
    assert_eq!((req["call_id"].as_str(), req["policy"].as_str()), (Some("b1"), Some("once")));
    post(format!("{base}/sessions/{sid}/approvals"), json!({"call_id": "b1", "allow": true})).await;

    assert_eq!(finished(&f, &sid).await["text"], "both ran");
    assert_eq!(kinds(&f, &sid, "approval_request"), 1, "the second bash call was not asked about");
    assert!(f.tool_outputs(&sid)[1].contains("two"));
    assert!(egin::agent::Agent::load(&f.dir, &toml::from_str("approval = \"sometimes\"").unwrap()).is_err());
}

#[tokio::test]
async fn hooks_see_events_and_done_can_send_the_agent_back() {
    let f = fx(vec![say("first try"), say("fixed it")], Approve::Auto).await;
    let log = f.dir.join("assistant.log");
    let flag = f.dir.join("checked");
    f.write("hooks/assistant", &format!("#!/bin/sh\ncat >> {}\necho >> {}\n", log.display(), log.display()), true);
    // The first answer fails the check; the second passes.
    f.write(
        "hooks/done",
        &format!("#!/bin/sh\ncat > /dev/null\nif [ ! -f {flag} ]; then touch {flag}; echo 'The tests fail: fix them.'; fi\n", flag = flag.display()),
        true,
    );

    let (sid, answer) = f.go(&f.agent(""), "make the tests pass").await;
    assert_eq!(answer.unwrap(), "fixed it");
    let reqs = f.mock.reqs();
    assert_eq!(reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"], "The tests fail: fix them.");
    assert_eq!(event(&f, &sid, "hook").await["message"], "The tests fail: fix them.");
    assert_eq!(kinds(&f, &sid, "done"), 1);

    // The other hooks run alongside, in order, with the event and its session on stdin.
    for _ in 0..100 {
        if std::fs::read_to_string(&log).unwrap_or_default().lines().filter(|l| !l.is_empty()).count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let lines: Vec<Value> = std::fs::read_to_string(&log).unwrap().lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.iter().map(|e| e["content"].as_str().unwrap()).collect::<Vec<_>>(), ["first try", "fixed it"]);
    assert_eq!(lines[0]["session"], sid);
}

#[tokio::test]
async fn the_agent_is_an_mcp_server_other_agents_can_call() {
    let f = fx(vec![say("hello"), say("hello again"), say("later"), say("from egin")], Approve::Auto).await;
    f.write("channels/mcp.toml", "description = \"the release checker\"\n", false);
    let base = f.serve_with(f.agent(""), Some("t0ken")).await;
    let http = reqwest::Client::new();
    let rpc = |body: Value, token: &str| http.post(format!("{base}/mcp")).bearer_auth(token).json(&body).send();
    let call = |id: u32, name: &str, args: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}});

    assert_eq!(rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), "wrong").await.unwrap().status(), 401, "behind EGIN_TOKEN");
    let init: Value = rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}}}), "t0ken")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(rpc(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), "t0ken").await.unwrap().status(), 202);
    let list: Value = rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), "t0ken").await.unwrap().json().await.unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["send_message", "get_session"]);
    assert!(tools[0]["description"].as_str().unwrap().contains("the release checker"));

    let first: Value = rpc(call(3, "send_message", json!({"message": "hi"})), "t0ken").await.unwrap().json().await.unwrap();
    assert_eq!(first["result"]["content"][0]["text"], "hello");
    let sid = first["result"]["structuredContent"]["session"].as_str().unwrap().to_string();
    let again: Value = rpc(call(4, "send_message", json!({"message": "and?", "session": sid})), "t0ken").await.unwrap().json().await.unwrap();
    assert_eq!(again["result"]["content"][0]["text"], "hello again");
    assert_eq!(f.mock.reqs()[1]["messages"].as_array().unwrap().len(), 4, "the session continued");

    // Without waiting: the call returns at once, get_session reports the answer.
    let started: Value = rpc(call(5, "send_message", json!({"message": "slow one", "wait": false})), "t0ken").await.unwrap().json().await.unwrap();
    let slow = started["result"]["structuredContent"]["session"].as_str().unwrap().to_string();
    let mut answer = Value::Null;
    for _ in 0..100 {
        let got: Value = rpc(call(6, "get_session", json!({"session": slow})), "t0ken").await.unwrap().json().await.unwrap();
        if got["result"]["structuredContent"]["status"] == "done" {
            answer = got["result"]["structuredContent"]["answer"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(answer, "later");
    let unknown: Value = rpc(call(7, "get_session", json!({"session": "nope"})), "t0ken").await.unwrap().json().await.unwrap();
    assert_eq!(unknown["result"]["isError"], true);

    // egin's own MCP client, from another agent, sees it like any other server.
    let other = fx(vec![], Approve::Auto).await;
    other.write("connections/checker.toml", &format!("url = \"{base}/mcp\"\nheaders = {{ Authorization = \"Bearer t0ken\" }}\n"), false);
    let caller = other.agent("");
    let out = egin::tools::mcp::call(&other.app, &caller, "checker__send_message", &json!({"message": "status?"})).await;
    assert_eq!(out.as_deref(), Some("from egin"));
}
