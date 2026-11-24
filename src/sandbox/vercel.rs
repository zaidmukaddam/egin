//! Vercel Sandbox as a cloud sandbox backend (`sandbox = "vercel"`).
//!
//! Auth is a Vercel access token (`VERCEL_TOKEN`, with `VERCEL_TEAM_ID` and `VERCEL_PROJECT_ID`) or
//! a Vercel OIDC token (`VERCEL_OIDC_TOKEN`), whose claims carry the team and project. One sandbox
//! per session, named `egin-<session>` so a restarted egin can resume it, created on the first tool
//! call from `VERCEL_SANDBOX_IMAGE` (default Vercel's universal image: Ubuntu with Node, Bun and
//! Python), seeded with the agent's workspace/ and tools/, and deleted with the session. Sandboxes
//! are created non-persistent, so nothing is snapshotted when they stop.
//!
//! Commands run with `wait: true`: the reply is newline-delimited JSON, the command record first,
//! then its log lines, then the record again with the exit code.

use super::{open_or_closed, quote, render, tar_gz, timed_out, vm_name, workdir, Out};
use crate::{agent::Agent, App};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

pub const WORKDIR: &str = "/vercel/sandbox/workspace";
const BASE: &str = "https://api.vercel.com";

struct Api {
    token: String,
    team: String,
    project: String,
}

fn api() -> Result<Api> {
    let env = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
    if let Some(token) = env("VERCEL_TOKEN") {
        let team = env("VERCEL_TEAM_ID").context("sandbox = \"vercel\" with VERCEL_TOKEN also needs VERCEL_TEAM_ID")?;
        let project = env("VERCEL_PROJECT_ID").context("sandbox = \"vercel\" with VERCEL_TOKEN also needs VERCEL_PROJECT_ID")?;
        return Ok(Api { token, team, project });
    }
    let token = env("VERCEL_OIDC_TOKEN").context("sandbox = \"vercel\" needs VERCEL_TOKEN (with VERCEL_TEAM_ID and VERCEL_PROJECT_ID) or VERCEL_OIDC_TOKEN")?;
    // The OIDC token is a JWT whose claims name the team (owner_id) and project (project_id).
    let claims: Value = token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .context("VERCEL_OIDC_TOKEN is not a JWT")?;
    let team = claims["owner_id"].as_str().context("VERCEL_OIDC_TOKEN has no owner_id")?.to_string();
    let project = claims["project_id"].as_str().context("VERCEL_OIDC_TOKEN has no project_id")?.to_string();
    Ok(Api { token, team, project })
}

async fn call(app: &App, a: &Api, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
    let mut r = app.http.request(method, format!("{BASE}{path}")).query(&[("teamId", &a.team)]).bearer_auth(&a.token).timeout(Duration::from_secs(120));
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(json!({ "raw": text }))))
}

fn key(sid: &str) -> String {
    format!("vercel:{sid}")
}

/// The id of the sandbox's current session: remembered, resumed by name, or a new sandbox.
async fn ensure(app: &App, agent: &Agent, sid: &str) -> Result<String> {
    let mut map = app.sandboxes.lock().await;
    if let Some(v) = map.get(&key(sid)) {
        return Ok(v["session"].as_str().unwrap_or("").to_string());
    }
    let a = api()?;
    let name = vm_name(sid);
    let (status, v) = call(app, &a, reqwest::Method::GET, &format!("/v2/sandboxes/{name}?projectId={}&resume=true", a.project), None).await?;
    let (session, created) = if status < 300 {
        // Session ids change on every resume.
        (v["session"]["id"].as_str().context("vercel: no session id")?.to_string(), false)
    } else if status == 404 {
        if !agent.cfg.secrets.is_empty() {
            bail!("`secrets` (host-side substitution) needs sandbox = \"microsandbox\"; Vercel Sandbox would put the value in the guest");
        }
        if !open_or_closed(agent, "vercel")? {
            bail!("sandbox = \"vercel\" cannot run with network = []; use network = [\"*\"] or sandbox = \"microsandbox\"");
        }
        // One vCPU, or an even number: each comes with 2 GB of memory.
        let vcpus = match agent.cfg.cpus.max(1) {
            1 => 1,
            n => n + n % 2,
        };
        let mut body = json!({
            "projectId": a.project,
            "name": name,
            "resources": {"vcpus": vcpus},
            "timeout": agent.cfg.idle_timeout.max(300) * 1000,
            "persistent": false,
            "tags": {"egin": sid},
        });
        if let Ok(image) = std::env::var("VERCEL_SANDBOX_IMAGE") {
            body["image"] = json!(image);
        }
        if let Ok(region) = std::env::var("VERCEL_SANDBOX_REGION") {
            body["region"] = json!(region);
        }
        let (s, v) = call(app, &a, reqwest::Method::POST, "/v3/sandboxes", Some(body)).await?;
        if s >= 300 {
            bail!("vercel sandbox create: {s}: {v}");
        }
        (v["session"]["id"].as_str().context("vercel: no session id")?.to_string(), true)
    } else {
        bail!("vercel sandbox get: {status}: {v}");
    };
    map.insert(key(sid), json!({"name": name, "session": session}));
    drop(map);
    if created {
        seed(app, &a, &session, agent).await?;
    }
    Ok(session)
}

/// Unpack a gzipped tar into `dir` (which must exist).
async fn write_tar(app: &App, a: &Api, session: &str, dir: &str, tgz: Vec<u8>) -> Result<()> {
    let r = app
        .http
        .post(format!("{BASE}/v2/sandboxes/sessions/{session}/fs/write"))
        .query(&[("teamId", &a.team)])
        .bearer_auth(&a.token)
        .header("Content-Type", "application/gzip")
        .header("x-cwd", dir)
        .body(tgz)
        .timeout(Duration::from_secs(300))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("vercel sandbox write to {dir}: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    Ok(())
}

/// Parse a `wait: true` command reply: log lines and the final command record.
fn outcome(ndjson: &str) -> Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    let (mut code, mut stdout, mut stderr) = (None, vec![], vec![]);
    for line in ndjson.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)?;
        if let Some(c) = v["command"]["exitCode"].as_i64() {
            code = Some(c as i32);
        }
        match v["stream"].as_str() {
            Some("stdout") => stdout.extend(v["data"].as_str().unwrap_or("").bytes()),
            Some("stderr") => stderr.extend(v["data"].as_str().unwrap_or("").bytes()),
            Some("error") => bail!("vercel sandbox command: {}", v["data"]),
            _ => {}
        }
    }
    Ok((code, stdout, stderr))
}

async fn run(app: &App, a: &Api, session: &str, command: &str, cwd: &str, timeout: u64) -> Result<Out> {
    let body = json!({"command": "bash", "args": ["-lc", command], "cwd": cwd, "timeout": timeout * 1000, "wait": true, "logs": true});
    let r = app
        .http
        .post(format!("{BASE}/v2/sandboxes/sessions/{session}/cmd"))
        .query(&[("teamId", &a.team)])
        .bearer_auth(&a.token)
        .json(&body)
        .timeout(Duration::from_secs(timeout + 60))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("vercel sandbox command: {} {}", r.status(), r.text().await.unwrap_or_default());
    }
    let (code, stdout, stderr) = outcome(&r.text().await?)?;
    match code {
        // The sandbox kills a command at its timeout; it ends with no exit code.
        None => Ok(timed_out(timeout)),
        Some(c) => Ok(render(c, &stdout, &stderr)),
    }
}

/// Copy the agent's workspace/ and tools/ into a new sandbox.
async fn seed(app: &App, a: &Api, session: &str, agent: &Agent) -> Result<()> {
    for (dir, dst) in [(agent.workspace.clone(), WORKDIR.to_string()), (agent.dir.join("tools"), "/egin/tools".to_string())] {
        if !dir.is_dir() {
            continue;
        }
        let o = run(app, a, session, &format!("sudo mkdir -p {dst} && sudo chown $(id -u) {dst}"), "/", 60).await?;
        if !o.ok {
            bail!("creating {dst}: {}", o.text);
        }
        write_tar(app, a, session, &dst, tar_gz(&dir)?).await?;
    }
    Ok(())
}

pub async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let a = api()?;
    let mut retried = false;
    loop {
        let session = ensure(app, agent, sid).await?;
        let _ =
            call(app, &a, reqwest::Method::POST, &format!("/v2/sandboxes/sessions/{session}/extend-timeout"), Some(json!({"duration": timeout * 1000}))).await;
        let mut script = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        if let Some(data) = &stdin {
            // stdin arrives as a one-file tar, unpacked into /tmp.
            let name = format!(".egin-stdin-{}", crate::new_id());
            let dir = std::env::temp_dir().join(format!("egin-{}", crate::new_id()));
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(&name), data)?;
            let tgz = tar_gz(&dir);
            let _ = std::fs::remove_dir_all(&dir);
            write_tar(app, &a, &session, "/tmp", tgz?).await?;
            script = format!("{script} < /tmp/{name}; c=$?; rm -f /tmp/{name}; exit $c");
        }
        match run(app, &a, &session, &script, &workdir(agent), timeout).await {
            Ok(o) => return Ok(o),
            // The session may have stopped: forget it, resume it, retry once.
            Err(_) if !retried => {
                app.sandboxes.lock().await.remove(&key(sid));
                retried = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Delete the session's sandbox, if it has one.
pub async fn remove(app: &App, sid: &str) {
    app.sandboxes.lock().await.remove(&key(sid));
    if let Ok(a) = api() {
        let path = format!("/v2/sandboxes/{}?projectId={}&deleteOrphanSnapshots=true", vm_name(sid), a.project);
        let _ = call(app, &a, reqwest::Method::DELETE, &path, None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waited_command_reply_gives_output_and_exit_code() {
        let reply = r#"{"command":{"id":"c1","exitCode":null}}
{"stream":"stdout","data":"hello\n"}
{"stream":"stderr","data":"warn\n"}
{"command":{"id":"c1","exitCode":3,"durationMs":12}}
"#;
        let (code, out, err) = outcome(reply).unwrap();
        assert_eq!((code, out.as_slice(), err.as_slice()), (Some(3), b"hello\n".as_slice(), b"warn\n".as_slice()));
        assert!(outcome("{\"stream\":\"error\",\"data\":{\"code\":\"x\"}}").is_err());
    }
}
