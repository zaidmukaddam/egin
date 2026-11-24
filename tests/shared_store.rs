//! Several egin processes on one session store (`EGIN_STORE_URL`): a session belongs to the store, not
//! to the process that started it. This is what a serverless host needs, where the instance that
//! takes the next request is rarely the one that took the last.

mod common;

use common::{bash, fx_on, libsql, say};
use egin::{agent::run, store::Store, Approve};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

/// The event types a stream sends up to and including `until`.
async fn kinds_until(url: String, until: &str) -> Vec<String> {
    let mut stream = reqwest::get(url).await.unwrap().bytes_stream();
    let (mut buf, mut kinds) = (String::new(), vec![]);
    while let Some(chunk) = stream.next().await {
        buf += &String::from_utf8_lossy(&chunk.unwrap());
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) else { continue };
            let kind = serde_json::from_str::<Value>(data).unwrap()["type"].as_str().unwrap().to_string();
            kinds.push(kind.clone());
            if kind == until {
                return kinds;
            }
        }
    }
    kinds
}

async fn status_becomes(store: &Store, sid: &str, want: &str) -> String {
    for _ in 0..200 {
        if store.status(sid).unwrap().as_deref() == Some(want) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    store.status(sid).unwrap().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_remote_store_answers_like_the_file() {
    let (url, _) = libsql().await;
    let path = std::env::temp_dir().join(format!("egin-parity-{}.db", egin::new_id()));
    let seen: Vec<String> = [Store::file(&path).unwrap(), Store::remote(&url, "").unwrap()]
        .iter()
        .map(|s| {
            let mut log = vec![];
            let mut say = |what: &str, v: String| log.push(format!("{what}: {v}"));
            s.create_session("s", "agent").unwrap();
            s.create_session("t", "agent").unwrap();
            s.set_owner("s", "alice").unwrap();
            s.set_parent("t", "s").unwrap();
            say("owner", format!("{:?} {:?} {:?}", s.owner("s").unwrap(), s.owner("t").unwrap(), s.owner("none").unwrap()));
            say("children", format!("{:?} {:?}", s.children("s").unwrap(), s.parent("t").unwrap()));
            for text in ["one", "two", "three"] {
                s.push_message("s", &json!({"role": "user", "content": text})).unwrap();
            }
            s.update_message("s", 2, &json!({"role": "user", "content": "3"})).unwrap();
            s.replace_range("s", 0, 2, &json!({"role": "user", "content": "summary"})).unwrap();
            say("messages", format!("{:?}", s.messages("s").unwrap()));
            let ids: Vec<i64> = ["user", "approval", "file_seen", "user"]
                .iter()
                .map(|kind| {
                    s.push_event(
                        "s",
                        &json!({"type": kind, "text": "hi \"there\"", "tool": "bash", "allow": true, "call_id": "c1", "path": "/a", "hash": "h1"}),
                    )
                    .unwrap()
                })
                .collect();
            say("event ids", format!("{ids:?}"));
            say("after", format!("{:?}", s.events_after("s", ids[1]).unwrap().iter().map(|e| e.0).collect::<Vec<_>>()));
            say("page", format!("{:?}", s.events_page("s", Some(ids[3]), 1).unwrap().iter().map(|e| e.0).collect::<Vec<_>>()));
            say("of", s.events_of("s", "user").unwrap().len().to_string());
            say("found", format!("{:?}", s.find_event("s", "approval", "c1").unwrap().map(|e| e["tool"].clone())));
            say("approved", format!("{} {}", s.tool_approved("s", "bash").unwrap(), s.tool_approved("s", "edit").unwrap()));
            say("file", format!("{:?} {:?}", s.file_seen("s", "/a").unwrap(), s.file_seen("s", "/b").unwrap()));
            for n in 0..3 {
                s.enqueue("s", &json!({"n": n})).unwrap();
            }
            say("inbox", format!("{:?} {:?}", s.take_inbox("s", 2).unwrap(), s.take_inbox("s", 2).unwrap()));
            say(
                "thread",
                format!(
                    "{} {} {:?} {:?}",
                    s.claim_thread("k", "s").unwrap(),
                    s.claim_thread("k", "t").unwrap(),
                    s.thread("k").unwrap(),
                    s.thread_of("s").unwrap()
                ),
            );
            s.put_stats("s", &egin::store::Stats { model_calls: 3, cost: 0.5, ..Default::default() }).unwrap();
            say("stats", format!("{:?}", s.stats("s").unwrap()));
            s.set_settings("s", &toml::from_str("model = \"m\"").unwrap()).unwrap();
            say("settings", format!("{:?} {:?}", s.settings("s").unwrap(), s.settings("t").unwrap()));
            say("lease", format!("{} {} {}", s.start_run("s", "A").unwrap(), s.start_run("s", "B").unwrap(), s.take_over("s", "B", 30).unwrap()));
            say("taken", format!("{} {} {}", s.take_over("s", "B", 0).unwrap(), s.renew_lease("s", "A").unwrap(), s.renew_lease("s", "B").unwrap()));
            say("running", format!("{:?} {:?} {:?}", s.with_status("running").unwrap(), s.status("s").unwrap(), s.lease_age("s").unwrap().map(|a| a < 5)));
            say(
                "list",
                format!(
                    "{:?}",
                    s.list_sessions(Some("alice"), 10).unwrap().iter().map(|v| (v["id"].clone(), v["status"].clone(), v["title"].clone())).collect::<Vec<_>>()
                ),
            );
            s.delete_session("s").unwrap();
            say("deleted", format!("{:?} {}", s.status("s").unwrap(), s.messages("s").unwrap().len()));
            say("missing", s.stats("s").is_err().to_string());
            log.join("\n")
        })
        .collect();
    assert_eq!(seen[0], seen[1]);
    assert!(
        seen[0].contains("messages: [(0, Object {\"content\": String(\"summary\")")
            && seen[0].contains("inbox: [Object {\"n\": Number(0)}, Object {\"n\": Number(1)}] [Object {\"n\": Number(2)}]"),
        "{}",
        seen[0]
    );
    assert!(seen[0].contains("lease: true false false\ntaken: true false true"), "{}", seen[0]);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_started_on_one_instance_is_streamed_and_shown_by_another() {
    let (store, _) = libsql().await;
    let a = fx_on(&store, vec![], Approve::Auto).await;
    let b = fx_on(&store, vec![say("answered by b")], Approve::Auto).await;
    let (at_a, at_b) = (a.serve(a.agent("")).await, b.serve(b.agent("")).await);
    let http = reqwest::Client::new();

    let created: Value = http.post(format!("{at_a}/sessions")).send().await.unwrap().json().await.unwrap();
    let sid = created["id"].as_str().unwrap();
    // The stream is open on A before anything runs, as a chat page's is.
    let stream = tokio::spawn(kinds_until(format!("{at_a}/sessions/{sid}/events"), "done"));
    let sent = http.post(format!("{at_b}/sessions/{sid}/messages")).json(&json!({"message": "go"})).send().await.unwrap();
    assert!(sent.status().is_success(), "B knows the session A created");

    let kinds = tokio::time::timeout(Duration::from_secs(20), stream).await.expect("A's stream ends with the run B did").unwrap();
    assert_eq!(kinds, ["user", "assistant", "done"], "the stored events, in order, without B's token deltas");
    let shown: Value = http.get(format!("{at_a}/sessions/{sid}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(shown["status"], "done");
    assert_eq!(shown["messages"].as_array().unwrap().last().unwrap()["content"], "answered by b");
    assert_eq!((a.mock.reqs().len(), b.mock.reqs().len()), (0, 1), "only B ran it");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_instance_died_is_finished_by_the_one_that_is_asked_next() {
    let (store, db) = libsql().await;
    let a = fx_on(&store, vec![], Approve::Auto).await;
    let b = fx_on(&store, vec![say("finished by b")], Approve::Auto).await;
    // A took a message and started the run, then its instance was recycled mid-call.
    let sid = run::new_session(&a.app, &a.agent("")).unwrap();
    run::send(&a.app, &sid, "go").unwrap();
    assert!(a.app.store.start_run(&sid, &a.app.runner).unwrap());
    let at_b = b.serve(b.agent("")).await;
    let http = reqwest::Client::new();

    // While A's lease is fresh, B leaves the run alone.
    let shown: Value = http.get(format!("{at_b}/sessions/{sid}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(shown["status"], "running");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(b.mock.reqs().len(), 0, "a session another instance is running is not run twice");

    // A has not renewed it for a minute: the next request about the session continues it on B.
    db.lock().unwrap().execute("UPDATE sessions SET lease = lease - 60", []).unwrap();
    let kinds = tokio::time::timeout(Duration::from_secs(20), kinds_until(format!("{at_b}/sessions/{sid}/events"), "done")).await.expect("B finishes the run");
    assert_eq!(kinds, ["user", "token", "token", "assistant", "done"], "B runs it now, so its stream carries the token deltas too");
    assert_eq!(status_becomes(&b.app.store, &sid, "done").await, "done");
    assert!(!a.app.store.renew_lease(&sid, &a.app.runner).unwrap(), "A, should it wake up, finds the session is no longer its own");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cron_request_resumes_runs_nobody_is_watching() {
    let (store, db) = libsql().await;
    let a = fx_on(&store, vec![], Approve::Auto).await;
    let b = fx_on(&store, vec![say("finished by b")], Approve::Auto).await;
    let sid = run::new_session(&a.app, &a.agent("")).unwrap();
    run::send(&a.app, &sid, "go").unwrap();
    a.app.store.claim(&sid, &a.app.runner).unwrap();
    let at_b = b.serve(b.agent("")).await;
    let http = reqwest::Client::new();

    let fresh: Value = http.post(format!("{at_b}/resume")).send().await.unwrap().json().await.unwrap();
    assert_eq!(fresh["resumed"], 0, "a run with a live runner is left to it");
    db.lock().unwrap().execute("UPDATE sessions SET lease = lease - 60", []).unwrap();
    let stale: Value = http.post(format!("{at_b}/resume")).send().await.unwrap().json().await.unwrap();
    assert_eq!(stale["resumed"], 1);
    assert_eq!(status_becomes(&b.app.store, &sid, "done").await, "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_on_one_instance_stops_the_run_on_another() {
    let (store, _) = libsql().await;
    // A's model starts a command that outlasts a lease renewal, then would answer.
    let a = fx_on(&store, vec![bash("c1", "sleep 12"), say("too late")], Approve::Auto).await;
    let b = fx_on(&store, vec![], Approve::Auto).await;
    let (at_a, at_b) = (a.serve(a.agent("")).await, b.serve(b.agent("")).await);
    let http = reqwest::Client::new();
    let created: Value = http.post(format!("{at_a}/sessions")).json(&json!({"message": "go"})).send().await.unwrap().json().await.unwrap();
    let sid = created["id"].as_str().unwrap();
    for _ in 0..100 {
        if a.mock.reqs().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cancelled = http.post(format!("{at_b}/sessions/{sid}/cancel")).send().await.unwrap();
    assert!(cancelled.status().is_success(), "B can cancel what A is running");
    // A's next lease renewal (every 10 s) finds the session cancelled and stops, before the command
    // would have returned and the model been called again.
    tokio::time::sleep(Duration::from_secs(14)).await;
    assert_eq!(a.mock.reqs().len(), 1, "A stopped instead of going on to its next model call");
    assert_eq!(b.app.store.status(sid).unwrap().as_deref(), Some("cancelled"), "and left the status as B set it");
    let last = b.app.store.events_after(sid, 0).unwrap().pop().unwrap().1;
    assert_eq!(last["type"], "cancelled");
}
