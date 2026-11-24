//! Channels feed outside messages into sessions. Each conversation thread maps to one session.
//!
//! channels/webhook.toml
//!   secret = "${WEBHOOK_SECRET}"     # required: X-Egin-Signature: sha256=<hex HMAC-SHA256 of the body>
//!   reply_url = "https://..."        # optional: answers are POSTed here; otherwise the request waits
//!   body: {"text": "...", "thread": "optional conversation key"}
//!
//! channels/slack.toml (Events API, request URL <server>/channels/slack)
//!   signing_secret = "${SLACK_SIGNING_SECRET}"
//!   bot_token = "${SLACK_BOT_TOKEN}"  # needs chat:write; subscribe to app_mention and message.im
//!
//! channels/telegram.toml (Bot API webhook <server>/channels/telegram)
//!   bot_token = "${TELEGRAM_BOT_TOKEN}"
//!   secret_token = "${TELEGRAM_SECRET}"  # the secret_token given to setWebhook
//!   username = "my_bot"                  # optional: answer @mentions in groups, not only replies
//!
//! channels/discord.toml (Interactions endpoint <server>/channels/discord)
//!   public_key = "${DISCORD_PUBLIC_KEY}"
//!   bot_token = "${DISCORD_BOT_TOKEN}"   # optional: lets a reply land after a restart
//!
//! channels/github.toml (repository or app webhook <server>/channels/github)
//!   secret = "${GITHUB_WEBHOOK_SECRET}"
//!   token = "${GITHUB_TOKEN}"            # posts the reply comments
//!   username = "my-bot"                  # the account the token belongs to; answers "@my-bot ..."
//!
//! channels/board.toml (Vercel Board, followed through `vercel board tail`; no public URL)
//!   channel = "dev"                      # a channel, or a subtree such as "dev/"
//!   profile = "egin"                     # the profile the agent writes as; its own messages are skipped
//!   args = ["--mentions", "@ada/egin"]   # optional: more `tail` filters

use crate::{agent::run, server::Srv, tools::mcp::expand};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{sync::Arc, time::Duration};

type Res = Result<Json<Value>, (StatusCode, String)>;

fn conf(srv: &Srv, channel: &str, key: &str) -> Option<String> {
    srv.agent.channels.get(channel)?.get(key)?.as_str().map(expand).filter(|s| !s.is_empty())
}

fn verify(secret: &str, signed: &[u8], hex_sig: &str) -> bool {
    let Ok(sig) = hex::decode(hex_sig) else { return false };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(signed);
    mac.verify_slice(&sig).is_ok()
}

fn deny(msg: &str) -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, msg.into())
}

/// Compare two secrets without leaking where they differ through timing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_body(body: &[u8]) -> Result<Value, (StatusCode, String)> {
    serde_json::from_slice(body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

fn ignored() -> Res {
    Ok(Json(json!({"ok": true})))
}

/// What a chat platform shows for a finished turn: the answer, or the failure. Never empty,
/// since Telegram and Discord refuse empty messages.
fn reply_text(answer: anyhow::Result<String>) -> String {
    match answer {
        Ok(a) if a.trim().is_empty() => "(no answer)".into(),
        Ok(a) => a,
        Err(e) => format!("Sorry, that failed: {e:#}"),
    }
}

/// Split `text` into messages of at most `max` UTF-16 units (how Telegram counts), breaking at a
/// newline in the second half of a piece when there is one.
fn split(text: &str, max: usize) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text.trim();
    loop {
        let mut units = 0;
        let Some((cut, _)) = rest.char_indices().find(|(_, c)| {
            units += c.len_utf16();
            units > max
        }) else {
            break;
        };
        let at = rest[..cut].rfind('\n').filter(|&i| i > cut / 2).unwrap_or(cut);
        out.push(rest[..at].trim_end().to_string());
        rest = rest[at..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

async fn send(req: reqwest::RequestBuilder, what: &str) {
    if let Err(e) = req.send().await.and_then(|r| r.error_for_status()) {
        eprintln!("egin: {what}: {e}");
    }
}

/// Remove every `@name` (ASCII, case-insensitive) from `text`.
fn strip_mention(text: &str, name: &str) -> String {
    let (mut text, needle) = (text.to_string(), format!("@{}", name.to_ascii_lowercase()));
    while let Some(i) = text.to_ascii_lowercase().find(&needle) {
        text.replace_range(i..i + needle.len(), "");
    }
    text.trim().to_string()
}

/// Send `text` into the thread's session (creating it on first use), run it, return the answer.
pub async fn converse(srv: &Arc<Srv>, thread: &str, text: &str) -> anyhow::Result<(String, String)> {
    let app = &srv.app;
    let sid = match app.store.thread(thread)? {
        Some(s) => s,
        None => {
            let s = run::new_session(app, &srv.agent)?;
            if !app.store.claim_thread(thread, &s)? {
                app.store.delete_session(&s)?;
            }
            app.store.thread(thread)?.unwrap()
        }
    };
    // One message at a time per thread.
    // ponytail: polls; a per-session notify would be tighter.
    while app.store.status(&sid)?.as_deref() == Some("running") {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    run::send(app, &sid, text)?;
    let answer = srv.run(&sid).await?;
    Ok((sid, answer))
}

async fn webhook(State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    let secret = conf(&srv, "webhook", "secret").ok_or_else(|| deny("webhook secret is not configured"))?;
    let sig = headers.get("x-egin-signature").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("sha256=")).unwrap_or("");
    if !verify(&secret, &body, sig) {
        return Err(deny("bad signature"));
    }
    let msg: Value = serde_json::from_slice(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let Some(text) = msg["text"].as_str().map(String::from) else { return Err((StatusCode::BAD_REQUEST, "`text` is required".into())) };
    let thread = format!("webhook:{}", msg["thread"].as_str().map(String::from).unwrap_or_else(crate::new_id));
    match conf(&srv, "webhook", "reply_url") {
        None => {
            let (sid, answer) = converse(&srv, &thread, &text).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
            Ok(Json(json!({"session": sid, "text": answer})))
        }
        Some(url) => {
            let srv2 = srv.clone();
            tokio::spawn(async move {
                let reply = match converse(&srv2, &thread, &text).await {
                    Ok((sid, answer)) => json!({"thread": thread, "session": sid, "text": answer}),
                    Err(e) => json!({"thread": thread, "error": format!("{e:#}")}),
                };
                post_webhook(&srv2, &url, &reply).await;
            });
            Ok(Json(json!({"accepted": true})))
        }
    }
}

async fn post_webhook(srv: &Srv, url: &str, reply: &Value) {
    if let Err(e) = srv.app.http.post(url).json(reply).send().await {
        eprintln!("egin: webhook reply to {url} failed: {e}");
    }
}

async fn post_slack(srv: &Srv, channel: &str, thread_ts: &str, text: &str) {
    let token = conf(srv, "slack", "bot_token").unwrap_or_default();
    let api = conf(srv, "slack", "api_url").unwrap_or_else(|| "https://slack.com/api".into());
    let r = srv
        .app
        .http
        .post(format!("{api}/chat.postMessage"))
        .bearer_auth(&token)
        .json(&json!({"channel": channel, "thread_ts": thread_ts, "text": text}))
        .send()
        .await;
    match r.and_then(|r| r.error_for_status()) {
        Ok(r) => {
            let body: Value = r.json().await.unwrap_or_default();
            if body["ok"] != true {
                eprintln!("egin: slack chat.postMessage: {body}");
            }
        }
        Err(e) => eprintln!("egin: slack chat.postMessage: {e}"),
    }
}

/// Send the answer of a channel session that `egin serve` resumed after a restart, the way the
/// interrupted turn would have: into its Slack thread, or to the webhook's `reply_url`. A webhook
/// without `reply_url` waited on the HTTP request, which the restart closed, so its answer is only stored.
pub async fn deliver(srv: &Arc<Srv>, thread: &str, sid: &str, answer: anyhow::Result<String>) {
    if let Some(rest) = thread.strip_prefix("slack:") {
        let Some((channel, thread_ts)) = rest.split_once(':') else { return };
        let text = match answer {
            Ok(a) => a,
            Err(e) => format!("Sorry, that failed: {e:#}"),
        };
        post_slack(srv, channel, thread_ts, &text).await;
    } else if thread.starts_with("webhook:") {
        match conf(srv, "webhook", "reply_url") {
            Some(url) => {
                let reply = match answer {
                    Ok(text) => json!({"thread": thread, "session": sid, "text": text}),
                    Err(e) => json!({"thread": thread, "error": format!("{e:#}")}),
                };
                post_webhook(srv, &url, &reply).await;
            }
            None => eprintln!("egin: session {sid} finished after a restart; its webhook caller is gone, so the answer is only stored"),
        }
    } else if let Some(rest) = thread.strip_prefix("telegram:") {
        let (chat, topic) = rest.split_once(':').map_or((rest, None), |(c, t)| (c, Some(t)));
        post_telegram(srv, chat, topic, None, &reply_text(answer)).await;
    } else if let Some(channel) = thread.strip_prefix("discord:") {
        // The interaction token the turn started with has expired by now; a bot token can still post.
        match conf(srv, "discord", "bot_token") {
            Some(token) => {
                let api = discord_api(srv);
                for content in split(&reply_text(answer), DISCORD_MAX) {
                    let req = srv.app.http.post(format!("{api}/channels/{channel}/messages")).header("authorization", format!("Bot {token}"));
                    send(req.json(&json!({"content": content})), "discord channel message").await;
                }
            }
            None => eprintln!("egin: session {sid} finished after a restart; set bot_token in channels/discord.toml to post such answers"),
        }
    } else if let Some(rest) = thread.strip_prefix("github:") {
        let Some((repo, number)) = rest.split_once('#') else { return };
        post_github(srv, repo, number, None, &reply_text(answer)).await;
    } else if let Some(rest) = thread.strip_prefix("custom:") {
        let Some((name, key)) = rest.split_once(':') else { return };
        post_described(srv, name, key, &reply_text(answer)).await;
    } else if let Some(root) = thread.strip_prefix("board:") {
        post_board(srv, root, &reply_text(answer)).await;
    }
}

/// A Board message holds up to 10,000 characters.
const BOARD_MAX: usize = 10_000;

/// `vercel board …` (or `cli` from channels/board.toml), writing as the channel's profile. Board's own
/// variables (BOARD_URL, BOARD_NAME, BOARD_TOKEN) reach it from the environment, as they reach the CLI.
fn board_cmd(srv: &Srv) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(conf(srv, "board", "cli").unwrap_or_else(|| "vercel".into()));
    cmd.arg("board").env("BOARD_PROFILE", board_profile(srv)).kill_on_drop(true);
    cmd
}

fn board_profile(srv: &Srv) -> String {
    conf(srv, "board", "profile").unwrap_or_else(|| "egin".into())
}

/// Reply in the thread rooted at `root`, through stdin so the text stays off the process list.
async fn post_board(srv: &Srv, root: &str, text: &str) {
    use tokio::io::AsyncWriteExt;
    for piece in split(text, BOARD_MAX) {
        let mut cmd = board_cmd(srv);
        cmd.args(["reply", root, "--file", "-", "--json"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let out = async {
            let mut child = cmd.spawn()?;
            child.stdin.take().unwrap().write_all(piece.as_bytes()).await?;
            anyhow::Ok(child.wait_with_output().await?)
        };
        match out.await {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!("egin: board reply: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => eprintln!("egin: board reply: {e:#}"),
        }
    }
}

/// Whether a board actor is the agent's own profile: `ada/egin` when it writes with a person's login,
/// `…+egin` when it writes as a project.
fn own_board_actor(actor: &Value, profile: &str) -> bool {
    let handle = actor["handle"].as_str().unwrap_or("");
    handle == profile || handle.ends_with(&format!("/{profile}")) || actor["id"].as_str().is_some_and(|id| id.ends_with(&format!("+{profile}")))
}

/// One event from `vercel board tail --json`: a new message, not the agent's own, runs in its thread's session.
fn board_event(srv: &Arc<Srv>, profile: &str, ev: &Value) -> anyhow::Result<()> {
    let m = &ev["message"];
    let (Some(id), Some(text)) = (m["id"].as_str(), m["text"].as_str()) else { return Ok(()) };
    let actor = if ev["actor"].is_object() { &ev["actor"] } else { &m["author"] };
    let own = own_board_actor(actor, profile) || m["by"]["profile"].as_str() == Some(profile);
    if ev["type"] != "message.posted" || own || text.trim().is_empty() {
        return Ok(());
    }
    // A tail restarted from its last cursor can print an event again.
    if !srv.app.store.claim_thread(&format!("board-msg:{id}"), "")? {
        return Ok(());
    }
    let root = m["re"].as_str().unwrap_or(id).to_string();
    let (srv, text) = (srv.clone(), text.to_string());
    tokio::spawn(async move {
        let answer = converse(&srv, &format!("board:{root}"), &text).await.map(|(_, a)| a);
        post_board(&srv, &root, &reply_text(answer)).await;
    });
    Ok(())
}

/// Vercel Board: with `channel` set in channels/board.toml, the server follows that channel through
/// `vercel board tail --json` and answers each new message in its thread, one session per thread. Board's
/// watchers wake only a Vercel deployment; the tail reaches an `egin serve` anywhere. A tail that ends is
/// started again from the last event it printed.
pub fn board_follow(srv: Arc<Srv>) {
    let Some(channel) = conf(&srv, "board", "channel") else { return };
    let profile = board_profile(&srv);
    let args: Vec<String> =
        srv.agent.channels["board"].get("args").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str().map(expand)).collect();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let (mut after, mut backoff) = (None::<String>, 1u64);
        loop {
            let mut cmd = board_cmd(&srv);
            cmd.args(["tail", &channel, "--json"]).args(&args).stdout(std::process::Stdio::piped());
            if let Some(a) = &after {
                cmd.args(["--after", a]);
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                        backoff = 1;
                        if let Some(c) = ev["cursor"].as_str() {
                            after = Some(c.to_string());
                        }
                        if let Err(e) = board_event(&srv, &profile, &ev) {
                            eprintln!("egin: board event: {e:#}");
                        }
                    }
                    let status = child.wait().await;
                    eprintln!("egin: board tail {channel} ended ({status:?}); restarting in {backoff}s");
                }
                Err(e) => eprintln!("egin: board tail {channel}: {e}; retrying in {backoff}s"),
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
    });
}

const TELEGRAM_MAX: usize = 4096;

/// sendMessage, split into Telegram's 4096-character messages; the first replies to `reply_to`.
async fn post_telegram(srv: &Srv, chat: &str, topic: Option<&str>, reply_to: Option<i64>, text: &str) {
    let token = conf(srv, "telegram", "bot_token").unwrap_or_default();
    let api = conf(srv, "telegram", "api_url").unwrap_or_else(|| "https://api.telegram.org".into());
    let chat_id = chat.parse::<i64>().map(Value::from).unwrap_or_else(|_| json!(chat));
    for (i, piece) in split(text, TELEGRAM_MAX).into_iter().enumerate() {
        let mut msg = json!({"chat_id": chat_id, "text": piece});
        if let Some(t) = topic.and_then(|t| t.parse::<i64>().ok()) {
            msg["message_thread_id"] = t.into();
        }
        if let (0, Some(id)) = (i, reply_to) {
            msg["reply_parameters"] = json!({"message_id": id});
        }
        send(srv.app.http.post(format!("{api}/bot{token}/sendMessage")).json(&msg), "telegram sendMessage").await;
    }
}

async fn telegram(State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    let secret = conf(&srv, "telegram", "secret_token").ok_or_else(|| deny("telegram secret_token is not configured"))?;
    let got = headers.get("x-telegram-bot-api-secret-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !same(&secret, got) {
        return Err(deny("bad secret token"));
    }
    let update = json_body(&body)?;
    let m = &update["message"];
    let (Some(text), Some(chat)) = (m["text"].as_str(), m["chat"]["id"].as_i64()) else { return ignored() };
    if m["from"]["is_bot"] == true {
        return ignored();
    }
    let username = conf(&srv, "telegram", "username").map(|u| u.trim_start_matches('@').to_string());
    let group = m["chat"]["type"] != "private";
    if group {
        // The bot's id is the part of its token before the colon.
        let bot_id = conf(&srv, "telegram", "bot_token").and_then(|t| t.split(':').next()?.parse::<i64>().ok());
        let replied = bot_id.is_some() && m["reply_to_message"]["from"]["id"].as_i64() == bot_id;
        let mentioned = username.as_ref().is_some_and(|u| text.to_ascii_lowercase().contains(&format!("@{}", u.to_ascii_lowercase())));
        if !replied && !mentioned {
            return ignored();
        }
    }
    // Telegram redelivers updates it thinks failed; each is handled once.
    if !srv.app.store.claim_thread(&format!("telegram-update:{}", update["update_id"]), "").map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
        return ignored();
    }
    let topic = (m["is_topic_message"] == true).then(|| m["message_thread_id"].to_string());
    let thread = match &topic {
        Some(t) => format!("telegram:{chat}:{t}"),
        None => format!("telegram:{chat}"),
    };
    let text = username.map_or_else(|| text.trim().to_string(), |u| strip_mention(text, &u));
    let reply_to = if group { m["message_id"].as_i64() } else { None };
    tokio::spawn(async move {
        let answer = converse(&srv, &thread, &text).await.map(|(_, a)| a);
        post_telegram(&srv, &chat.to_string(), topic.as_deref(), reply_to, &reply_text(answer)).await;
    });
    ignored()
}

const DISCORD_MAX: usize = 2000;

fn discord_api(srv: &Srv) -> String {
    conf(srv, "discord", "api_url").unwrap_or_else(|| "https://discord.com/api/v10".into())
}

async fn discord(State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    use ring::signature::{UnparsedPublicKey, ED25519};
    let key = conf(&srv, "discord", "public_key").ok_or_else(|| deny("discord public_key is not configured"))?;
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let signed = [h("x-signature-timestamp").as_bytes(), &body[..]].concat();
    let valid = match (hex::decode(key), hex::decode(h("x-signature-ed25519"))) {
        (Ok(pk), Ok(sig)) => UnparsedPublicKey::new(&ED25519, pk).verify(&signed, &sig).is_ok(),
        _ => false,
    };
    if !valid {
        return Err(deny("bad signature"));
    }
    let v = json_body(&body)?;
    match v["type"].as_i64() {
        Some(1) => return Ok(Json(json!({"type": 1}))), // PING
        Some(2) => {}                                   // a slash command
        _ => return Err((StatusCode::BAD_REQUEST, "only slash commands are handled".into())),
    }
    let text = v["data"]["options"].as_array().into_iter().flatten().filter_map(|o| o["value"].as_str()).collect::<Vec<_>>().join(" ");
    let channel = v["channel_id"].as_str().or(v["channel"]["id"].as_str()).unwrap_or("dm").to_string();
    let hook = format!("{}/webhooks/{}/{}", discord_api(&srv), v["application_id"].as_str().unwrap_or(""), v["token"].as_str().unwrap_or(""));
    // Discord wants an answer within 3 seconds: defer (the user sees "thinking"), then edit it in.
    tokio::spawn(async move {
        let answer = converse(&srv, &format!("discord:{channel}"), &text).await.map(|(_, a)| a);
        for (i, content) in split(&reply_text(answer), DISCORD_MAX).into_iter().enumerate() {
            let req = match i {
                0 => srv.app.http.patch(format!("{hook}/messages/@original")),
                _ => srv.app.http.post(&hook),
            };
            send(req.json(&json!({"content": content})), "discord interaction reply").await;
        }
    });
    Ok(Json(json!({"type": 5})))
}

/// Comment on the issue or pull request, or reply in a review comment's thread.
async fn post_github(srv: &Srv, repo: &str, number: &str, review_comment: Option<i64>, text: &str) {
    let token = conf(srv, "github", "token").unwrap_or_default();
    let api = conf(srv, "github", "api_url").unwrap_or_else(|| "https://api.github.com".into());
    let url = match review_comment {
        Some(id) => format!("{api}/repos/{repo}/pulls/{number}/comments/{id}/replies"),
        None => format!("{api}/repos/{repo}/issues/{number}/comments"),
    };
    let req = srv.app.http.post(url).bearer_auth(token).header("accept", "application/vnd.github+json").header("user-agent", "egin");
    send(req.json(&json!({"body": text})), "github comment").await;
}

async fn github(State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    let secret = conf(&srv, "github", "secret").ok_or_else(|| deny("github secret is not configured"))?;
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if !verify(&secret, &body, h("x-hub-signature-256").strip_prefix("sha256=").unwrap_or("")) {
        return Err(deny("bad signature"));
    }
    let event = h("x-github-event");
    let v = json_body(&body)?;
    let review = event == "pull_request_review_comment";
    if !(review || event == "issue_comment") || v["action"] != "created" {
        return ignored(); // ping and every other event
    }
    let c = &v["comment"];
    let author = c["user"]["login"].as_str().unwrap_or("");
    let me = conf(&srv, "github", "username").unwrap_or_default();
    // The bot's own comments, and any bot's, would start a loop.
    if c["user"]["type"] == "Bot" || v["sender"]["type"] == "Bot" || author.eq_ignore_ascii_case(&me) {
        return ignored();
    }
    let text = c["body"].as_str().unwrap_or("");
    if me.is_empty() || !text.to_ascii_lowercase().contains(&format!("@{}", me.to_ascii_lowercase())) {
        return ignored();
    }
    if !srv
        .app
        .store
        .claim_thread(&format!("github-delivery:{}", h("x-github-delivery")), "")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return ignored();
    }
    let repo = v["repository"]["full_name"].as_str().unwrap_or("").to_string();
    let number = if review { &v["pull_request"]["number"] } else { &v["issue"]["number"] }.to_string();
    let review_comment = if review { c["id"].as_i64() } else { None };
    let text = strip_mention(text, &me);
    tokio::spawn(async move {
        let answer = converse(&srv, &format!("github:{repo}#{number}"), &text).await.map(|(_, a)| a);
        post_github(&srv, &repo, &number, review_comment, &reply_text(answer)).await;
    });
    ignored()
}

async fn slack(State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    let secret = conf(&srv, "slack", "signing_secret").ok_or_else(|| deny("slack signing_secret is not configured"))?;
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let ts = h("x-slack-request-timestamp");
    let age = ts.parse::<i64>().map(|t| (chrono::Utc::now().timestamp() - t).abs()).unwrap_or(i64::MAX);
    let signed = [format!("v0:{ts}:").as_bytes(), &body[..]].concat();
    if age > 300 || !verify(&secret, &signed, h("x-slack-signature").strip_prefix("v0=").unwrap_or("")) {
        return Err(deny("bad or stale signature"));
    }
    let v: Value = serde_json::from_slice(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if v["type"] == "url_verification" {
        return Ok(Json(json!({"challenge": v["challenge"]})));
    }
    slack_event(srv, v).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({"ok": true})))
}

/// Handle one Events API payload (from the HTTP endpoint or Socket Mode): run the agent in the
/// thread's session and reply in the thread. Returns at once; the reply is posted when done.
fn slack_event(srv: Arc<Srv>, v: Value) -> anyhow::Result<()> {
    let e = &v["event"];
    let wanted = e["type"] == "app_mention" || (e["type"] == "message" && e["channel_type"] == "im");
    if v["type"] != "event_callback" || !wanted || e.get("bot_id").is_some() || e.get("subtype").is_some() {
        return Ok(());
    }
    // Slack retries deliveries it thinks failed; each event is handled once.
    let event_id = v["event_id"].as_str().unwrap_or("");
    if !srv.app.store.claim_thread(&format!("slack-event:{event_id}"), "")? {
        return Ok(());
    }
    let channel = e["channel"].as_str().unwrap_or("").to_string();
    let thread_ts = e["thread_ts"].as_str().or(e["ts"].as_str()).unwrap_or("").to_string();
    let mut text = e["text"].as_str().unwrap_or("").to_string();
    while let (Some(a), Some(b)) = (text.find("<@"), text.find('>')) {
        if b < a {
            break;
        }
        text.replace_range(a..=b, "");
    }
    // Answer Slack within its 3-second window; the agent replies in the thread when done.
    tokio::spawn(async move {
        let reply = match converse(&srv, &format!("slack:{channel}:{thread_ts}"), text.trim()).await {
            Ok((_, answer)) => answer,
            Err(e) => format!("Sorry, that failed: {e:#}"),
        };
        post_slack(&srv, &channel, &thread_ts, &reply).await;
    });
    Ok(())
}

/// Slack Socket Mode: with `app_token` (xapp-…, connections:write) set, the server dials Slack
/// over a WebSocket instead of exposing /channels/slack, so it works behind NAT. Reconnects forever.
pub fn socket_mode(srv: Arc<Srv>) {
    let Some(app_token) = conf(&srv, "slack", "app_token") else { return };
    let api = conf(&srv, "slack", "api_url").unwrap_or_else(|| "https://slack.com/api".into());
    tokio::spawn(async move {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let mut backoff = 1u64;
        loop {
            let url = async {
                let r: Value = srv.app.http.post(format!("{api}/apps.connections.open")).bearer_auth(&app_token).send().await?.json().await?;
                r["url"].as_str().map(String::from).ok_or_else(|| anyhow::anyhow!("apps.connections.open: {r}"))
            };
            let ws = match url.await {
                Ok(u) => tokio_tungstenite::connect_async(u).await.map_err(anyhow::Error::from),
                Err(e) => Err(e),
            };
            let (mut ws, _) = match ws {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("egin: slack socket mode: {e:#}; retrying in {backoff}s");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                    continue;
                }
            };
            eprintln!("egin: slack socket mode connected");
            backoff = 1;
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(env) = serde_json::from_str::<Value>(&text) else { continue };
                // Every envelope is acknowledged at once, then handled.
                if let Some(id) = env["envelope_id"].as_str() {
                    let _ = ws.send(Message::Text(json!({"envelope_id": id}).to_string())).await;
                }
                match env["type"].as_str() {
                    Some("events_api") => {
                        if let Err(e) = slack_event(srv.clone(), env["payload"].clone()) {
                            eprintln!("egin: slack event: {e:#}");
                        }
                    }
                    Some("disconnect") => break, // Slack asks clients to reconnect periodically.
                    _ => {}
                }
            }
        }
    });
}

pub fn routes(srv: &Arc<Srv>) -> Router<Arc<Srv>> {
    let mut r = Router::new();
    if srv.agent.channels.contains_key("webhook") {
        r = r.route("/channels/webhook", post(webhook));
    }
    if srv.agent.channels.contains_key("slack") {
        r = r.route("/channels/slack", post(slack));
    }
    for (name, handler) in [("telegram", post(telegram)), ("discord", post(discord)), ("github", post(github))] {
        if srv.agent.channels.contains_key(name) {
            r = r.route(&format!("/channels/{name}"), handler);
        }
    }
    // Described channels share one route; the built-in ones above take precedence by name.
    if srv.agent.channels.iter().any(|(n, t)| !BUILT_IN.contains(&n.as_str()) && t.contains_key("verify")) {
        r = r.route("/channels/{name}", post(described));
    }
    r
}

// --- Any signed webhook, from a description -----------------------------------------------------

/// A channel described in its file instead of built into egin: channels/<name>.toml with a
/// `[verify]` table is served at /channels/<name>. It checks a signature header over the raw body,
/// takes the message text and a thread key from the JSON with JSON pointers, runs the thread's
/// session, and sends the answer with an HTTP request built from a template.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generic {
    verify: Verify,
    message: Message,
    reply: Option<Reply>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Verify {
    header: String,
    secret: String,
    /// "hmac-sha256" (default), "hmac-sha1", or "token" (the header carries the secret itself).
    #[serde(default = "hmac256")]
    scheme: String,
    /// "hex" (default) or "base64".
    #[serde(default = "hex_enc")]
    encoding: String,
    /// Stripped from the header value first, such as "sha256=".
    #[serde(default)]
    prefix: String,
}

fn hmac256() -> String {
    "hmac-sha256".into()
}
fn hex_enc() -> String {
    "hex".into()
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    /// JSON pointer to the message text, such as "/data/body".
    text: String,
    /// JSON pointer to what makes a conversation: one session per value. Without it, one per request.
    thread: Option<String>,
    /// A delivery id, from a JSON pointer or a header: a delivery seen before is skipped.
    id: Option<String>,
    id_header: Option<String>,
    /// Handle an event only when every pointer here has this value.
    #[serde(default)]
    only: toml::Table,
    /// Ignore an event when any pointer here has this value (the bot's own messages, say).
    #[serde(default)]
    skip: toml::Table,
    /// Answer only text that contains this, which is then removed (an @mention).
    mention: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    url: String,
    #[serde(default = "post_method")]
    method: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    /// The request body; `{{answer}}` and `{{thread}}` become JSON strings, `${VAR}` reads the environment.
    body: String,
}

fn post_method() -> String {
    "POST".into()
}

/// Built-in channels have their own handlers; any other file with `[verify]` is a described one.
const BUILT_IN: [&str; 7] = ["webhook", "slack", "telegram", "discord", "github", "mcp", "board"];

/// Check a described channel's file when the agent loads.
pub fn check(name: &str, table: &toml::Table) -> anyhow::Result<()> {
    if BUILT_IN.contains(&name) || !table.contains_key("verify") {
        return Ok(());
    }
    let g: Generic = toml::Value::Table(table.clone()).try_into().map_err(|e| anyhow::anyhow!("channels/{name}.toml: {e}"))?;
    if !matches!(g.verify.scheme.as_str(), "hmac-sha256" | "hmac-sha1" | "token") || !matches!(g.verify.encoding.as_str(), "hex" | "base64") {
        anyhow::bail!("channels/{name}.toml: verify.scheme is hmac-sha256, hmac-sha1 or token; verify.encoding is hex or base64");
    }
    Ok(())
}

fn generic(srv: &Srv, name: &str) -> Option<Generic> {
    let t = srv.agent.channels.get(name)?;
    toml::Value::Table(t.clone()).try_into().ok()
}

fn signed(v: &Verify, body: &[u8], got: &str) -> bool {
    let secret = expand(&v.secret);
    let got = got.trim().strip_prefix(v.prefix.as_str()).unwrap_or(got.trim());
    if secret.is_empty() {
        return false;
    }
    if v.scheme == "token" {
        return same(&secret, got);
    }
    use base64::Engine;
    let sig = if v.encoding == "base64" { base64::engine::general_purpose::STANDARD.decode(got).ok() } else { hex::decode(got).ok() };
    let Some(sig) = sig else { return false };
    let alg = if v.scheme == "hmac-sha1" { ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY } else { ring::hmac::HMAC_SHA256 };
    ring::hmac::verify(&ring::hmac::Key::new(alg, secret.as_bytes()), body, &sig).is_ok()
}

/// A JSON value as text: strings as they are, anything else as JSON.
fn plain(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Whether the pointer's value equals the expected one (a string in the file may use `${VAR}`).
fn matches_value(event: &Value, pointer: &str, want: &toml::Value) -> bool {
    let got = event.pointer(pointer).unwrap_or(&Value::Null);
    match want {
        toml::Value::String(s) => got.as_str() == Some(expand(s).as_str()),
        other => serde_json::to_value(other).is_ok_and(|w| &w == got),
    }
}

async fn described(axum::extract::Path(name): axum::extract::Path<String>, State(srv): State<Arc<Srv>>, headers: HeaderMap, body: Bytes) -> Res {
    let g = generic(&srv, &name).ok_or_else(|| (StatusCode::NOT_FOUND, "no such channel".into()))?;
    let got = headers.get(g.verify.header.as_str()).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !signed(&g.verify, &body, got) {
        return Err(deny("bad signature"));
    }
    let event = json_body(&body)?;
    let m = &g.message;
    if !m.only.iter().all(|(p, w)| matches_value(&event, p, w)) || m.skip.iter().any(|(p, w)| matches_value(&event, p, w)) {
        return ignored();
    }
    let Some(mut text) = event.pointer(&m.text).and_then(plain) else { return ignored() };
    if let Some(mention) = &m.mention {
        let mention = expand(mention);
        if !text.contains(&mention) {
            return ignored();
        }
        text = text.replace(&mention, "").trim().to_string();
    }
    let delivery =
        m.id.as_ref()
            .and_then(|p| event.pointer(p))
            .and_then(plain)
            .or_else(|| m.id_header.as_ref().and_then(|h| headers.get(h.as_str())).and_then(|v| v.to_str().ok()).map(String::from));
    if let Some(d) = delivery {
        if !srv.app.store.claim_thread(&format!("{name}-delivery:{d}"), "").map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
            return ignored();
        }
    }
    let thread = m.thread.as_ref().and_then(|p| event.pointer(p)).and_then(plain).unwrap_or_else(crate::new_id);
    let key = format!("custom:{name}:{thread}");
    if g.reply.is_none() {
        // No reply request: the caller waits for the answer, as with the webhook channel.
        let (sid, answer) = converse(&srv, &key, &text).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        return Ok(Json(json!({"session": sid, "thread": thread, "text": answer})));
    }
    tokio::spawn(async move {
        let answer = converse(&srv, &key, &text).await.map(|(_, a)| a);
        post_described(&srv, &name, &thread, &reply_text(answer)).await;
    });
    ignored()
}

/// Send an answer with the channel's reply template.
async fn post_described(srv: &Srv, name: &str, thread: &str, answer: &str) {
    let Some(Generic { reply: Some(r), .. }) = generic(srv, name) else { return };
    let fill = |s: &str, quote: bool| {
        let (a, t) = if quote { (json!(answer).to_string(), json!(thread).to_string()) } else { (answer.to_string(), thread.to_string()) };
        expand(s).replace("{{answer}}", &a).replace("{{thread}}", &t)
    };
    let url = expand(&r.url).replace("{{thread}}", &url_encode(thread));
    let method = reqwest::Method::from_bytes(r.method.to_uppercase().as_bytes()).unwrap_or(reqwest::Method::POST);
    let mut req = srv.app.http.request(method, url).header("content-type", "application/json").body(fill(&r.body, true));
    for (k, v) in &r.headers {
        req = req.header(k.as_str(), fill(v, false));
    }
    send(req, &format!("channel {name} reply")).await;
}

fn url_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_pieces_under_the_limit() {
        assert_eq!(split("  hi  ", 10), ["hi"]);
        assert!(split("", 10).is_empty());
        let long = format!("{}\n{}", "a".repeat(8), "b".repeat(8));
        assert_eq!(split(&long, 10), ["aaaaaaaa", "bbbbbbbb"], "breaks at the newline");
        assert_eq!(split(&"x".repeat(25), 10).iter().map(|p| p.len()).collect::<Vec<_>>(), [10, 10, 5]);
        // An emoji is two UTF-16 units, as Telegram counts it.
        assert_eq!(split(&"😀".repeat(6), 10).len(), 2);
        assert_eq!(strip_mention("@Bot hi @bot", "bot"), "hi");
    }
}
