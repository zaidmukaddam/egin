//! E2B as a cloud sandbox backend (`sandbox = "e2b"`, `E2B_API_KEY`).
//!
//! One E2B sandbox per session, tagged `egin=<session>` in its metadata so a restarted egin finds
//! it again. It is created on the first tool call from the template in `E2B_TEMPLATE` (default
//! `base`), seeded with the agent's workspace/ and tools/, kept alive for `idle_timeout` past the
//! last command, and killed with the session. Files live in the sandbox; the host workspace is
//! copied up once and not synced back.
//!
//! Commands go to envd, the daemon inside the sandbox, over Connect's streaming JSON protocol:
//! each frame is one flag byte, a 4-byte big-endian length, then a JSON message.

use super::{open_or_closed, quote, render, tar_gz, timed_out, workdir, Out};
use crate::{agent::Agent, App};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

pub const WORKDIR: &str = "/home/user/workspace";
/// The port envd listens on inside every sandbox.
const ENVD_PORT: u16 = 49983;

struct Api {
    base: String,
    key: String,
}

fn api() -> Result<Api> {
    let key = std::env::var("E2B_API_KEY").context("sandbox = \"e2b\" needs E2B_API_KEY")?;
    let domain = std::env::var("E2B_DOMAIN").unwrap_or_else(|_| "e2b.app".into());
    Ok(Api { base: format!("https://api.{domain}"), key })
}

/// What one live sandbox needs: its id, envd's access token, and its domain.
#[derive(Clone, Debug)]
pub struct Sb {
    pub id: String,
    token: String,
    domain: String,
}

impl Sb {
    fn from(v: &Value) -> Result<Sb> {
        Ok(Sb {
            id: v["sandboxID"].as_str().context("e2b: no sandboxID")?.into(),
            token: v["envdAccessToken"].as_str().unwrap_or("").into(),
            domain: v["domain"]
                .as_str()
                .filter(|d| !d.is_empty())
                .map(String::from)
                .unwrap_or_else(|| std::env::var("E2B_DOMAIN").unwrap_or_else(|_| "e2b.app".into())),
        })
    }

    fn envd(&self) -> String {
        format!("https://{ENVD_PORT}-{}.{}", self.id, self.domain)
    }
}

async fn call(http: &reqwest::Client, a: &Api, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
    let mut r = http.request(method, format!("{}{path}", a.base)).header("X-API-Key", &a.key).timeout(Duration::from_secs(120));
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(json!({ "raw": text }))))
}

fn key(sid: &str) -> String {
    format!("e2b:{sid}")
}

/// The session's sandbox: remembered, found again by its metadata, or created.
async fn ensure(app: &App, agent: &Agent, sid: &str) -> Result<Sb> {
    let mut map = app.sandboxes.lock().await;
    if let Some(v) = map.get(&key(sid)) {
        return Sb::from(v);
    }
    let a = api()?;
    let ttl = agent.cfg.idle_timeout.max(60);
    let query = format!("/v2/sandboxes?metadata={}&state=running,paused", urlencode(&format!("egin={sid}")));
    let (status, list) = call(&app.http, &a, reqwest::Method::GET, &query, None).await?;
    let existing = (status < 300).then(|| list.as_array().and_then(|l| l.first()).and_then(|s| s["sandboxID"].as_str()).map(String::from)).flatten();
    let (v, created) = match existing {
        // Connect resumes a paused sandbox and hands out a fresh envd token.
        Some(id) => {
            let (s, v) = call(&app.http, &a, reqwest::Method::POST, &format!("/v2/sandboxes/{id}/connect"), Some(json!({"timeout": ttl}))).await?;
            if s >= 300 {
                bail!("e2b connect {id}: {s}: {v}");
            }
            (v, false)
        }
        None => {
            if !agent.cfg.secrets.is_empty() {
                bail!("`secrets` (host-side substitution) needs sandbox = \"microsandbox\"; E2B would put the value in the guest");
            }
            let mut body = json!({
                "templateID": std::env::var("E2B_TEMPLATE").unwrap_or_else(|_| "base".into()),
                "timeout": ttl,
                "cpuCount": agent.cfg.cpus,
                "metadata": {"egin": sid},
                "allow_internet_access": open_or_closed(agent, "e2b")?,
            });
            let (mut s, mut v) = call(&app.http, &a, reqwest::Method::POST, "/v2/sandboxes", Some(body.clone())).await?;
            // Plans without custom sizes refuse `cpuCount`; the template's own size is used instead.
            if s >= 300 && v.to_string().contains("cpu_count_disabled") {
                body.as_object_mut().unwrap().remove("cpuCount");
                (s, v) = call(&app.http, &a, reqwest::Method::POST, "/v2/sandboxes", Some(body)).await?;
            }
            if s >= 300 {
                bail!("e2b create: {s}: {v}");
            }
            (v, true)
        }
    };
    let sb = Sb::from(&v)?;
    map.insert(key(sid), v);
    drop(map);
    if created {
        seed(app, &sb, agent).await?;
    }
    Ok(sb)
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

async fn upload(http: &reqwest::Client, sb: &Sb, path: &str, bytes: Vec<u8>, user: &str) -> Result<()> {
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(bytes).file_name("f"));
    let r = http
        .post(format!("{}/files", sb.envd()))
        .query(&[("path", path), ("username", user)])
        .header("X-Access-Token", &sb.token)
        .multipart(form)
        .timeout(Duration::from_secs(300))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("e2b upload {path}: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    Ok(())
}

/// One Connect frame: flag byte, big-endian length, JSON.
fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string().into_bytes();
    let mut out = vec![0u8];
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend(body);
    out
}

/// The frames of a Connect stream: (flag, JSON). Flag 2 marks the end-of-stream frame.
fn frames(mut bytes: &[u8]) -> Vec<(u8, Value)> {
    let mut out = vec![];
    while bytes.len() >= 5 {
        let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        let Some(payload) = bytes.get(5..5 + len) else { break };
        out.push((bytes[0], serde_json::from_slice(payload).unwrap_or(Value::Null)));
        bytes = &bytes[5 + len..];
    }
    out
}

async fn run(http: &reqwest::Client, sb: &Sb, command: &str, cwd: &str, timeout: u64, user: &str) -> Result<Out> {
    // A task image may have no bash; sh is everywhere.
    let shell = if user == "root" { "/bin/sh" } else { "/bin/bash" };
    let args = if user == "root" { json!(["-c", command]) } else { json!(["-l", "-c", command]) };
    let start = json!({"process": {"cmd": shell, "args": args, "envs": {}, "cwd": cwd}, "stdin": false});
    let user = base64::engine::general_purpose::STANDARD.encode(format!("{user}:"));
    let r = http
        .post(format!("{}/process.Process/Start", sb.envd()))
        .header("Content-Type", "application/connect+json")
        .header("Connect-Protocol-Version", "1")
        .header("Connect-Timeout-Ms", (timeout * 1000).to_string())
        .header("Keepalive-Ping-Interval", "50")
        .header("X-Access-Token", &sb.token)
        .header("Authorization", format!("Basic {user}"))
        .body(frame(&start))
        .timeout(Duration::from_secs(timeout + 60))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("e2b exec: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    let bytes = r.bytes().await?;
    let (mut stdout, mut stderr, mut code) = (vec![], vec![], None);
    let b64 = |v: &Value| base64::engine::general_purpose::STANDARD.decode(v.as_str().unwrap_or("")).unwrap_or_default();
    for (flag, msg) in frames(&bytes) {
        if flag & 2 != 0 {
            if let Some(e) = msg.get("error") {
                if e["code"] == "deadline_exceeded" {
                    return Ok(timed_out(timeout));
                }
                bail!("e2b exec: {} {}", e["code"], e["message"]);
            }
            continue;
        }
        let ev = &msg["event"];
        stdout.extend(b64(&ev["data"]["stdout"]));
        stderr.extend(b64(&ev["data"]["stderr"]));
        if ev["end"].is_object() {
            // proto3 JSON leaves a zero exit code out.
            code = Some(ev["end"]["exitCode"].as_i64().unwrap_or(0) as i32);
        }
    }
    Ok(render(code.unwrap_or(-1), &stdout, &stderr))
}

/// Copy the agent's workspace/ and tools/ into a new sandbox.
async fn seed(app: &App, sb: &Sb, agent: &Agent) -> Result<()> {
    for (dir, dst) in [(agent.workspace.clone(), WORKDIR.to_string()), (agent.dir.join("tools"), "/egin/tools".to_string())] {
        if !dir.is_dir() {
            continue;
        }
        let tmp = format!("/tmp/egin-seed-{}.tgz", crate::new_id());
        upload(&app.http, sb, &tmp, tar_gz(&dir)?, "user").await?;
        let o =
            run(&app.http, sb, &format!("sudo mkdir -p {dst} && sudo chown user {dst} && tar -xzf {tmp} -C {dst} && rm -f {tmp}"), "/", 300, "user").await?;
        if !o.ok {
            bail!("seeding {dst}: {}", o.text);
        }
    }
    Ok(())
}

pub async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let mut retried = false;
    loop {
        let sb = ensure(app, agent, sid).await?;
        // The sandbox's lifetime is a hard deadline: push it out to idle_timeout past this command.
        if let Ok(a) = api() {
            let _ = call(
                &app.http,
                &a,
                reqwest::Method::POST,
                &format!("/sandboxes/{}/timeout", sb.id),
                Some(json!({"timeout": agent.cfg.idle_timeout.max(60) + timeout})),
            )
            .await;
        }
        let mut script = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        if let Some(data) = &stdin {
            let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
            upload(&app.http, &sb, &f, data.clone(), "user").await?;
            script = format!("{script} < {f}; c=$?; rm -f {f}; exit $c");
        }
        match run(&app.http, &sb, &script, &workdir(agent), timeout, "user").await {
            Ok(o) => return Ok(o),
            // It may have timed out or paused: forget it, find or create it again, retry once.
            Err(_) if !retried => {
                app.sandboxes.lock().await.remove(&key(sid));
                retried = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Kill the session's sandbox, if it has one.
pub async fn remove(app: &App, sid: &str) {
    let known = app.sandboxes.lock().await.remove(&key(sid)).and_then(|v| v["sandboxID"].as_str().map(String::from));
    let Ok(a) = api() else { return };
    let id = match known {
        Some(id) => Some(id),
        None => {
            let q = format!("/v2/sandboxes?metadata={}", urlencode(&format!("egin={sid}")));
            call(&app.http, &a, reqwest::Method::GET, &q, None)
                .await
                .ok()
                .and_then(|(_, l)| l.as_array().and_then(|l| l.first()).and_then(|s| s["sandboxID"].as_str()).map(String::from))
        }
    };
    if let Some(id) = id {
        let _ = call(&app.http, &a, reqwest::Method::DELETE, &format!("/sandboxes/{id}"), None).await;
    }
}

// ---- the fallback for `egin bench --sandbox daytona` ----
//
// When Daytona cannot give a run its sandbox, the run gets one from E2B instead: a template built once from the
// task's base image, the Dockerfile's steps replayed into it as a local microVM does, commands run as root.

/// E2B template aliases: lowercase letters, digits and dashes.
fn alias(image: &str) -> String {
    use sha2::{Digest, Sha256};
    let clean: String = image.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let hash = hex::encode(&Sha256::digest(image.as_bytes())[..4]);
    format!("egin-{}-{hash}", clean.trim_matches('-').chars().take(40).collect::<String>())
}

/// The E2B template of a Docker image, built on first use (minutes) and found by its alias after that.
pub async fn bench_template(app: &App, image: &str) -> Result<String> {
    // One build at a time per process: runs of the same task wait for the first one's build.
    static BUILDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = BUILDING.lock().await;
    let a = api()?;
    let name = alias(image);
    let (s, v) = call(&app.http, &a, reqwest::Method::GET, &format!("/templates/aliases/{name}"), None).await?;
    if s < 300 && v["templateID"].is_string() {
        return Ok(name);
    }
    let (s, v) = call(&app.http, &a, reqwest::Method::POST, "/v3/templates", Some(json!({"name": name, "cpuCount": 2, "memoryMB": 4096}))).await?;
    if s >= 300 {
        bail!("e2b template {name}: {s}: {v}");
    }
    let (tid, bid) = (v["templateID"].as_str().context("e2b: no templateID")?.to_string(), v["buildID"].as_str().context("e2b: no buildID")?.to_string());
    let (s, v) = call(
        &app.http,
        &a,
        reqwest::Method::POST,
        &format!("/v2/templates/{tid}/builds/{bid}"),
        Some(json!({"fromImage": image, "steps": [], "force": false})),
    )
    .await?;
    if s >= 300 {
        bail!("e2b build of {image}: {s}: {v}");
    }
    for _ in 0..360 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (_, st) = call(&app.http, &a, reqwest::Method::GET, &format!("/templates/{tid}/builds/{bid}/status"), None).await?;
        match st["status"].as_str().unwrap_or("") {
            "ready" | "uploaded" => return Ok(name),
            "error" | "failed" => bail!("e2b build of {image} failed: {}", st["reason"]),
            _ => {}
        }
    }
    bail!("e2b build of {image} did not finish in 30 minutes")
}

/// A bench run's sandbox from a template, for `lifetime` seconds (or the longest the plan allows).
pub async fn bench_create(app: &App, name: &str, template: &str, env: &[(String, String)], cpus: u8, offline: bool, lifetime: u64) -> Result<Sb> {
    let a = api()?;
    let mut body = json!({
        "templateID": template,
        "timeout": lifetime,
        "cpuCount": cpus.max(1),
        "metadata": {"egin": name},
        "envVars": env.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>(),
        "allow_internet_access": !offline,
    });
    for _ in 0..3 {
        let (s, v) = call(&app.http, &a, reqwest::Method::POST, "/v2/sandboxes", Some(body.clone())).await?;
        if s < 300 {
            return Sb::from(&v);
        }
        let text = v.to_string();
        if text.contains("cpu_count_disabled") {
            body.as_object_mut().unwrap().remove("cpuCount");
        } else if text.to_lowercase().contains("timeout") {
            // Plans cap a sandbox's lifetime (an hour on the smallest).
            body["timeout"] = json!(3600);
        } else {
            bail!("e2b create {name}: {s}: {v}");
        }
    }
    bail!("e2b create {name}: refused")
}

/// A shell script in a bench sandbox, as root: (exit code, combined output), 124 on a timeout.
pub async fn bench_sh(http: &reqwest::Client, sb: &Sb, script: &str, cwd: &str, stdin: Option<Vec<u8>>, timeout: u64) -> Result<(i32, String)> {
    let mut script = script.to_string();
    if let Some(data) = stdin {
        let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
        upload(http, sb, &f, data, "root").await?;
        script = format!("( {script} ) < {f}; c=$?; rm -f {f}; exit $c");
    }
    let o = run(http, sb, &script, cwd, timeout, "root").await?;
    Ok(match o.text.rfind("[exit ") {
        _ if o.ok => (0, o.text),
        Some(i) => (o.text[i + 6..].trim_end_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(1), o.text),
        None => (124, o.text),
    })
}

/// A bench sandbox as the agent's tools see it: Out, the way every backend reports.
pub async fn bench_exec(http: &reqwest::Client, sb: &Sb, argv: &[String], stdin: Option<Vec<u8>>, cwd: &str, timeout: u64) -> Result<Out> {
    let mut script = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
    if let Some(data) = stdin {
        let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
        upload(http, sb, &f, data, "root").await?;
        script = format!("{script} < {f}; c=$?; rm -f {f}; exit $c");
    }
    run(http, sb, &script, cwd, timeout, "root").await
}

/// The handle `app.daytona` keeps for a run's sandbox, and back: (id, toolbox) of `e2b:<id>` and `<token>@<domain>`.
pub fn to_handle(sb: &Sb) -> (String, String) {
    (format!("e2b:{}", sb.id), format!("{}@{}", sb.token, sb.domain))
}

pub fn from_handle(id: &str, toolbox: &str) -> Option<Sb> {
    let id = id.strip_prefix("e2b:")?;
    let (token, domain) = toolbox.split_once('@')?;
    Some(Sb { id: id.into(), token: token.into(), domain: domain.into() })
}

pub async fn bench_kill(http: &reqwest::Client, id: &str) -> Result<()> {
    let a = api()?;
    let (s, v) = call(http, &a, reqwest::Method::DELETE, &format!("/sandboxes/{id}"), None).await?;
    if s >= 300 && s != 404 {
        bail!("e2b kill {id}: {s}: {v}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_aliases_are_valid_and_stable() {
        let a = alias("alexgshaw/path-tracing-reverse:20251031");
        assert!(a.starts_with("egin-alexgshaw-path-tracing-reverse-20251031-"));
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert_eq!(a, alias("alexgshaw/path-tracing-reverse:20251031"));
        let sb = Sb { id: "abc".into(), token: "t0k".into(), domain: "e2b.app".into() };
        let (id, tb) = to_handle(&sb);
        assert_eq!(from_handle(&id, &tb).unwrap().id, "abc");
        assert!(from_handle("dayt-1", "x").is_none());
    }

    #[test]
    fn connect_frames_round_trip() {
        let mut stream = frame(&json!({"event": {"data": {"stdout": "aGk="}}}));
        stream.extend(frame(&json!({"event": {"end": {"exited": true}}})));
        let mut end = frame(&json!({}));
        end[0] = 2;
        stream.extend(end);
        let f = frames(&stream);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].1["event"]["data"]["stdout"], "aGk=");
        assert_eq!(f[1].1["event"]["end"].get("exitCode"), None, "a zero exit code is left out");
        assert_eq!(f[2].0, 2);
    }

    #[test]
    fn metadata_queries_are_encoded() {
        assert_eq!(urlencode("egin=abc 1"), "egin%3Dabc%201");
    }
}
