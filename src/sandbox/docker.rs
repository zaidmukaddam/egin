//! Docker Sandboxes as a backend, through the `sbx` CLI (`brew install docker/tap/sbx`, then
//! `sbx login`):
//!   sandbox = "docker"        a local Docker microVM; the workspace is bind-mounted
//!   sandbox = "docker-cloud"  Docker's cloud sandboxes; the workspace is copied up once
//! One sandbox per session, `egin-<session>`, created on the first tool call: `sbx exec` exits
//! 125 when the sandbox does not exist, which triggers a create and one retry.

use super::{host, vm_name, workdir, Out};
use crate::agent::Agent;
use anyhow::{bail, Result};

pub const CLOUD_WORKDIR: &str = "/workspace";

fn cloud(agent: &Agent) -> bool {
    agent.cfg.sandbox == "docker-cloud"
}

fn base(agent: &Agent) -> Vec<String> {
    let mut v = vec!["--no-input".to_string()];
    if cloud(agent) {
        v.insert(0, "--cloud".into());
    }
    v
}

async fn sbx(args: Vec<String>, stdin: Option<Vec<u8>>, timeout: u64, cwd: &str) -> Result<Out> {
    host("sbx", &args, cwd, stdin, timeout).await
}

/// Docker's cloud bills fixed shapes; take the smallest that covers the request.
fn cloud_shape(cpus: u8, memory_mb: u32) -> (u8, u32) {
    const SHAPES: [(u8, u32); 5] = [(1, 2048), (2, 4096), (4, 8192), (8, 16384), (16, 32768)];
    SHAPES.into_iter().find(|&(c, m)| c >= cpus && m >= memory_mb).unwrap_or(SHAPES[4])
}

async fn create(agent: &Agent, sid: &str) -> Result<()> {
    let cfg = &agent.cfg;
    if !cfg.secrets.is_empty() {
        bail!("`secrets` needs sandbox = \"microsandbox\"; use `sbx secret` for Docker Sandboxes");
    }
    let name = vm_name(sid);
    let mut a = base(agent);
    a.extend(["create".into(), "shell".into()]);
    if !cloud(agent) {
        // Mounted at their host paths, so the agent's tools run from where tools.rs expects them.
        a.push(agent.workspace.to_string_lossy().into());
        if agent.dir.join("tools").is_dir() {
            a.push(agent.dir.join("tools").to_string_lossy().into());
        }
    }
    let (cpus, mem) = if cloud(agent) { cloud_shape(cfg.cpus, cfg.memory_mb) } else { (cfg.cpus, cfg.memory_mb.max(512)) };
    a.extend(["--name".into(), name.clone(), "--no-progress".into(), "--cpus".into(), cpus.to_string(), "--memory".into(), format!("{mem}m")]);
    let hosts = &cfg.network;
    if cloud(agent) {
        // Cloud egress is an allowlist; nothing listed means none beyond the account default.
        for h in hosts {
            // sbx patterns: "**" is every host, "*.example.com" its subdomains.
            a.extend(["--allow-network".into(), if h == "*" { "**".into() } else { h.strip_prefix('.').map(|s| format!("*.{s}")).unwrap_or(h.clone()) }]);
        }
    } else if hosts.is_empty() {
        a.extend(["--deny-network".into(), "**".into()]);
    }
    let o = sbx(a, None, 600, "/").await?;
    if !o.ok {
        bail!("sbx create {name}: {}", o.text);
    }
    if !cloud(agent) && !hosts.is_empty() && !hosts.iter().any(|h| h == "*") {
        let o = sbx(
            vec!["--no-input".into(), "policy".into(), "allow".into(), "network".into(), "--sandbox".into(), name.clone(), hosts.join(",")],
            None,
            120,
            "/",
        )
        .await?;
        if !o.ok {
            bail!("sbx policy allow {name}: {}", o.text);
        }
    }
    if cloud(agent) {
        for (dir, dst) in [(agent.workspace.clone(), CLOUD_WORKDIR.to_string()), (agent.dir.join("tools"), "/egin/tools".to_string())] {
            if dir.is_dir() {
                let mut a = base(agent);
                a.extend(["cp".into(), dir.to_string_lossy().into(), format!("{name}:{dst}")]);
                let o = sbx(a, None, 600, "/").await?;
                if !o.ok {
                    bail!("sbx cp {}: {}", dir.display(), o.text);
                }
            }
        }
        // Cloud copies drop permissions; tools must stay executable.
        let mut a = base(agent);
        a.extend(["exec".into(), name, "sh".into(), "-c".into(), "chmod +x /egin/tools/* 2>/dev/null; true".into()]);
        sbx(a, None, 120, "/").await?;
    }
    Ok(())
}

pub(super) async fn exec(agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let name = vm_name(sid);
    let mut created = false;
    loop {
        let mut a = base(agent);
        a.push("exec".into());
        if stdin.is_some() {
            a.push("-i".into());
        }
        a.extend(["-w".into(), workdir(agent), name.clone()]);
        a.extend(argv.iter().cloned());
        let o = sbx(a, stdin.clone(), timeout, "/").await?;
        // 125: sbx could not run the command, e.g. the sandbox does not exist yet.
        if !created && o.text.ends_with("[exit 125]") {
            create(agent, sid).await?;
            created = true;
            continue;
        }
        return Ok(o);
    }
}

pub(super) async fn remove(agent_sandbox: &str, sid: &str) {
    let mut a = vec!["--no-input".to_string()];
    if agent_sandbox == "docker-cloud" {
        a.insert(0, "--cloud".into());
    }
    a.extend(["rm".into(), "-f".into(), vm_name(sid)]);
    let _ = sbx(a, None, 120, "/").await;
}
