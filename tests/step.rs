//! `egin step`: a session its host stores, advanced one bounded step at a time by processes that
//! share nothing but what the host hands them.

mod common;

use common::{bash, call, fx, say, Fx};
use egin::{agent::step::step, store::Store, App, Approve};
use serde_json::{json, Value};

/// One step in a process of its own: a new App, with a store that holds nothing yet.
async fn advance(f: &Fx, session: &Value, extra: Value) -> (Value, String) {
    advance_as(f, "", session, extra).await
}

/// The same, for an agent with these agent.toml overrides.
async fn advance_as(f: &Fx, agent: &str, session: &Value, extra: Value) -> (Value, String) {
    let app = App::with_store(Store::memory().unwrap(), Approve::Host, "key", &f.mock.url, &f.mock.url).unwrap();
    let mut input = json!({"id": "s1", "messages": session["messages"], "events": session["events"], "stats": session["stats"]});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let (mut tokens, mut seen) = (String::new(), vec![]);
    // A `cancel_after` in the input is the host cancelling that many milliseconds into the step.
    let after = input["cancel_after"].as_u64();
    let cancel = async move {
        match after {
            Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
            None => std::future::pending().await,
        }
    };
    let out = step(&app, &f.agent(agent), &input, cancel, |e, stored| match stored {
        true => seen.push(e["type"].clone()),
        false => tokens += e["text"].as_str().unwrap_or(""),
    })
    .await
    .unwrap();
    // The host keeps the whole log: what it had, then what the step added. `seen` is the stored
    // events the step showed while it ran.
    let mut events = session["events"].as_array().cloned().unwrap_or_default();
    events.extend(out["events"].as_array().unwrap().iter().cloned());
    (json!({"messages": out["messages"], "events": events, "stats": out["stats"], "status": out["status"], "seen": seen}), tokens)
}

fn roles(session: &Value) -> Vec<&str> {
    session["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect()
}

fn kinds(session: &Value) -> Vec<&str> {
    session["events"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn a_session_advances_one_model_call_at_a_time_across_processes() {
    let f = fx(vec![bash("c1", "echo hi > made.txt"), say("all done")], Approve::Auto).await;

    let (s, _) = advance(&f, &json!({}), json!({"calls": 0})).await;
    assert_eq!((roles(&s), s["status"].as_str()), (vec!["system"], Some("idle")), "a new session holds the agent's instructions");

    let (s, _) = advance(&f, &s, json!({"message": "go", "calls": 0})).await;
    assert_eq!((roles(&s), kinds(&s), s["status"].as_str()), (vec!["system", "user"], vec!["user"], Some("running")));
    assert_eq!(f.mock.reqs().len(), 0, "taking a message calls no model");

    let (s, _) = advance(&f, &s, json!({})).await;
    assert_eq!(roles(&s), ["system", "user", "assistant", "tool"], "one model call and the tool call it made");
    assert_eq!(s["status"], "running", "the turn has work left");
    assert_eq!(f.mock.reqs().len(), 1);
    assert!(f.dir.join("workspace/made.txt").exists());

    let (s, tokens) = advance(&f, &s, json!({})).await;
    assert_eq!(roles(&s), ["system", "user", "assistant", "tool", "assistant"]);
    assert_eq!((s["status"].as_str(), tokens.as_str()), (Some("done"), "all done"), "the answer's deltas left as they arrived");
    assert_eq!(kinds(&s), ["user", "assistant", "tool_call", "tool_started", "tool_result", "assistant", "done"], "one log, in order, across four processes");
    assert_eq!(s["events"][6]["text"], "all done");
    assert_eq!(s["stats"]["model_calls"], 2, "usage adds up across steps");
    // The second call saw exactly what the first step left: the tool result, once.
    let second = &f.mock.reqs()[1];
    assert_eq!(roles(second), ["system", "user", "assistant", "tool"]);
}

#[tokio::test]
async fn a_step_given_seconds_runs_the_turn_until_they_are_up() {
    let f = fx(vec![bash("c1", "echo hi > made.txt"), say("all done")], Approve::Auto).await;

    // No time at all: the message is taken and shown, and no model call starts.
    let (s, _) = advance(&f, &json!({}), json!({"message": "go", "seconds": 0})).await;
    assert_eq!((kinds(&s), s["status"].as_str(), f.mock.reqs().len()), (vec!["user"], Some("running"), 0));
    assert_eq!(s["seen"], json!(["user"]), "the host saw the message as an event, not only in the result");

    // Time enough: the model call, its tool call and the call after it are one step.
    let (s, tokens) = advance(&f, &s, json!({"seconds": 60})).await;
    assert_eq!((s["status"].as_str(), tokens.as_str(), f.mock.reqs().len()), (Some("done"), "all done", 2));
    assert_eq!(roles(&s), ["system", "user", "assistant", "tool", "assistant"]);
    assert_eq!(json!(kinds(&s)[1..]), s["seen"], "every event was shown as it happened, in the order the result carries them");
    assert!(f.dir.join("workspace/made.txt").exists());
}

#[tokio::test]
async fn a_host_can_cancel_a_step_while_it_runs() {
    let f = fx(vec![bash("c1", "sleep 20"), say("never said")], Approve::Auto).await;
    let started = std::time::Instant::now();
    let (s, _) = advance(&f, &json!({}), json!({"message": "go", "seconds": 60, "cancel_after": 400})).await;
    assert!(started.elapsed().as_secs() < 10, "the step did not wait for the call to finish");
    assert_eq!((s["status"].as_str(), f.mock.reqs().len()), (Some("cancelled"), 1));
    assert_eq!(kinds(&s).last(), Some(&"cancelled"));
    assert_eq!(json!(kinds(&s)), s["seen"], "the events that ended the turn were shown too");
    let result = s["messages"].as_array().unwrap().last().unwrap();
    assert_eq!((result["role"].as_str(), result["tool_call_id"].as_str()), (Some("tool"), Some("c1")), "the call it stopped in has a result");

    // The next turn starts from a clean place.
    let (next, _) = advance(&f, &s, json!({"message": "again", "seconds": 60})).await;
    assert_eq!((next["status"].as_str(), f.mock.reqs().len()), (Some("done"), 2));
}

#[tokio::test]
async fn a_step_can_run_a_whole_turn_or_cancel_one() {
    let f = fx(vec![bash("c1", "true"), say("done")], Approve::Auto).await;
    let (s, _) = advance(&f, &json!({}), json!({"message": "go", "calls": 10})).await;
    assert_eq!((s["status"].as_str(), f.mock.reqs().len()), (Some("done"), 2), "a budget above what the turn needs finishes it");

    let (s, _) = advance(&f, &s, json!({"message": "again", "calls": 0})).await;
    let (s, _) = advance(&f, &s, json!({"cancel": true})).await;
    assert_eq!((s["status"].as_str(), kinds(&s).last().copied()), (Some("cancelled"), Some("cancelled")));
    assert_eq!(f.mock.reqs().len(), 2, "cancelling calls no model");
}

const ASK: &str = "approval = \"ask\"";

#[tokio::test]
async fn a_step_stops_at_an_approval_and_a_later_one_brings_the_answer() {
    let f = fx(vec![bash("c1", "echo hi > made.txt"), say("done")], Approve::Auto).await;
    let (s, _) = advance_as(&f, ASK, &json!({}), json!({"message": "go", "calls": 0})).await;

    let (s, _) = advance_as(&f, ASK, &s, json!({})).await;
    assert_eq!(s["status"], "waiting", "the turn waits on its host");
    assert_eq!(kinds(&s), ["user", "assistant", "tool_call", "approval_request"]);
    assert_eq!((s["events"][3]["call_id"].as_str(), s["events"][3]["tool"].as_str()), (Some("c1"), Some("bash")));
    assert_eq!(roles(&s), ["system", "user", "assistant"], "the call has no result yet");
    assert!(!f.dir.join("workspace/made.txt").exists(), "and has not run");

    // Run again with no answer, the session waits where it was and asks nothing twice.
    let (again, _) = advance_as(&f, ASK, &s, json!({})).await;
    assert_eq!((again["status"].as_str(), kinds(&again).len()), (Some("waiting"), 4));
    assert_eq!(f.mock.reqs().len(), 1);

    let (s, _) = advance_as(&f, ASK, &s, json!({"answers": [{"call_id": "c1", "allow": true}]})).await;
    assert_eq!(s["status"], "done");
    assert_eq!(
        kinds(&s),
        ["user", "assistant", "tool_call", "approval_request", "approval", "tool_started", "tool_result", "assistant", "done"],
        "the call is announced once"
    );
    assert_eq!(s["events"][4], json!({"type": "approval", "call_id": "c1", "tool": "bash", "allow": true, "at": s["events"][4]["at"]}));
    assert!(f.dir.join("workspace/made.txt").exists(), "the approved call ran");
    assert_eq!(roles(&s), ["system", "user", "assistant", "tool", "assistant"]);
}

#[tokio::test]
async fn a_denied_call_and_a_cancelled_wait_both_leave_the_call_with_a_result() {
    let f = fx(vec![bash("c1", "touch made.txt"), say("understood"), say("moving on")], Approve::Auto).await;
    let (s, _) = advance_as(&f, ASK, &json!({}), json!({"message": "go", "calls": 10})).await;
    assert_eq!(s["status"], "waiting", "a budget does not carry a turn past a request");

    let (denied, _) = advance_as(&f, ASK, &s, json!({"answers": [{"call_id": "c1", "allow": false}], "calls": 10})).await;
    assert_eq!(denied["status"], "done");
    assert_eq!(denied["messages"][3]["content"], "error: the user denied this tool call");

    // Cancelling instead turns the request down and gives the call its result there and then, so
    // the next turn does not stop at it again and the result sits where the model expects it.
    let (cancelled, _) = advance_as(&f, ASK, &s, json!({"cancel": true})).await;
    assert_eq!(kinds(&cancelled)[4..], ["approval", "tool_result", "cancelled"]);
    assert_eq!(cancelled["events"][4]["allow"], false);
    let (next, _) = advance_as(&f, ASK, &cancelled, json!({"message": "never mind", "calls": 10})).await;
    assert_eq!(next["status"], "done");
    assert_eq!(roles(&next), ["system", "user", "assistant", "tool", "user", "assistant"]);
    assert!(!f.dir.join("workspace/made.txt").exists(), "the call never ran");
}

#[tokio::test]
async fn a_step_stops_at_a_question_and_takes_its_answer() {
    let ask = call(&[("q1", "ask_user", json!({"question": "Which colour?", "options": ["red", "blue"]}))]);
    let f = fx(vec![ask, say("blue it is")], Approve::Auto).await;
    let (s, _) = advance(&f, &json!({}), json!({"message": "paint it", "calls": 10})).await;
    assert_eq!((s["status"].as_str(), kinds(&s).last().copied()), (Some("waiting"), Some("question")));
    assert_eq!(s["events"].as_array().unwrap().last().unwrap()["question"], "Which colour?");

    let (s, _) = advance(&f, &s, json!({"answers": [{"call_id": "q1", "answer": "blue"}], "calls": 10})).await;
    assert_eq!(s["status"], "done");
    assert_eq!(s["messages"][3]["content"], "blue", "the answer is the call's result");
}
