//! Daytona as a cloud sandbox backend (`sandbox = "daytona"`, `DAYTONA_API_KEY`).
//!
//! One Daytona sandbox per session, named `egin-<session>` and labelled `egin=<session>`. It is
//! created on the first tool call (from `image`, built by Daytona and cached as a snapshot), seeded
//! with the agent's workspace/ and tools/, auto-stopped after `idle_timeout`, restarted on the
//! next call, and deleted with the session. Files live in the cloud sandbox: unlike microsandbox
//! there is no bind mount, so the host workspace is copied up once and not synced back.
//!
//! The toolbox exec API takes a command string and no stdin, so argv is shell-quoted and stdin is
//! uploaded as a file and redirected.

use super::{render, timed_out, vm_name, workdir, Out};
use crate::{agent::Agent, App};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const WORKDIR: &str = "/home/daytona/workspace";

struct Api {
    base: String,
    key: String,
}

fn api() -> Result<Api> {
    let key = std::env::var("DAYTONA_API_KEY").context("sandbox = \"daytona\" needs DAYTONA_API_KEY")?;
    let base = std::env::var("DAYTONA_API_URL").unwrap_or_else(|_| "https://app.daytona.io/api".into());
    Ok(Api { base: base.trim_end_matches('/').into(), key })
}

fn name(sid: &str) -> String {
    vm_name(sid)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn call(app: &App, a: &Api, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
    let mut r = app.http.request(method, format!("{}{path}", a.base)).bearer_auth(&a.key).timeout(Duration::from_secs(120));
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(json!({ "raw": text }))))
}

/// Daytona's Dockerfile for the agent's image, plus the network and resource settings.
fn create_body(agent: &Agent, sid: &str) -> Result<Value> {
    let cfg = &agent.cfg;
    if !cfg.secrets.is_empty() {
        bail!("`secrets` (host-side substitution) needs sandbox = \"microsandbox\"; Daytona would put the value in the guest");
    }
    let mut body = json!({
        "name": name(sid),
        "labels": {"egin": sid},
        "buildInfo": {"dockerfileContent": format!("FROM {}\nRUN mkdir -p {WORKDIR} /egin/tools\n", cfg.image)},
        "cpu": cfg.cpus,
        "memory": (cfg.memory_mb as u64).div_ceil(1024).max(1),
        "autoStopInterval": cfg.idle_timeout.div_ceil(60).max(1),
    });
    let hosts = &cfg.network;
    if hosts.is_empty() {
        body["networkBlockAll"] = json!(true);
    } else if !hosts.iter().any(|h| h == "*") {
        let list: Vec<String> = hosts.iter().map(|h| h.strip_prefix('.').map(|s| format!("*.{s}")).unwrap_or(h.clone())).collect();
        body["domainAllowList"] = json!(list.join(","));
    }
    Ok(body)
}

/// The session's sandbox id and toolbox URL: created on first use, started if it stopped.
async fn ensure(app: &App, agent: &Agent, sid: &str) -> Result<(String, String)> {
    // ponytail: one lock across creates, like the microsandbox path.
    let mut map = app.daytona.lock().await;
    if let Some(v) = map.get(sid) {
        return Ok(v.clone());
    }
    let a = api()?;
    let (status, mut sb) = call(app, &a, reqwest::Method::GET, &format!("/sandbox/{}", name(sid)), None).await?;
    let created = status == 404;
    if created {
        let (s, v) = call(app, &a, reqwest::Method::POST, "/sandbox", Some(create_body(agent, sid)?)).await?;
        if s >= 300 {
            bail!("daytona create: {s}: {v}");
        }
        sb = v;
    } else if status >= 300 {
        bail!("daytona get: {status}: {sb}");
    }
    let id = sb["id"].as_str().context("daytona: sandbox has no id")?.to_string();
    // Wait until it runs (a new one may still be building its image); a stopped one is started once.
    let mut start_sent = false;
    for _ in 0..300 {
        match sb["state"].as_str().unwrap_or("") {
            "started" => break,
            "stopped" | "archived" if !start_sent => {
                call(app, &a, reqwest::Method::POST, &format!("/sandbox/{id}/start"), None).await?;
                start_sent = true;
            }
            "error" | "build_failed" | "destroyed" => bail!("daytona sandbox {}: {} {}", name(sid), sb["state"], sb["errorReason"]),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        sb = call(app, &a, reqwest::Method::GET, &format!("/sandbox/{id}"), None).await?.1;
    }
    if sb["state"] != "started" {
        bail!("daytona sandbox {} did not start (state {})", name(sid), sb["state"]);
    }
    let toolbox = format!("{}/{id}", sb["toolboxProxyUrl"].as_str().unwrap_or("https://proxy.app.daytona.io/toolbox").trim_end_matches('/'));
    map.insert(sid.to_string(), (id.clone(), toolbox.clone()));
    drop(map);
    if created {
        seed(app, &a, &toolbox, agent).await?;
    }
    Ok((id, toolbox))
}

async fn upload(app: &App, a: &Api, toolbox: &str, path: &str, bytes: Vec<u8>) -> Result<()> {
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(bytes).file_name("f"));
    let r = app
        .http
        .post(format!("{toolbox}/files/upload"))
        .query(&[("path", path)])
        .bearer_auth(&a.key)
        .multipart(form)
        .timeout(Duration::from_secs(300))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("daytona upload {path}: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    Ok(())
}

async fn run(app: &App, a: &Api, toolbox: &str, command: &str, cwd: &str, timeout: u64) -> Result<Out> {
    let body = json!({"command": command, "cwd": cwd, "timeout": timeout});
    let r = app.http.post(format!("{toolbox}/process/execute")).bearer_auth(&a.key).json(&body).timeout(Duration::from_secs(timeout + 60)).send().await?;
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or_default();
    if status == 408 {
        return Ok(timed_out(timeout));
    }
    if status >= 300 {
        bail!("daytona execute: {status}: {v}");
    }
    let code = v["exitCode"].as_i64().unwrap_or(-1) as i32;
    // The toolbox returns stdout and stderr together.
    Ok(render(code, v["result"].as_str().unwrap_or("").as_bytes(), b""))
}

/// Copy the agent's workspace/ and tools/ into a new sandbox.
async fn seed(app: &App, a: &Api, toolbox: &str, agent: &Agent) -> Result<()> {
    for (dir, dst) in [(agent.workspace.clone(), WORKDIR.to_string()), (agent.dir.join("tools"), "/egin/tools".to_string())] {
        if !dir.is_dir() {
            continue;
        }
        let tar = std::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg("-").arg("-C").arg(&dir).arg(".").output()?;
        if !tar.status.success() {
            bail!("tar {}: {}", dir.display(), String::from_utf8_lossy(&tar.stderr));
        }
        let tmp = format!("/tmp/egin-seed-{}.tgz", crate::new_id());
        upload(app, a, toolbox, &tmp, tar.stdout).await?;
        let o = run(app, a, toolbox, &format!("sh -c {}", quote(&format!("mkdir -p {dst} && tar -xzf {tmp} -C {dst} && rm -f {tmp}"))), "/", 300).await?;
        if !o.ok {
            bail!("seeding {dst}: {}", o.text);
        }
    }
    Ok(())
}

pub async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let a = api()?;
    let mut retried = false;
    loop {
        let (_, toolbox) = ensure(app, agent, sid).await?;
        let mut script = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        if let Some(data) = &stdin {
            let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
            upload(app, &a, &toolbox, &f, data.clone()).await?;
            script = format!("{script} < {f}; c=$?; rm -f {f}; exit $c");
        }
        match run(app, &a, &toolbox, &format!("sh -c {}", quote(&script)), &workdir(agent), timeout).await {
            Ok(o) => return Ok(o),
            // The sandbox may have auto-stopped: forget it, start it again, retry once.
            Err(_) if !retried => {
                app.daytona.lock().await.remove(sid);
                retried = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Delete the session's sandbox, if it has one.
pub async fn remove(app: &App, sid: &str) {
    app.daytona.lock().await.remove(sid);
    if let Ok(a) = api() {
        let _ = call(app, &a, reqwest::Method::DELETE, &format!("/sandbox/{}", name(sid)), None).await;
    }
}
