//! Daytona's v2 API for `egin bench --sandbox daytona` (`DAYTONA_API_KEY`). A task's environment/ is built
//! once into an image, by Daytona's own Docker build from the task's Dockerfile and its build context, and
//! every run of the task gets a sandbox of its own from that image: an x86 Firecracker microVM that boots in
//! seconds, so amd64-only task images (DeepSWE's) run as they are. A separate verifier's tests/ is built the
//! same way. Commands and files go through the sandbox's toolbox, as for the macOS sandboxes.
//!
//! Each sandbox is created with a lifetime that ends it even if egin never does (a crash), is deleted after an
//! idle hour (a live run touches it every few minutes, a dead one cannot), and an offline task's
//! sandbox is created with its network blocked, outside the VM where the agent cannot undo it.

use super::daytona_macos::{process, upload_file};
use crate::App;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

#[derive(Clone, Debug)]
pub struct Remote {
    pub id: String,
    pub name: String,
    pub toolbox: String,
    http: reqwest::Client,
    key: String,
    /// Set when Daytona could not give the run a sandbox and E2B did (`e2b_fallback`).
    e2b: Option<super::e2b::Sb>,
    /// Keeps the sandbox from idling out while any clone of this handle lives (see `create`).
    _beat: Option<std::sync::Arc<Beat>>,
}

/// How long a bench sandbox may go without a toolbox call before Daytona deletes it, and how often a live run
/// makes one. A run whose egin died (killed, out of memory, its host gone) stops calling, so its sandbox goes
/// within the hour instead of holding Daytona's quota until its lifetime ends, up to ~9 h on FrontierHarness.
/// An hour, not less: a busy org has refused toolbox calls (429) for many minutes at a time.
const IDLE_DELETE: u64 = 3600;
const BEAT: Duration = Duration::from_secs(300);

/// A task touching a sandbox every `BEAT`, stopped when dropped.
#[derive(Debug)]
pub struct Beat(tokio::task::JoinHandle<()>);

impl Drop for Beat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn beat(http: reqwest::Client, key: String, toolbox: String) -> Beat {
    Beat(tokio::spawn(async move {
        let body = json!({"kind": "exec", "shellCommand": "true", "cwd": "/", "timeoutMs": 10_000});
        loop {
            tokio::time::sleep(BEAT).await;
            // Any toolbox call resets the idle clock; a failed one is retried at the next beat.
            let _ = http.post(format!("{toolbox}/processes")).bearer_auth(&key).json(&body).timeout(Duration::from_secs(60)).send().await;
        }
    }))
}

/// A run's sandbox from E2B when Daytona cannot give one: a template of the task's base image (built once), a
/// sandbox from it. The caller replays the Dockerfile's steps, as for a local microVM.
#[allow(clippy::too_many_arguments)]
pub async fn e2b_fallback(app: &App, name: &str, image: &str, env: &[(String, String)], cpus: u8, offline: bool, lifetime: u64) -> Result<Remote> {
    let template = super::e2b::bench_template(app, image).await?;
    let sb = super::e2b::bench_create(app, name, &template, env, cpus, offline, lifetime).await?;
    let (id, toolbox) = super::e2b::to_handle(&sb);
    Ok(Remote { id, name: name.into(), toolbox, http: app.http.clone(), key: String::new(), e2b: Some(sb), _beat: None })
}

struct Api {
    base: String,
    key: String,
}

fn api() -> Result<Api> {
    let key = std::env::var("DAYTONA_API_KEY").context("egin bench --sandbox daytona needs DAYTONA_API_KEY")?;
    let base = std::env::var("DAYTONA_V2_API_URL").unwrap_or_else(|_| "https://api.daytona.com".into());
    Ok(Api { base: base.trim_end_matches('/').into(), key })
}

async fn call(http: &reqwest::Client, a: &Api, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
    let mut r = http.request(method, format!("{}{path}", a.base)).bearer_auth(&a.key).timeout(Duration::from_secs(120));
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(json!({ "raw": text }))))
}

/// A call that waits out Daytona's admission limits: OVERLOADED and RATE_LIMITED say when to retry, and
/// LIMIT_EXCEEDED (too many sandboxes or builds at once) frees up as other runs end.
async fn call_admitted(http: &reqwest::Client, a: &Api, method: reqwest::Method, path: &str, body: Value) -> Result<(u16, Value)> {
    for _ in 0..120 {
        let (s, v) = call(http, a, method.clone(), path, Some(body.clone())).await?;
        if s != 429 {
            return Ok((s, v));
        }
        let wait = v["retry_after_seconds"].as_u64().unwrap_or(30).clamp(5, 120);
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
    bail!("daytona {path}: still refused after waiting an hour (429)")
}

/// A tar of `dir`, the build context of its Dockerfile. Daytona builds with its built-in Dockerfile frontend
/// and refuses a `# syntax=` directive; that frontend already reads heredocs, the usual reason for one, so the
/// directive is left out of a copy of the context, and the task folder is not touched.
fn context_tar(dir: &Path) -> Result<Vec<u8>> {
    let tar = |d: &Path| -> Result<Vec<u8>> {
        let o = std::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-cf").arg("-").arg("-C").arg(d).arg(".").output()?;
        anyhow::ensure!(o.status.success(), "tar {}: {}", d.display(), String::from_utf8_lossy(&o.stderr));
        Ok(o.stdout)
    };
    let dockerfile = std::fs::read_to_string(dir.join("Dockerfile")).unwrap_or_default();
    let Some(stripped) = without_syntax_directive(&dockerfile) else { return tar(dir) };
    let copy = std::env::temp_dir().join(format!("egin-context-{}", crate::new_id()));
    let o = std::process::Command::new("cp").arg("-a").arg(dir).arg(&copy).output()?;
    anyhow::ensure!(o.status.success(), "copy {}: {}", dir.display(), String::from_utf8_lossy(&o.stderr));
    let out = std::fs::write(copy.join("Dockerfile"), stripped).map_err(anyhow::Error::from).and_then(|_| tar(&copy));
    let _ = std::fs::remove_dir_all(&copy);
    out
}

/// The Dockerfile without its `# syntax=` parser directive, or None when it has none. Directives are the
/// comment lines at the very top, before any other line.
fn without_syntax_directive(dockerfile: &str) -> Option<String> {
    let mut lines = dockerfile.lines().collect::<Vec<_>>();
    let top = lines.iter().take_while(|l| l.trim_start().starts_with('#')).count();
    let i = lines[..top].iter().position(|l| {
        let d = l.trim_start().trim_start_matches('#').trim_start().to_ascii_lowercase();
        d.strip_prefix("syntax").is_some_and(|r| r.trim_start().starts_with('='))
    })?;
    lines.remove(i);
    Some(lines.join("\n") + "\n")
}

#[cfg(test)]
#[test]
fn drops_only_the_syntax_directive() {
    let d = "# syntax=docker/dockerfile:1.6\n# a comment\nFROM python:3.11\nRUN echo hi\n";
    assert_eq!(without_syntax_directive(d).unwrap(), "# a comment\nFROM python:3.11\nRUN echo hi\n");
    assert_eq!(without_syntax_directive("# Syntax = docker/dockerfile:1\nFROM a\n").unwrap(), "FROM a\n");
    assert!(without_syntax_directive("FROM a\n# syntax=docker/dockerfile:1\n").is_none());
    assert!(without_syntax_directive("FROM a\n").is_none());
}

/// Build `dir`'s Dockerfile, with `dir` as its context, into an image, and wait until it is ready. Daytona
/// deduplicates builds, so the same context builds once however often it is asked for. The image is anonymous
/// and deleted after a day without a sandbox made from it.
pub async fn build_image(app: &App, dir: &Path) -> Result<String> {
    let a = api()?;
    let http = &app.http;
    let tar = context_tar(dir)?;
    anyhow::ensure!(tar.len() <= 1 << 30, "the build context {} is over Daytona's 1 GiB", dir.display());
    let sha = hex::encode(Sha256::digest(&tar));
    let (s, mut up) =
        call_admitted(http, &a, reqwest::Method::POST, "/build-contexts", json!({"bytes": tar.len(), "sha256": sha, "dockerfile": "Dockerfile"})).await?;
    if s >= 300 {
        bail!("daytona build context for {}: {s}: {up}", dir.display());
    }
    if up["state"] != "ready" {
        let part = up["part_size"].as_u64().context("daytona: build context has no part_size")? as usize;
        for p in up["parts"].as_array().into_iter().flatten() {
            let n = p["part_number"].as_u64().unwrap_or(1) as usize;
            let chunk = tar[(n - 1) * part..(n * part).min(tar.len())].to_vec();
            let url = p["url"].as_str().context("daytona: a part has no url")?;
            // A part URL is a credential of its own: no Authorization header.
            let r = http.put(url).header("content-type", "application/octet-stream").body(chunk).timeout(Duration::from_secs(600)).send().await?;
            if !r.status().is_success() {
                bail!("daytona build context part {n}: {}", r.status());
            }
        }
        let region = up["region_id"].clone();
        let (s, done) =
            call(http, &a, reqwest::Method::POST, &format!("/build-contexts/{}/complete", up["id"].as_str().unwrap_or("")), Some(json!({"region_id": region})))
                .await?;
        if s >= 300 {
            bail!("daytona build context for {}: {s}: {done}", dir.display());
        }
        up = done;
    }
    let body = json!({"context_upload_id": up["id"], "idle_expiry_seconds": 86_400, "cpu_arch": "amd64"});
    let (s, im) = call_admitted(http, &a, reqwest::Method::POST, "/images", body).await?;
    if s >= 300 {
        bail!("daytona image build for {}: {s}: {im}", dir.display());
    }
    ready(http, &a, im["id"].as_str().context("daytona: image has no id")?, &dir.display().to_string()).await
}

async fn ready(http: &reqwest::Client, a: &Api, id: &str, what: &str) -> Result<String> {
    for _ in 0..720 {
        let (_, v) = call(http, a, reqwest::Method::GET, &format!("/images/{id}"), None).await?;
        match v["status"].as_str().unwrap_or("") {
            "ready" => return Ok(id.into()),
            "failed" => bail!("daytona image for {what} failed: {}", v["error"]),
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
    bail!("daytona image for {what} was not ready in an hour")
}

/// Register a registry image (a Dockerfile's FROM) in the project, for a sandbox to boot from.
pub async fn pull_image(app: &App, reference: &str) -> Result<String> {
    let a = api()?;
    let body = json!({"source": {"type": "registry", "ref": reference}, "idle_expiry_seconds": 86_400, "cpu_arch": "amd64"});
    let (s, im) = call_admitted(&app.http, &a, reqwest::Method::POST, "/images", body).await?;
    if s >= 300 {
        bail!("daytona pull {reference}: {s}: {im}");
    }
    ready(&app.http, &a, im["id"].as_str().context("daytona: image has no id")?, reference).await
}

/// Capture a sandbox's disk as an image, for runs to boot from. Daytona's Docker builder refuses a build whose
/// layers add over 100,000 files (DeepSWE's Go and Node module caches do); replaying the Dockerfile in a sandbox
/// and capturing it has no such limit.
pub async fn capture(r: &Remote) -> Result<String> {
    let a = api()?;
    let (s, im) = call_admitted(&r.http, &a, reqwest::Method::POST, &format!("/sandboxes/{}/images", r.id), json!({"idle_expiry_seconds": 86_400})).await?;
    if s >= 300 {
        bail!("daytona capture of {}: {s}: {im}", r.name);
    }
    ready(&r.http, &a, im["id"].as_str().context("daytona: image has no id")?, &r.name).await
}

/// A sandbox of one run, from a built image, with the Dockerfile's `env` (a captured image keeps the disk but
/// not ENV). `lifetime` seconds after it starts Daytona deletes it, whether or not egin got to; `offline`
/// blocks its network from the start. `model` (a project secret and the router's host) is for a harness that runs
/// inside the sandbox: the secret is attached, so the sandbox holds only its placeholder, and an offline sandbox
/// may still reach the router's host.
#[allow(clippy::too_many_arguments)] // one call per sandbox, each argument a different fact about it
pub async fn create(
    app: &App,
    name: &str,
    image: &str,
    env: &[(String, String)],
    cpus: u8,
    memory_mb: u32,
    offline: bool,
    lifetime: u64,
    model: Option<(&str, &str)>,
) -> Result<Remote> {
    let a = api()?;
    let http = &app.http;
    let mut body = json!({
        "name": name,
        "image": image,
        "env": env.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>(),
        "labels": {"egin": "bench"},
        "cpu": cpus.max(1),
        "memory_mib": memory_mb.max(2048),
        // Room for what the agent installs on top of the image.
        "disk_mib": 20_480,
        // Deleted after an idle hour, which only an orphan reaches: a live run's handle touches it every few
        // minutes, since the model can think for minutes without a command, and the project default (stop, not
        // delete) stopped sandboxes mid-run (SANDBOX_NOT_RUNNING killed 28 FrontierHarness runs).
        "lifecycle_policy": {"timeout_seconds": lifetime, "on_timeout": "delete", "idle_expiry_seconds": IDLE_DELETE, "on_idle_timeout": "delete"},
        "wait_seconds": 0,
    });
    if let Some((secret, _)) = model {
        body["secrets"] = json!([secret]);
    }
    match (offline, model) {
        (true, Some((_, host))) => body["network_policy"] = json!({"allow_domains": [host]}),
        (true, None) => body["network_policy"] = json!({"block_all": true}),
        _ => {}
    }
    let (s, mut sb) = call_admitted(http, &a, reqwest::Method::POST, "/sandboxes", body).await?;
    if s >= 300 {
        bail!("daytona create {name}: {s}: {sb}");
    }
    let id = sb["id"].as_str().context("daytona: sandbox has no id")?.to_string();
    for _ in 0..600 {
        match sb["status"].as_str().unwrap_or("") {
            "running" => break,
            "error" | "failed" | "deleted" | "lost" => bail!("daytona sandbox {name}: {} {}", sb["status"], sb["error"]),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        sb = call(http, &a, reqwest::Method::GET, &format!("/sandboxes/{id}"), None).await?.1;
    }
    if sb["status"] != "running" {
        bail!("daytona sandbox {name} did not start (status {})", sb["status"]);
    }
    let toolbox = format!("{}/{id}", sb["toolbox_proxy_url"].as_str().context("daytona: no toolbox_proxy_url")?.trim_end_matches('/'));
    let b = beat(http.clone(), a.key.clone(), toolbox.clone());
    let r = Remote { id, name: name.into(), toolbox, http: http.clone(), key: a.key, e2b: None, _beat: Some(std::sync::Arc::new(b)) };
    // Daytona puts its own minimal git (no `submodule`) at /usr/local/bin, ahead of the image's. Use the image's
    // git when it is a full one, so Dockerfile steps, agents and verifiers see the toolchain the task was built for.
    let fix = "g=/usr/local/bin/git; [ -x /usr/bin/git ] && ! [ $g -ef /usr/bin/git ] \
               && /usr/bin/git submodule -h 2>&1 | grep -q 'usage: git submodule' && ln -sf /usr/bin/git $g; true";
    if let Err(e) = r.sh(fix, "/", None, 60).await {
        eprintln!("egin: daytona git fix in {}: {e:#}", r.name);
    }
    Ok(r)
}

impl Remote {
    /// Run a shell script: (exit code, combined output), 124 on a timeout. stdin goes in as a file.
    pub async fn sh(&self, script: &str, cwd: &str, stdin: Option<Vec<u8>>, timeout: u64) -> Result<(i32, String)> {
        if let Some(sb) = &self.e2b {
            return super::e2b::bench_sh(&self.http, sb, script, cwd, stdin, timeout).await.with_context(|| format!("in {}", self.name));
        }
        let mut script = script.to_string();
        if let Some(data) = stdin {
            let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
            upload_file(&self.http, &self.key, &self.toolbox, &f, data).await?;
            script = format!("( {script} ) < {f}; c=$?; rm -f {f}; exit $c");
        }
        let cmd = format!("sh -c {}", super::quote(&script));
        Ok(match run(&self.http, &self.id, &self.toolbox, &cmd, cwd, timeout).await.with_context(|| format!("in {}", self.name))? {
            Some((code, out, err)) => (code, out + &err),
            None => (124, format!("timed out after {timeout}s")),
        })
    }

    pub async fn delete(&self) -> Result<()> {
        if let Some(sb) = &self.e2b {
            return super::e2b::bench_kill(&self.http, &sb.id).await;
        }
        let a = api()?;
        let (s, v) = call(&self.http, &a, reqwest::Method::DELETE, &format!("/sandboxes/{}", self.id), None).await?;
        if s >= 300 && s != 404 {
            bail!("daytona delete {}: {s}: {v}", self.name);
        }
        Ok(())
    }
}

/// The agent's tools in a bench run (`sandbox = "daytona-v2"`): the run's sandbox, which bench made and
/// registered for the session in `app.daytona`.
pub async fn exec(app: &App, agent: &crate::agent::Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<super::Out> {
    let (id, toolbox) = app.daytona.lock().await.get(sid).cloned().with_context(|| format!("no Daytona sandbox for session {sid}"))?;
    if let Some(sb) = super::e2b::from_handle(&id, &toolbox) {
        return super::e2b::bench_exec(&app.http, &sb, argv, stdin, &super::workdir(agent), timeout).await;
    }
    let a = api()?;
    let r = Remote { id, name: super::vm_name(sid), toolbox, http: app.http.clone(), key: a.key, e2b: None, _beat: None };
    let script = argv.iter().map(|s| super::quote(s)).collect::<Vec<_>>().join(" ");
    let mut script = script;
    if let Some(data) = stdin {
        let f = format!("/tmp/.egin-stdin-{}", crate::new_id());
        upload_file(&r.http, &r.key, &r.toolbox, &f, data).await?;
        script = format!("{script} < {f}; c=$?; rm -f {f}; exit $c");
    }
    let cmd = format!("sh -c {}", super::quote(&script));
    Ok(match run(&r.http, &r.id, &r.toolbox, &cmd, &super::workdir(agent), timeout).await? {
        Some((code, out, err)) => super::render(code, out.as_bytes(), err.as_bytes()),
        None => super::timed_out(timeout),
    })
}

/// A command in a bench sandbox. When Daytona has stopped the sandbox (409 SANDBOX_NOT_RUNNING) or its proxy
/// lost it (502 "sandbox not available"), start it again and send the command once more: the files are on its
/// disk, and a command that never reached it cannot have run twice.
async fn run(http: &reqwest::Client, id: &str, toolbox: &str, cmd: &str, cwd: &str, timeout: u64) -> Result<Option<(i32, String, String)>> {
    let a = api()?;
    match process(http, &a.key, toolbox, cmd, cwd, timeout).await {
        Err(e) if format!("{e:#}").contains("SANDBOX_NOT_RUNNING") || format!("{e:#}").contains("not available") => {
            eprintln!("egin: daytona sandbox {id} stopped ({e:#}); starting it again");
            let _ = call(http, &a, reqwest::Method::POST, &format!("/sandboxes/{id}/start"), Some(json!({}))).await;
            for _ in 0..90 {
                let (_, sb) = call(http, &a, reqwest::Method::GET, &format!("/sandboxes/{id}"), None).await?;
                match sb["status"].as_str().unwrap_or("") {
                    "running" => break,
                    "error" | "failed" | "deleted" | "lost" => bail!("daytona sandbox {id} is {}: {e:#}", sb["status"]),
                    _ => tokio::time::sleep(Duration::from_secs(2)).await,
                }
            }
            process(http, &a.key, toolbox, cmd, cwd, timeout).await
        }
        other => other,
    }
}
