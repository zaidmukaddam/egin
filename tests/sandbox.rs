//! Smoke test for the microsandbox backend against real microVMs (HAR-19c).
//! Run it after every microsandbox upgrade. Skips on hosts without hardware virtualization.

use egin::{agent::Agent, sandbox, tools, App, Approve};
use std::sync::Arc;

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

async fn setup(overrides: &str) -> (Arc<App>, Arc<Agent>, String) {
    let dir = std::env::temp_dir().join(format!("egin-vm-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), format!("sandbox = \"microsandbox\"\nimage = \"python:3.13-slim\"\n{overrides}")).unwrap();
    let agent = Arc::new(Agent::load(&dir, &toml::Table::new()).unwrap());
    let app = App::with(&egin::state_db(&dir), Approve::Auto, "key", "http://127.0.0.1:9", "http://127.0.0.1:9").unwrap();
    (app, agent, format!("smoke{}", egin::new_id()))
}

async fn sh(app: &App, agent: &Agent, sid: &str, script: &str, timeout: u64) -> sandbox::Out {
    sandbox::exec(app, agent, sid, &args(&["sh", "-c", script]), None, timeout).await
}

/// Tools on the host see its real environment, so `secrets` (a placeholder in the VM, the real value only
/// on requests to one host) can't be kept from them: such an agent must not load.
#[test]
fn local_sandbox_refuses_secrets() {
    let dir = std::env::temp_dir().join(format!("egin-secrets-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "sandbox = \"local\"\nsecrets = [{ name = \"GITHUB_TOKEN\", host = \"api.github.com\" }]\n").unwrap();
    let err = Agent::load(&dir, &toml::Table::new()).expect_err("an agent with secrets on the host must not load");
    assert!(format!("{err:#}").contains("secrets"), "{err:#}");
    std::fs::write(dir.join("agent.toml"), "sandbox = \"local\"\n").unwrap();
    assert!(Agent::load(&dir, &toml::Table::new()).is_ok(), "without secrets the local sandbox loads");
}

const FETCH: &str = "python3 -c \"import sys,urllib.request; print(urllib.request.urlopen(sys.argv[1], timeout=5).status)\"";

#[tokio::test]
async fn microsandbox_backend_end_to_end() {
    if !sandbox::vm_supported() {
        eprintln!("skipped: no hardware virtualization");
        return;
    }
    let (app, agent, sid) = setup("").await;

    // Commands run in a Linux guest, in /workspace, which is the host workspace.
    let o = sh(&app, &agent, &sid, "uname -s; pwd; echo from-guest > g.txt", 30).await;
    assert!(o.ok, "{}", o.text);
    assert_eq!(o.text, "Linux\n/workspace\n");
    assert_eq!(std::fs::read_to_string(agent.workspace.join("g.txt")).unwrap(), "from-guest\n");
    tools::write(&app, &agent, &sid, "deep/dir/f.txt", b"x").await.unwrap();
    assert_eq!(tools::read(&app, &agent, &sid, "deep/dir/f.txt").await.unwrap(), "x");

    // Custom tools run in the guest from the read-only mount.
    let o = sandbox::exec(&app, &agent, &sid, &args(&["/egin/tools/word_count.sh"]), Some(br#"{"text":"a b c"}"#.to_vec()), 30).await;
    assert_eq!(o.text, "3\n");

    // Default egress is open.
    let o = sh(&app, &agent, &sid, &format!("{FETCH} http://example.com"), 30).await;
    assert!(o.ok && o.text.trim() == "200", "egress should be open: {}", o.text);

    // A timeout kills the command and what it started.
    let o = sh(&app, &agent, &sid, "sleep 40 & sleep 40", 2).await;
    assert_eq!(o.text, "error: timed out after 2s");
    let o = sh(&app, &agent, &sid, "ps -eo args 2>/dev/null | grep -c '^sleep 40' || true", 10).await;
    eprintln!("sleeps left after timeout: {}", o.text.trim());

    // A slow command moves to the background and keeps running after the call returns.
    let call = serde_json::json!({"command": "sleep 3; echo late > bg.txt", "timeout_secs": 1});
    let out = tools::execute(&app, &agent, &sid, "bash", &call).await;
    assert!(out.contains("[still running in the background"), "{out}");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert_eq!(std::fs::read_to_string(agent.workspace.join("bg.txt")).unwrap_or_default(), "late\n", "the job outlived its exec");
    let pid = out.split("process group ").nth(1).unwrap().split(' ').next().unwrap();
    let call = serde_json::json!({"command": format!("sleep 30 & kill -- -{pid} 2>&1; echo checked")});
    assert!(tools::execute(&app, &agent, &sid, "bash", &call).await.contains("checked"));

    // A stopped VM (idle timeout) is restarted on the next call; files persist.
    let vm = app.vms.lock().await.get(&sid).cloned().unwrap();
    vm.stop().await.unwrap();
    let o = sh(&app, &agent, &sid, "cat g.txt", 60).await;
    assert_eq!(o.text, "from-guest\n", "reconnect after stop");

    // Removal deletes the persisted VM.
    sandbox::remove(&app, "microsandbox", &sid).await;
    assert!(microsandbox::Sandbox::get(&sandbox::vm_name(&sid)).await.is_err());
}

#[tokio::test]
async fn egress_allowlist_and_secrets() {
    if !sandbox::vm_supported() {
        return;
    }
    std::env::set_var("EGIN_SMOKE_SECRET", "real-secret-value");
    let (app, agent, sid) = setup("network = [\"example.com\"]\nsecrets = [{ name = \"EGIN_SMOKE_SECRET\", host = \"httpbin.org\" }]\n").await;

    let o = sh(&app, &agent, &sid, &format!("{FETCH} http://example.com"), 30).await;
    assert_eq!(o.text.trim(), "200", "allowed domain: {}", o.text);
    let o = sh(&app, &agent, &sid, &format!("{FETCH} http://www.wikipedia.org"), 30).await;
    assert!(!o.ok, "domain off the list must be blocked: {}", o.text);

    // The guest only ever sees a placeholder.
    let o = sh(&app, &agent, &sid, "printf %s \"$EGIN_SMOKE_SECRET\"", 30).await;
    assert!(!o.text.contains("real-secret-value") && !o.text.is_empty(), "guest saw: {}", o.text);
    // The real value is substituted on the way to the allowed host.
    let echo = "python3 -c \"import os,urllib.request; r=urllib.request.Request('https://httpbin.org/headers', headers={'X-Probe': os.environ['EGIN_SMOKE_SECRET']}); print(urllib.request.urlopen(r, timeout=10).read().decode())\"";
    let o = sh(&app, &agent, &sid, echo, 30).await;
    eprintln!("httpbin saw: {}", o.text);
    assert!(o.text.contains("real-secret-value"), "secret not substituted for the allowed host: {}", o.text);
    sandbox::remove(&app, "microsandbox", &sid).await;
}

/// The same checks for each cloud/alternative backend: exec in the working directory with exit
/// codes, the seeded workspace, stdin (write_file and custom tools), and removal.
async fn backend_basics(kind: &str) {
    let dir = std::env::temp_dir().join(format!("egin-{kind}-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("workspace/seed.txt"), "from host\n").unwrap();
    std::fs::write(dir.join("agent.toml"), format!("sandbox = \"{kind}\"\nimage = \"python:3.13-slim\"\nnetwork = [\"*\"]\n")).unwrap();
    let agent = Arc::new(Agent::load(&dir, &toml::Table::new()).unwrap());
    let app = App::with(&egin::state_db(&dir), Approve::Auto, "key", "http://127.0.0.1:9", "http://127.0.0.1:9").unwrap();
    let sid = format!("smoke{}", egin::new_id());

    let o = sh(&app, &agent, &sid, "pwd; cat seed.txt; exit 3", 120).await;
    assert_eq!(o.text, format!("{}\nfrom host\n\n[exit 3]", sandbox::workdir(&agent)), "{kind}");
    tools::write(&app, &agent, &sid, "out/a b.txt", b"it's here").await.unwrap();
    assert_eq!(tools::read(&app, &agent, &sid, "out/a b.txt").await.unwrap(), "it's here");
    // The agent's custom tool, called the way the loop calls it.
    let out = tools::execute(&app, &agent, &sid, "word_count", &serde_json::json!({"text": "a b c"})).await;
    assert_eq!(out.trim(), "3", "{kind} custom tool");
    sandbox::remove(&app, kind, &sid).await;
}

#[tokio::test]
async fn daytona_backend() {
    egin::load_env();
    if std::env::var("DAYTONA_API_KEY").is_err() {
        eprintln!("skipped: no DAYTONA_API_KEY");
        return;
    }
    backend_basics("daytona").await;
}

/// A macOS sandbox runs on a Mac reserved for 24 hours (bought on first use when none is held), so
/// only with EGIN_TEST_DAYTONA_MACOS=1.
#[tokio::test]
async fn daytona_macos_backend() {
    egin::load_env();
    if std::env::var("EGIN_TEST_DAYTONA_MACOS").is_err() {
        eprintln!("skipped: set EGIN_TEST_DAYTONA_MACOS=1 (reserves a Mac)");
        return;
    }
    backend_basics("daytona-macos").await;
}

#[tokio::test]
async fn e2b_backend() {
    egin::load_env();
    if std::env::var("E2B_API_KEY").is_err() {
        eprintln!("skipped: no E2B_API_KEY");
        return;
    }
    backend_basics("e2b").await;
}

/// Needs VERCEL_TOKEN with VERCEL_TEAM_ID and VERCEL_PROJECT_ID, or VERCEL_OIDC_TOKEN.
#[tokio::test]
async fn vercel_backend() {
    egin::load_env();
    let token = std::env::var("VERCEL_TOKEN").is_ok() && std::env::var("VERCEL_PROJECT_ID").is_ok();
    if !token && std::env::var("VERCEL_OIDC_TOKEN").is_err() {
        eprintln!("skipped: no Vercel credentials");
        return;
    }
    backend_basics("vercel").await;
}

#[tokio::test]
async fn docker_sandboxes_backend() {
    let signed_in = std::process::Command::new("sbx").args(["--no-input", "ls"]).output().is_ok_and(|o| o.status.success());
    if !signed_in {
        eprintln!("skipped: sbx missing or not signed in");
        return;
    }
    backend_basics("docker").await;

    // `network = []` denies egress.
    let dir = std::env::temp_dir().join(format!("egin-docker-net-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "sandbox = \"docker\"\nnetwork = []\n").unwrap();
    let agent = Arc::new(Agent::load(&dir, &toml::Table::new()).unwrap());
    let app = App::with(&egin::state_db(&dir), Approve::Auto, "key", "http://127.0.0.1:9", "http://127.0.0.1:9").unwrap();
    let sid = format!("smoke{}", egin::new_id());
    let o = sh(&app, &agent, &sid, "curl -s -m 5 -o /dev/null -w '%{http_code}' https://example.com || echo blocked", 60).await;
    assert!(!o.text.contains("200"), "egress should be denied: {}", o.text);
    sandbox::remove(&app, "docker", &sid).await;
}

/// Billed per second on Docker's cloud, so only with EGIN_TEST_DOCKER_CLOUD=1.
#[tokio::test]
async fn docker_cloud_backend() {
    if std::env::var("EGIN_TEST_DOCKER_CLOUD").is_err() {
        eprintln!("skipped: set EGIN_TEST_DOCKER_CLOUD=1 (billed)");
        return;
    }
    backend_basics("docker-cloud").await;
}
