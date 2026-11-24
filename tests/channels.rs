//! The HTTP API and channels: sessions over SSE with approvals, the signed webhook, and Slack
//! (Events API and Socket Mode).

mod common;

use axum::{routing::post, Json, Router};
use common::{bash, fx, hmac_hex, say};
use egin::{server, Approve};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// A fake Slack Web API that records chat.postMessage calls; returns (base URL, posted bodies).
async fn fake_slack(socket_url: Option<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let posted = Arc::new(Mutex::new(vec![]));
    let sink = posted.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let mut router = Router::new().route(
        "/chat.postMessage",
        post(move |Json(b): Json<Value>| async move {
            sink.lock().unwrap().push(b);
            Json(json!({"ok": true}))
        }),
    );
    if let Some(url) = socket_url {
        router = router.route("/apps.connections.open", post(move || async move { Json(json!({"ok": true, "url": url})) }));
    }
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (api, posted)
}

async fn wait_for(posted: &Arc<Mutex<Vec<Value>>>) {
    wait_n(posted, 1).await
}

async fn wait_n(posted: &Arc<Mutex<Vec<Value>>>, n: usize) {
    for _ in 0..100 {
        if posted.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A fake platform API that answers every request with `{"ok": true}` and records
/// `{method, path, body}` for each.
async fn fake_api() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(vec![]));
    let sink = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(move |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| {
        let sink = sink.clone();
        async move {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            sink.lock().unwrap().push(json!({"method": method.as_str(), "path": uri.path(), "body": body}));
            Json(json!({"ok": true}))
        }
    });
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (api, seen)
}

#[tokio::test]
async fn http_api_streams_events_and_takes_approvals() {
    let f = fx(vec![bash("c1", "touch approved.txt"), say("all done")], Approve::Api).await;
    let a = f.agent("approval = \"ask\"");
    let base = f.serve(a.clone()).await;

    let http = reqwest::Client::new();
    let created: Value = http.post(format!("{base}/sessions")).json(&json!({"message": "go"})).send().await.unwrap().json().await.unwrap();
    let sid = created["id"].as_str().unwrap();

    let mut stream = http.get(format!("{base}/sessions/{sid}/events")).send().await.unwrap().bytes_stream();
    let (mut buf, mut kinds, mut tokens) = (String::new(), vec![], String::new());
    'read: while let Some(chunk) = stream.next().await {
        buf += &String::from_utf8_lossy(&chunk.unwrap());
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) else { continue };
            let e: Value = serde_json::from_str(data).unwrap();
            let kind = e["type"].as_str().unwrap().to_string();
            match kind.as_str() {
                "token" => tokens += e["text"].as_str().unwrap(),
                "approval_request" => {
                    let r = http.post(format!("{base}/sessions/{sid}/approvals")).json(&json!({"call_id": e["call_id"], "allow": true})).send().await.unwrap();
                    assert!(r.status().is_success());
                }
                _ => {}
            }
            if kind != "token" {
                kinds.push(kind.clone());
            }
            if kind == "done" {
                break 'read;
            }
        }
    }
    assert_eq!(kinds, ["user", "assistant", "tool_call", "approval_request", "approval", "tool_started", "tool_result", "assistant", "done"]);
    assert_eq!(tokens, "all done");
    assert!(a.workspace.join("approved.txt").exists());

    let shown: Value = http.get(format!("{base}/sessions/{sid}")).send().await.unwrap().json().await.unwrap();
    assert_eq!((shown["status"].as_str(), shown["stats"]["model_calls"].as_u64()), (Some("done"), Some(2)));
    // A late subscriber replays the stored log from any event id.
    let replay = http.get(format!("{base}/sessions/{sid}/events?after=0")).send().await.unwrap().bytes_stream().next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&replay).contains("\"type\":\"user\""));
    assert_eq!(http.post(format!("{base}/sessions/nope/messages")).json(&json!({"message": "x"})).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn a_session_can_pick_an_offered_model_and_reasoning_effort() {
    let f = fx(vec![say("one"), say("two")], Approve::Auto).await;
    f.write("agent.toml", "model = \"mock\"\nmodels = [\"mock-big\"]\napproval = \"never\"\nsandbox = \"local\"\n", false);
    let base = f.serve_with(f.agent(""), None).await;
    let http = reqwest::Client::new();

    let info: Value = http.get(format!("{base}/agent")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["models"], json!(["mock", "mock-big"]));

    let refused = http.post(format!("{base}/sessions")).json(&json!({"message": "hi", "model": "pricey"})).send().await.unwrap();
    assert_eq!(refused.status(), 400, "a model the agent does not offer is refused");

    let done = |sid: String| {
        let http = http.clone();
        let base = base.clone();
        async move {
            for _ in 0..200 {
                let s: Value = http.get(format!("{base}/sessions/{sid}")).send().await.unwrap().json().await.unwrap();
                if s["status"] == "done" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("session did not finish");
        }
    };
    let created: Value = http
        .post(format!("{base}/sessions"))
        .json(&json!({"message": "hi", "model": "mock-big", "reasoning_effort": "high"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = created["id"].as_str().unwrap().to_string();
    done(sid.clone()).await;
    let first = f.mock.reqs().last().unwrap().clone();
    assert_eq!((first["model"].as_str(), first["reasoning_effort"].as_str()), (Some("mock-big"), Some("high")));

    // The choice stays with the session for later turns, and a message can change it.
    http.post(format!("{base}/sessions/{sid}/messages")).json(&json!({"message": "again", "model": "mock"})).send().await.unwrap();
    done(sid).await;
    let second = f.mock.reqs().last().unwrap().clone();
    assert_eq!((second["model"].as_str(), second["reasoning_effort"].as_str()), (Some("mock"), Some("high")));
}

#[tokio::test]
async fn token_guards_session_routes_but_not_channels() {
    let f = fx(vec![say("hello")], Approve::Auto).await;
    std::env::set_var("EGIN_TEST_TOKEN_HOOK", "s3cret");
    f.write("channels/webhook.toml", "secret = \"${EGIN_TEST_TOKEN_HOOK}\"\n", false);
    let base = f.serve_with(f.agent(""), Some("t0ken")).await;
    let http = reqwest::Client::new();
    let create = |auth: Option<&str>| {
        let mut r = http.post(format!("{base}/sessions")).json(&json!({}));
        if let Some(a) = auth {
            r = r.header("authorization", a);
        }
        r.send()
    };
    assert_eq!(create(None).await.unwrap().status(), 401);
    assert_eq!(create(Some("Bearer wrong")).await.unwrap().status(), 401);
    assert_eq!(create(Some("Bearer t0ken")).await.unwrap().status(), 200);
    assert_eq!(http.get(format!("{base}/sessions/x/events")).send().await.unwrap().status(), 401);
    // A host's health check has no token to send.
    assert_eq!(http.get(format!("{base}/health")).send().await.unwrap().text().await.unwrap(), "ok");
    // The webhook has its own HMAC check and needs no bearer token.
    let body = json!({"text": "hi"});
    let sig = format!("sha256={}", hmac_hex("s3cret", body.to_string().as_bytes()));
    let r = http.post(format!("{base}/channels/webhook")).header("x-egin-signature", sig).body(body.to_string()).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn users_see_only_their_own_sessions() {
    let f = fx(vec![say("hi alice")], Approve::Auto).await;
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let as_user = |method: reqwest::Method, path: &str, user: Option<&str>| {
        let mut r = http.request(method, format!("{base}{path}"));
        if let Some(u) = user {
            r = r.header("x-egin-user", u);
        }
        r
    };
    let created: Value =
        as_user(reqwest::Method::POST, "/sessions", Some("alice")).json(&json!({"message": "plan my week"})).send().await.unwrap().json().await.unwrap();
    let sid = created["id"].as_str().unwrap();

    assert_eq!(as_user(reqwest::Method::GET, &format!("/sessions/{sid}"), Some("alice")).send().await.unwrap().status(), 200);
    for path in [format!("/sessions/{sid}"), format!("/sessions/{sid}/events"), format!("/sessions/{sid}/cancel")] {
        let method = if path.ends_with("cancel") { reqwest::Method::POST } else { reqwest::Method::GET };
        assert_eq!(as_user(method, &path, Some("bob")).send().await.unwrap().status(), 404, "{path}");
    }
    assert_eq!(as_user(reqwest::Method::GET, &format!("/sessions/{sid}"), None).send().await.unwrap().status(), 200, "the operator sees everything");

    let list = |user| async move { as_user(reqwest::Method::GET, "/sessions", user).send().await.unwrap().json::<Value>().await.unwrap()["sessions"].clone() };
    let mine = list(Some("alice")).await;
    assert_eq!((mine[0]["id"].as_str(), mine[0]["title"].as_str()), (Some(sid), Some("plan my week")));
    assert_eq!(list(Some("bob")).await.as_array().unwrap().len(), 0);

    // An approval names its session: a call id from elsewhere is not found.
    let r = as_user(reqwest::Method::POST, &format!("/sessions/{sid}/approvals"), Some("alice"))
        .json(&json!({"call_id": "not-here", "allow": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn uploads_land_in_the_workspace_and_reach_the_model() {
    let f = fx(vec![say("got it")], Approve::Auto).await;
    let a = f.agent("");
    let base = f.serve(a.clone()).await;
    let http = reqwest::Client::new();
    let sid = http.post(format!("{base}/sessions")).json(&json!({})).send().await.unwrap().json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();

    let up = |name: &str, body: &'static str| http.post(format!("{base}/sessions/{sid}/files?name={name}")).body(body).send();
    let first: Value = up("../../notes.txt", "hello").await.unwrap().json().await.unwrap();
    assert_eq!(first["name"], "notes.txt", "path components are dropped");
    let second: Value = up("notes.txt", "again").await.unwrap().json().await.unwrap();
    assert_eq!(second["name"], "2-notes.txt", "an earlier upload is never overwritten");
    let on_disk = a.workspace.join("uploads").join(&sid).join("notes.txt");
    assert_eq!(std::fs::read_to_string(on_disk).unwrap(), "hello");

    let bad = http.post(format!("{base}/sessions/{sid}/messages")).json(&json!({"message": "x", "files": [{"path": "/etc/passwd"}]})).send().await.unwrap();
    assert_eq!(bad.status(), 400);
    let r = http.post(format!("{base}/sessions/{sid}/messages")).json(&json!({"message": "read it", "files": [first]})).send().await.unwrap();
    assert!(r.status().is_success());
    for _ in 0..100 {
        if !f.mock.reqs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sent = f.mock.reqs()[0]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(sent.starts_with("read it\n\nAttached files:\n- ") && sent.ends_with("/notes.txt"), "{sent}");
    let user = f.app.store.events_after(&sid, 0).unwrap().into_iter().find(|(_, e)| e["type"] == "user").unwrap().1;
    assert_eq!((user["text"].as_str(), user["files"][0]["name"].as_str()), (Some("read it"), Some("notes.txt")));
}

#[tokio::test]
async fn webhook_channel_verifies_and_keeps_threads() {
    let f = fx(vec![say("first answer"), say("second answer")], Approve::Auto).await;
    std::env::set_var("EGIN_TEST_HOOK_SECRET", "s3cret");
    f.write("channels/webhook.toml", "secret = \"${EGIN_TEST_HOOK_SECRET}\"\n", false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let post = |body: Value, sig: String| http.post(format!("{base}/channels/webhook")).header("x-egin-signature", sig).body(body.to_string()).send();
    let signed = |body: &Value| format!("sha256={}", hmac_hex("s3cret", body.to_string().as_bytes()));

    let body = json!({"text": "hi", "thread": "t1"});
    assert_eq!(post(body.clone(), "sha256=00".into()).await.unwrap().status(), 401);
    let r: Value = post(body.clone(), signed(&body)).await.unwrap().json().await.unwrap();
    assert_eq!(r["text"], "first answer");
    let body2 = json!({"text": "again", "thread": "t1"});
    let r2: Value = post(body2.clone(), signed(&body2)).await.unwrap().json().await.unwrap();
    assert_eq!((r2["text"].as_str(), &r2["session"]), (Some("second answer"), &r["session"]), "same thread, same session");
    assert_eq!(f.mock.reqs()[1]["messages"].as_array().unwrap().len(), 4, "the second turn sees the first");
}

#[tokio::test]
async fn slack_channel_verifies_dedupes_and_replies_in_thread() {
    let (slack_api, posted) = fake_slack(None).await;
    let f = fx(vec![say("pong")], Approve::Auto).await;
    f.write("channels/slack.toml", &format!("signing_secret = \"sig\"\nbot_token = \"xoxb-test\"\napi_url = \"{slack_api}\"\n"), false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let send = |body: Value| {
        let ts = chrono::Utc::now().timestamp().to_string();
        let sig = format!("v0={}", hmac_hex("sig", format!("v0:{ts}:{body}").as_bytes()));
        http.post(format!("{base}/channels/slack")).header("x-slack-request-timestamp", ts).header("x-slack-signature", sig).body(body.to_string()).send()
    };
    let challenge: Value = send(json!({"type": "url_verification", "challenge": "abc"})).await.unwrap().json().await.unwrap();
    assert_eq!(challenge["challenge"], "abc");
    let event = json!({"type": "event_callback", "event_id": "Ev1", "event": {"type": "app_mention", "channel": "C1", "ts": "111.1", "text": "<@U9> ping"}});
    assert!(send(event.clone()).await.unwrap().status().is_success());
    assert!(send(event).await.unwrap().status().is_success(), "a retried delivery is acknowledged but not run twice");
    let bad = http
        .post(format!("{base}/channels/slack"))
        .header("x-slack-request-timestamp", "1")
        .header("x-slack-signature", "v0=00")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    wait_for(&posted).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*posted.lock().unwrap(), [json!({"channel": "C1", "thread_ts": "111.1", "text": "pong"})]);
    assert_eq!(f.mock.reqs()[0]["messages"][1]["content"], "ping", "the mention is stripped");
}

#[tokio::test]
async fn telegram_channel_checks_the_secret_and_keeps_one_session_per_chat() {
    let (api, posted) = fake_api().await;
    let f = fx(vec![say("one"), say("two"), say("three")], Approve::Auto).await;
    f.write("channels/telegram.toml", &format!("bot_token = \"123:abc\"\nsecret_token = \"tg-secret\"\nusername = \"egin_bot\"\napi_url = \"{api}\"\n"), false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let send =
        |secret: &str, update: Value| http.post(format!("{base}/channels/telegram")).header("x-telegram-bot-api-secret-token", secret).json(&update).send();
    let dm = |id: i64, text: &str| json!({"update_id": id, "message": {"message_id": id, "chat": {"id": 5, "type": "private"}, "from": {"id": 1, "is_bot": false}, "text": text}});

    assert_eq!(send("wrong", dm(1, "hi")).await.unwrap().status(), 401);
    assert!(send("tg-secret", dm(1, "hi")).await.unwrap().status().is_success());
    wait_n(&posted, 1).await;
    assert!(send("tg-secret", dm(1, "hi")).await.unwrap().status().is_success(), "a redelivered update is acknowledged, not run again");
    assert!(send("tg-secret", dm(2, "again")).await.unwrap().status().is_success());
    wait_n(&posted, 2).await;
    assert_eq!(f.mock.reqs()[1]["messages"].as_array().unwrap().len(), 4, "the second message in the chat sees the first");

    // In a group only a mention (or a reply to the bot) is answered, as a reply to that message.
    let group = |id: i64, text: &str| json!({"update_id": id, "message": {"message_id": 70 + id, "chat": {"id": -9, "type": "supergroup"}, "from": {"id": 2, "is_bot": false}, "text": text}});
    assert!(send("tg-secret", group(3, "just chatting")).await.unwrap().status().is_success());
    assert!(send("tg-secret", group(4, "@egin_bot hello")).await.unwrap().status().is_success());
    wait_n(&posted, 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let path = "/bot123:abc/sendMessage";
    assert_eq!(
        *posted.lock().unwrap(),
        [
            json!({"method": "POST", "path": path, "body": {"chat_id": 5, "text": "one"}}),
            json!({"method": "POST", "path": path, "body": {"chat_id": 5, "text": "two"}}),
            json!({"method": "POST", "path": path, "body": {"chat_id": -9, "text": "three", "reply_parameters": {"message_id": 74}}}),
        ]
    );
    assert_eq!(f.mock.reqs()[2]["messages"][1]["content"], "hello", "the mention is stripped and the group has its own session");
    assert_ne!(f.app.store.thread("telegram:5").unwrap(), f.app.store.thread("telegram:-9").unwrap());
}

#[tokio::test]
async fn discord_channel_verifies_defers_and_edits_the_answer_in() {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let (api, posted) = fake_api().await;
    let long = format!("{}\n{}", "a".repeat(1500), "b".repeat(1000));
    let f = fx(vec![say(&long), say("short")], Approve::Auto).await;
    f.write("channels/discord.toml", &format!("public_key = \"{}\"\napi_url = \"{api}\"\n", hex::encode(key.public_key().as_ref())), false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let send = |body: Value, signer: &Ed25519KeyPair| {
        let ts = "1700000000";
        let sig = hex::encode(signer.sign(format!("{ts}{body}").as_bytes()).as_ref());
        http.post(format!("{base}/channels/discord")).header("x-signature-timestamp", ts).header("x-signature-ed25519", sig).body(body.to_string()).send()
    };

    let other = Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap().as_ref()).unwrap();
    assert_eq!(send(json!({"type": 1}), &other).await.unwrap().status(), 401, "a body signed by another key is refused");
    let pong: Value = send(json!({"type": 1}), &key).await.unwrap().json().await.unwrap();
    assert_eq!(pong, json!({"type": 1}));

    let ask = |token: &str, prompt: &str| json!({"type": 2, "application_id": "A1", "token": token, "channel_id": "C7", "data": {"name": "ask", "options": [{"name": "prompt", "type": 3, "value": prompt}]}});
    let deferred: Value = send(ask("tok1", "hello"), &key).await.unwrap().json().await.unwrap();
    assert_eq!(deferred, json!({"type": 5}));
    wait_n(&posted, 2).await;
    send(ask("tok2", "more"), &key).await.unwrap();
    wait_n(&posted, 3).await;

    assert_eq!(
        *posted.lock().unwrap(),
        [
            json!({"method": "PATCH", "path": "/webhooks/A1/tok1/messages/@original", "body": {"content": "a".repeat(1500)}}),
            json!({"method": "POST", "path": "/webhooks/A1/tok1", "body": {"content": "b".repeat(1000)}}),
            json!({"method": "PATCH", "path": "/webhooks/A1/tok2/messages/@original", "body": {"content": "short"}}),
        ],
        "an answer over 2000 characters continues in a followup"
    );
    assert_eq!(f.mock.reqs()[1]["messages"].as_array().unwrap().len(), 4, "one session per channel");
}

#[tokio::test]
async fn github_channel_verifies_ignores_bots_and_answers_mentions_in_place() {
    let (api, posted) = fake_api().await;
    let f = fx(vec![say("first"), say("second"), say("third")], Approve::Auto).await;
    f.write("channels/github.toml", &format!("secret = \"gh-secret\"\ntoken = \"ghp_test\"\nusername = \"egin-bot\"\napi_url = \"{api}\"\n"), false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let n = std::sync::atomic::AtomicUsize::new(0);
    let send = |event: &str, body: Value, secret: &str| {
        let sig = format!("sha256={}", hmac_hex(secret, body.to_string().as_bytes()));
        let delivery = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string();
        http.post(format!("{base}/channels/github"))
            .header("x-github-event", event)
            .header("x-github-delivery", delivery)
            .header("x-hub-signature-256", sig)
            .body(body.to_string())
            .send()
    };
    let comment = |login: &str, kind: &str, text: &str| {
        json!({"action": "created", "repository": {"full_name": "o/r"}, "issue": {"number": 3},
            "comment": {"id": 1, "body": text, "user": {"login": login, "type": kind}}, "sender": {"login": login, "type": kind}})
    };

    assert_eq!(send("issue_comment", comment("alice", "User", "@egin-bot hi"), "wrong").await.unwrap().status(), 401);
    send("ping", json!({"zen": "hi"}), "gh-secret").await.unwrap();
    send("issue_comment", comment("alice", "User", "no mention here"), "gh-secret").await.unwrap();
    send("issue_comment", comment("dependabot[bot]", "Bot", "@egin-bot rebase"), "gh-secret").await.unwrap();
    send("issue_comment", comment("alice", "User", "@egin-bot why does CI fail?"), "gh-secret").await.unwrap();
    wait_n(&posted, 1).await;
    let review = json!({"action": "created", "repository": {"full_name": "o/r"}, "pull_request": {"number": 4},
        "comment": {"id": 99, "body": "@Egin-Bot is this safe?", "user": {"login": "bob", "type": "User"}}, "sender": {"login": "bob", "type": "User"}});
    send("pull_request_review_comment", review, "gh-secret").await.unwrap();
    wait_n(&posted, 2).await;
    send("issue_comment", comment("alice", "User", "@egin-bot and now?"), "gh-secret").await.unwrap();
    wait_n(&posted, 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        *posted.lock().unwrap(),
        [
            json!({"method": "POST", "path": "/repos/o/r/issues/3/comments", "body": {"body": "first"}}),
            json!({"method": "POST", "path": "/repos/o/r/pulls/4/comments/99/replies", "body": {"body": "second"}}),
            json!({"method": "POST", "path": "/repos/o/r/issues/3/comments", "body": {"body": "third"}}),
        ]
    );
    let reqs = f.mock.reqs();
    assert_eq!(reqs[0]["messages"][1]["content"], "why does CI fail?");
    assert_eq!(reqs[1]["messages"].as_array().unwrap().len(), 2, "the pull request has its own session");
    assert_eq!(reqs[2]["messages"].as_array().unwrap().len(), 4, "the issue's second mention continues its session");
}

#[tokio::test]
async fn a_described_channel_verifies_filters_dedupes_and_replies_from_its_template() {
    let (api, posted) = fake_api().await;
    let f = fx(vec![say("first \"quoted\" answer"), say("second")], Approve::Auto).await;
    std::env::set_var("EGIN_TEST_DESCRIBED_SECRET", "shh");
    f.write(
        "channels/tracker.toml",
        &format!(
            "[verify]\nheader = \"X-Sig\"\nsecret = \"${{EGIN_TEST_DESCRIBED_SECRET}}\"\nprefix = \"sha256=\"\n\n\
             [message]\ntext = \"/data/body\"\nthread = \"/data/issueId\"\nid_header = \"X-Delivery\"\n\
             only = {{ \"/type\" = \"Comment\" }}\nskip = {{ \"/actor/id\" = \"bot\" }}\nmention = \"@agent\"\n\n\
             [reply]\nurl = \"{api}/issues/{{{{thread}}}}/comments\"\nheaders = {{ Authorization = \"key-1\" }}\nbody = '''{{\"body\": {{{{answer}}}}, \"issue\": {{{{thread}}}}}}'''\n"
        ),
        false,
    );
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let n = std::sync::atomic::AtomicUsize::new(0);
    let send = |body: Value, secret: &str| {
        let sig = format!("sha256={}", hmac_hex(secret, body.to_string().as_bytes()));
        let delivery = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string();
        http.post(format!("{base}/channels/tracker")).header("x-sig", sig).header("x-delivery", delivery).body(body.to_string()).send()
    };
    let comment = |actor: &str, kind: &str, text: &str| json!({"type": kind, "actor": {"id": actor}, "data": {"issueId": "ISS 7", "body": text}});

    assert_eq!(send(comment("ann", "Comment", "@agent hi"), "wrong").await.unwrap().status(), 401);
    send(comment("ann", "Issue", "@agent hi"), "shh").await.unwrap();
    send(comment("bot", "Comment", "@agent hi"), "shh").await.unwrap();
    send(comment("ann", "Comment", "no mention"), "shh").await.unwrap();
    send(comment("ann", "Comment", "@agent what is blocking this?"), "shh").await.unwrap();
    wait_n(&posted, 1).await;
    send(comment("ann", "Comment", "@agent and now?"), "shh").await.unwrap();
    wait_n(&posted, 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        *posted.lock().unwrap(),
        [
            json!({"method": "POST", "path": "/issues/ISS%207/comments", "body": {"body": "first \"quoted\" answer", "issue": "ISS 7"}}),
            json!({"method": "POST", "path": "/issues/ISS%207/comments", "body": {"body": "second", "issue": "ISS 7"}}),
        ],
        "the answer is escaped into the JSON template and the thread URL-encoded into the path"
    );
    let reqs = f.mock.reqs();
    assert_eq!(reqs[0]["messages"][1]["content"], "what is blocking this?", "the mention is removed");
    assert_eq!(reqs[1]["messages"].as_array().unwrap().len(), 4, "one session per issue");

    // A mistake in the file stops the agent from loading.
    f.write("channels/tracker.toml", "[verify]\nheader = \"X\"\nsecret = \"s\"\nscheme = \"md5\"\n[message]\ntext = \"/t\"\n", false);
    assert!(egin::agent::Agent::load(&f.dir, &toml::Table::new()).is_err());
}

#[tokio::test]
async fn slack_socket_mode_acks_and_replies_in_thread() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    // A fake Slack: apps.connections.open hands out a WebSocket that delivers one event.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", ws_listener.local_addr().unwrap());
    let acks = Arc::new(Mutex::new(vec![]));
    let acks2 = acks.clone();
    tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let event = json!({"envelope_id": "env-1", "type": "events_api", "payload": {"type": "event_callback", "event_id": "Ev9",
            "event": {"type": "message", "channel_type": "im", "channel": "D1", "ts": "222.2", "text": "hello"}}});
        ws.send(Message::Text(event.to_string())).await.unwrap();
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            acks2.lock().unwrap().push(serde_json::from_str::<Value>(&t).unwrap());
        }
    });
    let (api, posted) = fake_slack(Some(ws_url)).await;

    let f = fx(vec![say("hi back")], Approve::Auto).await;
    f.write("channels/slack.toml", &format!("app_token = \"xapp-test\"\nbot_token = \"xoxb-test\"\napi_url = \"{api}\"\n"), false);
    let (_, srv) = server::router(f.app.clone(), f.agent(""), None);
    egin::server::channels::socket_mode(srv);
    wait_for(&posted).await;
    assert_eq!(*posted.lock().unwrap(), [json!({"channel": "D1", "thread_ts": "222.2", "text": "hi back"})]);
    assert_eq!(*acks.lock().unwrap(), [json!({"envelope_id": "env-1"})]);
}

/// A stand-in for `vercel`: `board tail` prints the events in `events.jsonl`, then stays open the way a
/// live tail does; `board reply` records its arguments, BOARD_PROFILE and the text it was given on stdin.
const FAKE_VERCEL: &str = r#"#!/bin/sh
dir=$(dirname "$0")
case "$2" in
  tail) cat "$dir/events.jsonl"; sleep 30 ;;
  reply) { echo "ARGS $* PROFILE=$BOARD_PROFILE"; cat; echo; } >> "$dir/replies.log"; echo '{"id":"msg_r"}' ;;
esac
"#;

#[tokio::test]
async fn board_channel_answers_in_the_thread_and_skips_its_own_and_repeated_messages() {
    let f = fx(vec![say("first answer"), say("second answer")], Approve::Auto).await;
    f.write("fake-vercel.sh", FAKE_VERCEL, true);
    let ev = |cursor: &str, id: &str, re: Value, text: &str, handle: &str| {
        json!({"cursor": cursor, "type": "message.posted", "message": {"id": id, "re": re, "text": text}, "actor": {"id": format!("act_{handle}"), "handle": handle}}).to_string()
    };
    let events = [
        ev("c1", "msg_1", Value::Null, "is the deploy green?", "ada"),
        // The agent's own reply, as the board shows a profile that writes with ada's login.
        ev("c2", "msg_2", json!("msg_1"), "first answer", "ada/egin"),
        // The same event again, as a tail restarted from an earlier cursor prints it.
        ev("c1", "msg_1", Value::Null, "is the deploy green?", "ada"),
        // Not a new message.
        json!({"cursor": "c3", "type": "labels.changed", "message": {"id": "msg_1", "re": null, "text": "is the deploy green?"}, "actor": {"handle": "ada"}})
            .to_string(),
        ev("c4", "msg_3", json!("msg_1"), "and the preview?", "bob"),
    ];
    f.write("events.jsonl", &(events.join("\n") + "\n"), false);
    let cli = f.dir.join("fake-vercel.sh");
    f.write("channels/board.toml", &format!("channel = \"dev\"\nprofile = \"egin\"\ncli = \"{}\"\n", cli.display()), false);

    let (_, srv) = server::router(f.app.clone(), f.agent(""), None);
    egin::server::channels::board_follow(srv);
    let log = f.dir.join("replies.log");
    for _ in 0..200 {
        if std::fs::read_to_string(&log).is_ok_and(|l| l.contains("second answer")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let log = std::fs::read_to_string(&log).expect("the agent replied");
    let replies: Vec<&str> = log.lines().filter(|l| l.starts_with("ARGS")).collect();
    assert_eq!(replies, ["ARGS board reply msg_1 --file - --json PROFILE=egin"; 2], "both answers go to the thread's root: {log}");
    assert!(log.contains("first answer") && log.contains("second answer"), "{log}");
    assert_eq!(f.mock.reqs().len(), 2, "its own message, the repeat and the label change start no turn");
    let sid = f.app.store.thread("board:msg_1").unwrap().expect("one session for the thread");
    let second = &f.mock.reqs()[1]["messages"];
    assert!(second.as_array().unwrap().iter().any(|m| m["content"] == "is the deploy green?"), "the reply ran in the thread's session {sid}");
}

async fn status_becomes(f: &common::Fx, sid: &str, want: &str) -> String {
    for _ in 0..100 {
        let s = f.app.store.status(sid).unwrap().unwrap_or_default();
        if s == want {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f.app.store.status(sid).unwrap().unwrap_or_default()
}

#[tokio::test]
async fn cancelling_a_session_cancels_its_running_subagent() {
    // The parent hands work to `helper`, whose bash call is still running when the parent is cancelled.
    let task = common::call(&[("t1", "task", json!({"agent": "helper", "prompt": "work"}))]);
    let f = fx(vec![task, bash("b1", "sleep 5")], Approve::Auto).await;
    f.write("subagents/helper/agent.toml", "model = \"mock\"\napproval = \"never\"\nsandbox = \"local\"\n", false);
    f.write("subagents/helper/instructions.md", "Do the work.", false);
    let base = f.serve(f.agent("")).await;
    let http = reqwest::Client::new();
    let created: Value = http.post(format!("{base}/sessions")).json(&json!({"message": "go"})).send().await.unwrap().json().await.unwrap();
    let parent = created["id"].as_str().unwrap().to_string();

    let mut child = None;
    for _ in 0..100 {
        child = f.app.store.children(&parent).unwrap().into_iter().next();
        if child.as_ref().is_some_and(|c| f.app.store.status(c).unwrap().as_deref() == Some("running")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let child = child.expect("the subagent session was started");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(http.post(format!("{base}/sessions/{parent}/cancel")).send().await.unwrap().status().is_success());

    assert_eq!(f.app.store.status(&parent).unwrap().as_deref(), Some("cancelled"));
    assert_eq!(f.app.store.status(&child).unwrap().as_deref(), Some("cancelled"), "the subagent is not left running");
    let events = f.app.store.events_after(&child, 0).unwrap();
    assert_eq!(events.last().unwrap().1["type"], "cancelled");
}

#[tokio::test]
async fn serve_resumes_only_sessions_no_one_else_is_running() {
    let f = fx(vec![say("resumed")], Approve::Auto).await;
    let agent = f.agent("");
    let started = |prompt: &str| {
        let sid = egin::agent::run::new_session(&f.app, &agent).unwrap();
        egin::agent::run::send(&f.app, &sid, prompt).unwrap();
        f.app.store.set_status(&sid, "running").unwrap();
        sid
    };
    // Cut off by a crash: no process renews its lease.
    let orphan = started("finish me");
    // A subagent cut off with its parent: the parent's interrupted task call stands for it.
    let sub = started("sub work");
    f.app.store.set_parent(&sub, &orphan).unwrap();
    // Another process is running this one right now.
    let live = started("someone else's");
    f.app.store.claim(&live, "another-process").unwrap();

    let (_, srv) = server::router(f.app.clone(), agent.clone(), None);
    server::resume_all(&srv).unwrap();

    assert_eq!(status_becomes(&f, &orphan, "done").await, "done", "the crashed session is resumed");
    assert_eq!(f.app.store.status(&sub).unwrap().as_deref(), Some("cancelled"), "the subagent is left to its parent");
    assert_eq!(f.app.store.status(&live).unwrap().as_deref(), Some("running"), "a session with a fresh lease is not taken over");
    assert_eq!(f.mock.reqs().len(), 1, "only the crashed session called the model");
}

#[tokio::test]
async fn a_slack_session_resumed_after_a_restart_still_replies() {
    let (slack_api, posted) = fake_slack(None).await;
    let f = fx(vec![say("pong")], Approve::Auto).await;
    f.write("channels/slack.toml", &format!("signing_secret = \"sig\"\nbot_token = \"xoxb-test\"\napi_url = \"{slack_api}\"\n"), false);
    let agent = f.agent("");
    // A turn the restart cut off: the thread's session holds the message and is marked running.
    let sid = egin::agent::run::new_session(&f.app, &agent).unwrap();
    assert!(f.app.store.claim_thread("slack:C1:111.1", &sid).unwrap());
    egin::agent::run::send(&f.app, &sid, "ping").unwrap();
    f.app.store.set_status(&sid, "running").unwrap();

    let (_, srv) = server::router(f.app.clone(), agent, None);
    server::resume_all(&srv).unwrap();
    wait_for(&posted).await;
    assert_eq!(*posted.lock().unwrap(), [json!({"channel": "C1", "thread_ts": "111.1", "text": "pong"})]);
}
