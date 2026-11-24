//! Daytona's macOS sandboxes (`sandbox = "daytona-macos"`, `DAYTONA_API_KEY`): a macOS VM on a
//! dedicated Apple silicon Mac, for agents that build and test iOS and Mac apps. The base image
//! (`base-macos`) has Xcode and its iPhone simulator, Homebrew, git, Node.js and Python.
//!
//! macOS sandboxes run on Mac reservations, bought for 24 hours at a time. Without one, Daytona's
//! auto-reserve buys one on the first create, so the first tool call of a session can start a
//! billed reservation. A session's sandbox is created once (the create is idempotent on the
//! session id, so a restarted egin gets the same one back), seeded with workspace/ and tools/, and
//! deleted with the session.
//!
//! The API is Daytona's v2 one: `POST /sandboxes/macos` on the macOS API, then the toolbox's
//! `/processes` and `/files/upload` on the sandbox's `toolbox_proxy_url`.

use super::{quote, render, tar_gz, timed_out, vm_name, workdir, Out};
use crate::{agent::Agent, App};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const WORKDIR: &str = "/Users/lume/workspace";
/// The system volume is read-only on macOS, so the tools folder lives in the user's home.
pub const TOOLS: &str = "/Users/lume/.egin/tools";
/// Where Daytona has Macs (GET /mac-catalog lists them); `DAYTONA_MACOS_REGION` picks another.
fn region() -> String {
    std::env::var("DAYTONA_MACOS_REGION").unwrap_or_else(|_| "us-east-bos-1".into())
}

struct Api {
    base: String,
    key: String,
}

fn api() -> Result<Api> {
    let key = std::env::var("DAYTONA_API_KEY").context("sandbox = \"daytona-macos\" needs DAYTONA_API_KEY")?;
    let base = std::env::var("DAYTONA_MACOS_API_URL").unwrap_or_else(|_| "https://api.daytona.com".into());
    Ok(Api { base: base.trim_end_matches('/').into(), key })
}

async fn call(app: &App, a: &Api, method: reqwest::Method, url: &str, body: Option<Value>, key: Option<&str>) -> Result<(u16, Value)> {
    let mut r = app.http.request(method, url).bearer_auth(&a.key).timeout(Duration::from_secs(120));
    if let Some(k) = key {
        r = r.header("idempotency-key", k);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(json!({ "raw": text }))))
}

/// The image: `image` from agent.toml when it names a macOS image, else Daytona's base image.
fn image(agent: &Agent) -> String {
    let i = &agent.cfg.image;
    if i.is_empty() || i == &crate::agent::Config::default().image {
        "base-macos".into()
    } else {
        i.clone()
    }
}

/// The session's sandbox id and toolbox URL, created on first use.
async fn ensure(app: &App, agent: &Agent, sid: &str) -> Result<(String, String)> {
    let mut map = app.daytona.lock().await;
    if let Some(v) = map.get(sid) {
        return Ok(v.clone());
    }
    let a = api()?;
    let cfg = &agent.cfg;
    if !cfg.secrets.is_empty() {
        bail!("`secrets` (host-side substitution) needs sandbox = \"microsandbox\"; Daytona would put the value in the guest");
    }
    let mut body = json!({
        "image": image(agent),
        "region_id": region(),
        "name": vm_name(sid),
        "labels": {"egin": sid},
        // A Mac sandbox takes at least 4 cores and 4 GiB.
        "cores": cfg.cpus.max(4),
        "memory_mib": cfg.memory_mb.max(4096),
        "wait_seconds": 0,
    });
    let hosts = &cfg.network;
    if hosts.is_empty() {
        body["network_policy"] = json!({"block_all": true});
    } else if !hosts.iter().any(|h| h == "*") {
        let list: Vec<String> = hosts.iter().map(|h| h.strip_prefix('.').map(|s| format!("*.{s}")).unwrap_or(h.clone())).collect();
        body["network_policy"] = json!({"block_all": true, "allow_domains": list});
    }
    // The same key returns the same sandbox, and buys no second reservation, on a retry or a restart.
    let (s, mut sb) = call(app, &a, reqwest::Method::POST, &format!("{}/sandboxes/macos", a.base), Some(body), Some(&vm_name(sid))).await?;
    if s >= 300 {
        bail!("daytona macOS create: {s}: {sb}");
    }
    let id = sb["id"].as_str().context("daytona macOS: the sandbox has no id")?.to_string();
    // A Mac may take a few minutes: a reservation can wait for a free Mac, then macOS boots.
    for _ in 0..450 {
        match sb["status"].as_str().unwrap_or("") {
            "running" => break,
            "error" | "failed" | "deleted" | "deleting" => bail!("daytona macOS sandbox {}: {sb}", vm_name(sid)),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        sb = call(app, &a, reqwest::Method::GET, &format!("{}/sandboxes/{id}", a.base), None, None).await?.1;
    }
    if sb["status"] != "running" {
        bail!("daytona macOS sandbox {} did not start (status {})", vm_name(sid), sb["status"]);
    }
    let toolbox = format!("{}/{id}", sb["toolbox_proxy_url"].as_str().context("daytona macOS: no toolbox_proxy_url")?.trim_end_matches('/'));
    let fresh = run(app, &a, &toolbox, &format!("test -d {WORKDIR}"), "/", 30).await.map(|o| !o.ok).unwrap_or(true);
    map.insert(sid.to_string(), (id.clone(), toolbox.clone()));
    drop(map);
    if fresh {
        seed(app, &a, &toolbox, agent).await?;
    }
    Ok((id, toolbox))
}

async fn upload(app: &App, a: &Api, toolbox: &str, path: &str, bytes: Vec<u8>) -> Result<()> {
    upload_file(&app.http, &a.key, toolbox, path, bytes).await
}

/// Write `bytes` to `path` through a v2 sandbox toolbox (macOS here; Linux for `egin bench --sandbox daytona`).
pub(crate) async fn upload_file(http: &reqwest::Client, key: &str, toolbox: &str, path: &str, bytes: Vec<u8>) -> Result<()> {
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(bytes).file_name("f"));
    let r =
        http.post(format!("{toolbox}/files/upload")).query(&[("path", path)]).bearer_auth(key).multipart(form).timeout(Duration::from_secs(300)).send().await?;
    if !r.status().is_success() {
        bail!("daytona upload {path}: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    Ok(())
}

/// Run a shell command and wait for it. Output comes back as log frames, stdout before stderr.
async fn run(app: &App, a: &Api, toolbox: &str, command: &str, cwd: &str, timeout: u64) -> Result<Out> {
    Ok(match process(&app.http, &a.key, toolbox, command, cwd, timeout).await? {
        Some((code, out, err)) => render(code, out.as_bytes(), err.as_bytes()),
        None => timed_out(timeout),
    })
}

/// A shell command through a v2 sandbox toolbox, waited on to the end: (exit code, stdout, stderr), or None
/// when it ran past `timeout`.
pub(crate) async fn process(http: &reqwest::Client, key: &str, toolbox: &str, command: &str, cwd: &str, timeout: u64) -> Result<Option<(i32, String, String)>> {
    let body = json!({"kind": "exec", "shellCommand": command, "cwd": cwd, "timeoutMs": timeout * 1000});
    // The toolbox holds a request for at most 10 minutes; a longer command is waited on after.
    let wait = (timeout * 1000).min(600_000);
    // A 429 means the toolbox refused the command before starting it (the org is busy), so sending it again
    // can't run it twice. Without this wait a busy org scored the whole task as failed.
    let (mut pause, mut waited) = (Duration::from_secs(5), Duration::ZERO);
    let (status, mut p) = loop {
        let r = http
            .post(format!("{toolbox}/processes"))
            .query(&[("waitMs", wait.to_string()), ("logs", "text".into())])
            .bearer_auth(key)
            .json(&body)
            .timeout(Duration::from_millis(wait + 60_000))
            .send()
            .await?;
        let status = r.status().as_u16();
        let p: Value = r.json().await.unwrap_or_default();
        if status != 429 || waited >= Duration::from_secs(900) {
            break (status, p);
        }
        tokio::time::sleep(pause).await;
        waited += pause;
        pause = (pause * 2).min(Duration::from_secs(60));
    };
    if status >= 300 {
        bail!("daytona exec: {status}: {p}");
    }
    let id = p["id"].as_str().unwrap_or("").to_string();
    while p["state"] != "terminal" {
        let auth = |r: reqwest::RequestBuilder| r.bearer_auth(key).timeout(Duration::from_secs(120));
        auth(http.get(format!("{toolbox}/processes/{id}/wait")).query(&[("timeoutMs", "60000")])).send().await?;
        p = auth(http.get(format!("{toolbox}/processes/{id}"))).send().await?.json().await?;
    }
    if p["reason"] == "timed_out" {
        return Ok(None);
    }
    // Long output: the first page came attached only when the run ended within the wait.
    let logs = if p["logs"]["eof"] == true {
        p["logs"].clone()
    } else {
        http.get(format!("{toolbox}/processes/{id}/logs")).query(&[("limit", "5000")]).bearer_auth(key).send().await?.json().await?
    };
    let (mut out, mut err) = (String::new(), String::new());
    for f in logs["frames"].as_array().into_iter().flatten() {
        let data = f["data"].as_str().unwrap_or("");
        if f["channel"] == "stderr" {
            err += data;
        } else {
            out += data;
        }
    }
    Ok(Some((p["exitCode"].as_i64().unwrap_or(-1) as i32, out, err)))
}

/// Copy the agent's workspace/ and tools/ into a new sandbox.
async fn seed(app: &App, a: &Api, toolbox: &str, agent: &Agent) -> Result<()> {
    for (dir, dst) in [(agent.workspace.clone(), WORKDIR), (agent.dir.join("tools"), TOOLS)] {
        if !dir.is_dir() {
            continue;
        }
        let tmp = format!("/tmp/egin-seed-{}.tgz", crate::new_id());
        upload(app, a, toolbox, &tmp, tar_gz(&dir)?).await?;
        let o = run(app, a, toolbox, &format!("mkdir -p {dst} && tar -xzf {tmp} -C {dst} && rm -f {tmp}"), "/", 300).await?;
        if !o.ok {
            bail!("seeding {dst}: {}", o.text);
        }
    }
    Ok(())
}

pub async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let a = api()?;
    let (_, toolbox) = ensure(app, agent, sid).await?;
    let mut script = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
    if let Some(data) = stdin {
        let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
        upload(app, &a, &toolbox, &f, data).await?;
        script = format!("{script} < {f}; c=$?; rm -f {f}; exit $c");
    }
    run(app, &a, &toolbox, &script, &workdir(agent), timeout).await
}

/// Delete the session's sandbox, if it has one. The Mac reservation itself runs out with its period.
pub async fn remove(app: &App, sid: &str) {
    let Some((id, _)) = app.daytona.lock().await.remove(sid) else { return };
    if let Ok(a) = api() {
        let _ = call(app, &a, reqwest::Method::DELETE, &format!("{}/sandboxes/{id}", a.base), None, None).await;
    }
}
