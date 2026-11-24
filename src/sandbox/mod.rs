//! Where tool commands run. Every tool goes through [`exec`] (argv, optional stdin, timeout),
//! and the agent's `sandbox` setting picks the backend:
//! - `microsandbox`: one local microVM per session, the workspace bind-mounted ([`microvm`])
//! - `daytona`: one Daytona cloud sandbox per session ([`daytona`])
//! - `daytona-macos`: one Daytona macOS sandbox per session, on Apple silicon ([`daytona_macos`])
//! - `docker` / `docker-cloud`: Docker Sandboxes through the `sbx` CLI ([`docker`])
//! - `e2b`: one E2B sandbox per session ([`e2b`])
//! - `vercel`: one Vercel Sandbox per session ([`vercel`])
//! - `local`: on the host, approval-gated, killed as a whole process group on timeout

pub mod daytona;
pub mod daytona_bench;
pub mod daytona_macos;
pub mod docker;
pub mod e2b;
pub mod microvm;
pub mod vercel;

use crate::{agent::Agent, App};
use anyhow::Result;
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

pub use microvm::vm_supported;

/// A finished command: `text` is stdout then stderr, plus `[exit N]` when it failed.
pub struct Out {
    pub ok: bool,
    pub text: String,
}

/// The sandbox name for a session, shared by every backend.
pub fn vm_name(sid: &str) -> String {
    format!("egin-{sid}")
}

/// The working directory commands see.
pub fn workdir(agent: &Agent) -> String {
    if let Some(w) = &agent.cfg.workdir {
        return w.clone();
    }
    match agent.cfg.sandbox.as_str() {
        "microsandbox" => microvm::WORKDIR.into(),
        "daytona" => daytona::WORKDIR.into(),
        "daytona-macos" => daytona_macos::WORKDIR.into(),
        "e2b" => e2b::WORKDIR.into(),
        "vercel" => vercel::WORKDIR.into(),
        "docker-cloud" => docker::CLOUD_WORKDIR.into(),
        _ => agent.workspace.to_string_lossy().into(),
    }
}

/// Where the agent's tools/ folder is inside the sandbox; None where tools run from the folder itself.
pub fn tools_dir(agent: &Agent) -> Option<&'static str> {
    match agent.cfg.sandbox.as_str() {
        "daytona-macos" => Some(daytona_macos::TOOLS),
        "microsandbox" | "daytona" | "daytona-v2" | "docker-cloud" | "e2b" | "vercel" => Some("/egin/tools"),
        _ => None,
    }
}

/// Run argv in the session's sandbox. Backend failures come back as an `error: …` result
/// for the model, never as a panic.
pub async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Out {
    // A helper works in its parent's sandbox, on the same files.
    let owner = app.sandbox_of.lock().unwrap().get(sid).cloned();
    let sid = owner.as_deref().unwrap_or(sid);
    let r = match agent.cfg.sandbox.as_str() {
        "microsandbox" => microvm::exec(app, agent, sid, argv, stdin, timeout).await,
        "daytona" => daytona::exec(app, agent, sid, argv, stdin, timeout).await,
        // A bench run's sandbox, which `egin bench --sandbox daytona` made and owns.
        "daytona-v2" => daytona_bench::exec(app, agent, sid, argv, stdin, timeout).await,
        "daytona-macos" => daytona_macos::exec(app, agent, sid, argv, stdin, timeout).await,
        "e2b" => e2b::exec(app, agent, sid, argv, stdin, timeout).await,
        "vercel" => vercel::exec(app, agent, sid, argv, stdin, timeout).await,
        "docker" | "docker-cloud" => docker::exec(agent, sid, argv, stdin, timeout).await,
        _ => host(&argv[0], &argv[1..], &workdir(agent), stdin, timeout).await,
    };
    r.unwrap_or_else(|e| Out { ok: false, text: format!("error: {e:#}") })
}

/// Stop and remove a session's sandbox (session deleted, or an eval run finished).
pub async fn remove(app: &App, kind: &str, sid: &str) {
    match kind {
        "daytona" => daytona::remove(app, sid).await,
        // egin bench deletes its own sandboxes when a run ends.
        "daytona-v2" => {}
        "daytona-macos" => daytona_macos::remove(app, sid).await,
        "e2b" => e2b::remove(app, sid).await,
        "vercel" => vercel::remove(app, sid).await,
        "docker" | "docker-cloud" => docker::remove(kind, sid).await,
        _ => microvm::remove(app, sid).await,
    }
}

pub(crate) fn render(code: i32, stdout: &[u8], stderr: &[u8]) -> Out {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    if code != 0 {
        text.push_str(&format!("\n[exit {code}]"));
    }
    Out { ok: code == 0, text }
}

pub(crate) fn timed_out(timeout: u64) -> Out {
    Out { ok: false, text: format!("error: timed out after {timeout}s") }
}

/// Every credential egin reads. Commands run on the host get an environment without them.
pub const CREDENTIALS: &[&str] = &[
    "YDC_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "AI_GATEWAY_API_KEY",
    "VERCEL_OIDC_TOKEN",
    "VERCEL_TOKEN",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "AWS_BEDROCK_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "BRAVE_API_KEY",
    "PARALLEL_API_KEY",
    "FIRECRAWL_API_KEY",
    "CONTEXT_DEV_API_KEY",
    "DAYTONA_API_KEY",
    "E2B_API_KEY",
    "EGIN_TOKEN",
];

/// Shell-quote one argument.
pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A gzipped tar of a directory's contents, made with the host's tar.
pub(crate) fn tar_gz(dir: &std::path::Path) -> Result<Vec<u8>> {
    let o = std::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg("-").arg("-C").arg(dir).arg(".").output()?;
    if !o.status.success() {
        anyhow::bail!("tar {}: {}", dir.display(), String::from_utf8_lossy(&o.stderr));
    }
    Ok(o.stdout)
}

/// The cloud backends cannot limit egress to a host list, so a list is refused rather than ignored.
pub(crate) fn open_or_closed(agent: &Agent, backend: &str) -> Result<bool> {
    let hosts = &agent.cfg.network;
    if hosts.is_empty() {
        return Ok(false);
    }
    if hosts.iter().any(|h| h == "*") {
        return Ok(true);
    }
    anyhow::bail!("sandbox = \"{backend}\" cannot limit network access to a list of hosts; use network = [\"*\"] or [], or sandbox = \"microsandbox\"")
}

/// Run a command on the host, in its own process group so a timeout kills everything it started.
pub async fn host(cmd: &str, args: &[String], cwd: &str, stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let mut c = Command::new(cmd);
    c.args(args).current_dir(cwd).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    c.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    // Commands the model writes must not see egin's own credentials.
    for key in CREDENTIALS {
        c.env_remove(key);
    }
    #[cfg(unix)]
    c.process_group(0);
    let mut child = c.spawn()?;
    let pid = child.id();
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(&data).await;
    }
    match tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output()).await {
        Ok(o) => {
            let o = o?;
            Ok(render(o.status.code().unwrap_or(-1), &o.stdout, &o.stderr))
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // SAFETY: plain syscall; a negative pid targets the process group.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
            Ok(timed_out(timeout))
        }
    }
}
