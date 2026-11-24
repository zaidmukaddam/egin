//! microsandbox backend: one local microVM per session (`egin-<session>`), the workspace
//! bind-mounted at /workspace and the agent's tools/ read-only at /egin/tools. Created on first
//! use, restarted after its idle timeout, replaced if a crash left it half-made.

use super::{render, timed_out, vm_name, workdir, Out};
use crate::{agent::Agent, App};
use anyhow::{bail, Result};
use microsandbox::{NetworkPolicy, Sandbox};
use std::time::Duration;

pub const WORKDIR: &str = "/workspace";

/// Hardware virtualization microsandbox can use on this host.
pub fn vm_supported() -> bool {
    (cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")) || std::path::Path::new("/dev/kvm").exists()
}

pub(super) async fn exec(app: &App, agent: &Agent, sid: &str, argv: &[String], stdin: Option<Vec<u8>>, timeout: u64) -> Result<Out> {
    let cwd = workdir(agent);
    let mut retried = false;
    loop {
        let vm = vm(app, agent, sid).await?;
        let (args, data, cwd) = (argv[1..].to_vec(), stdin.clone(), cwd.clone());
        let r = vm
            .exec_with(argv[0].clone(), move |mut e| {
                e = e.args(args).cwd(cwd).timeout(Duration::from_secs(timeout));
                match data {
                    Some(d) => e.stdin_bytes(d),
                    None => e.stdin_null(),
                }
            })
            .await;
        match r {
            Ok(o) => return Ok(render(o.status().code, o.stdout_bytes(), o.stderr_bytes())),
            Err(microsandbox::MicrosandboxError::ExecTimeout(_)) => return Ok(timed_out(timeout)),
            // The VM may have stopped on its idle timeout: reconnect once.
            Err(_) if !retried => {
                app.vms.lock().await.remove(sid);
                retried = true;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The session's microVM: created on first use, restarted if it stopped, reconnected after a crash.
async fn vm(app: &App, agent: &Agent, sid: &str) -> Result<Sandbox> {
    // ponytail: one lock across boots, so concurrent sessions start their VMs one at a time.
    let mut vms = app.vms.lock().await;
    if let Some(vm) = vms.get(sid) {
        return Ok(vm.clone());
    }
    let vm = match builder(agent, sid)?.connect_or_create().await {
        Ok(vm) => vm,
        // A crash during boot or cleanup leaves a half-made VM that cannot be reconnected.
        // Replace it: the workspace is a bind mount, so only guest-side state is lost.
        Err(e) => {
            eprintln!("egin: replacing VM {}: {e}", vm_name(sid));
            builder(agent, sid)?.replace().create().await?
        }
    };
    vms.insert(sid.to_string(), vm.clone());
    Ok(vm)
}

fn builder(agent: &Agent, sid: &str) -> Result<microsandbox::sandbox::SandboxBuilder> {
    let cfg = &agent.cfg;
    let mut b = Sandbox::builder(vm_name(sid))
        .image(cfg.image.as_str())
        .cpus(cfg.cpus)
        .memory(cfg.memory_mb)
        .idle_timeout(cfg.idle_timeout)
        .workdir(workdir(agent))
        .volume(WORKDIR, |m| m.bind(&agent.workspace));
    let tools = agent.dir.join("tools");
    if tools.is_dir() {
        b = b.volume("/egin/tools", |m| m.bind(tools).readonly());
    }
    let mut hosts: Vec<String> = cfg.network.clone();
    for s in &cfg.secrets {
        let Ok(value) = std::env::var(&s.name) else { bail!("secret {} is not set in the host environment", s.name) };
        b = b.secret_env(&s.name, value, &s.host);
        hosts.push(s.host.clone());
    }
    // A domain allowlist needs TLS interception for HTTPS. microsandbox's strict hostname policy (on by default) lets
    // an allowed name through only when it can read the request's real host, which plain TLS hides, so every HTTPS
    // call to an allowed domain was refused. Interception keeps that check (it is what stops domain fronting) and
    // microsandbox points the guest's clients at its CA: SSL_CERT_FILE, REQUESTS_CA_BUNDLE, NODE_EXTRA_CA_CERTS.
    let domains = !hosts.is_empty() && !hosts.iter().any(|h| h == "*");
    Ok(b.network(|n| {
        let n = n.policy(policy(&hosts));
        if domains {
            n.tls(|t| t)
        } else {
            n
        }
    }))
}

/// `[]` denies all egress, `["*"]` allows it, otherwise only the listed domains (".x.com" matches subdomains).
fn policy(hosts: &[String]) -> NetworkPolicy {
    if hosts.is_empty() {
        return NetworkPolicy::none();
    }
    if hosts.iter().any(|h| h == "*") {
        return NetworkPolicy::allow_all();
    }
    let hosts = hosts.to_vec();
    NetworkPolicy::builder()
        .default_deny()
        .egress(move |r| {
            for h in &hosts {
                match h.strip_prefix('.') {
                    Some(suffix) => r.allow().domain_suffix(suffix),
                    None => r.allow().domain(h.as_str()),
                };
            }
            r
        })
        .build()
        .unwrap_or_else(|_| NetworkPolicy::none())
}

pub(super) async fn remove(app: &App, sid: &str) {
    if let Some(vm) = app.vms.lock().await.remove(sid) {
        let _ = vm.stop().await;
    }
    let _ = Sandbox::remove(&vm_name(sid)).await;
}
