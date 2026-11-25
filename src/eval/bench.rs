//! Terminal-Bench 2 adapter (`egin bench`). The official task images are amd64-only, so each
//! task's environment is rebuilt as an arm64 microVM: boot the Dockerfile's (multi-arch) base
//! image and replay its WORKDIR / COPY / RUN steps inside the VM. The built environment is forked
//! for every run, the agent works in the fork, then the task's tests/ are copied in and
//! tests/test.sh decides the reward, as Harbor does. Results are relative (egin vs baseline on
//! the same rebuilt environments), not comparable to the amd64 leaderboard.

use crate::sandbox::daytona_bench;
use crate::{
    agent::{run, Agent},
    eval::{save_trace, Outcome},
    App,
};
use anyhow::{anyhow, bail, Context, Result};
use microsandbox::{NetworkPolicy, Sandbox};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
enum Step {
    Workdir(String),
    Copy { srcs: Vec<String>, dst: String },
    Run(String),
}

/// An environment to build: a base image, the Dockerfile steps to replay, the directory COPY reads from.
#[derive(Debug, Clone)]
struct Env {
    image: String,
    steps: Vec<Step>,
    env: Vec<(String, String)>,
    workdir: String,
    cpus: u8,
    memory_mb: u32,
    context: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TbTask {
    pub name: String,
    pub dir: PathBuf,
    pub difficulty: String,
    environment: Env,
    pub workdir: String,
    pub agent_timeout: u64,
    verifier_timeout: u64,
    /// Harbor's separate verifier (`[verifier] environment_mode = "separate"`, as DeepSWE uses): the
    /// tests run in their own environment built from tests/Dockerfile, never in the one the agent
    /// touched, and see only the `artifacts` the `collect` commands saved.
    verifier: Option<Env>,
    collect: Vec<(String, u64)>,
    artifacts: Vec<Artifact>,
    /// `[agent] network_mode = "no-network"`.
    offline: bool,
}

/// One entry of a task's `artifacts`: a path, or `{ source, exclude, service }`.
#[derive(Debug, Clone, PartialEq)]
struct Artifact {
    path: String,
    /// Patterns left out when `path` is a directory, as tar's `--exclude` matches them.
    exclude: Vec<String>,
    /// The service container the path lives in. egin bench runs none, so these are not copied.
    service: Option<String>,
}

/// Join Dockerfile continuation lines and drop comments. A BuildKit heredoc (`RUN python - <<'EOF'` …
/// `EOF`) keeps its body lines as they are, newlines and `#` lines included, as part of its instruction.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut lines = text.lines();
    while let Some(raw) = lines.next() {
        let line = raw.trim_end();
        if cur.is_empty() && line.trim_start().starts_with('#') {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(l) => {
                cur.push_str(l);
                cur.push(' ');
            }
            None => {
                cur.push_str(line);
                for (delim, strip_tabs) in heredocs(&cur) {
                    for body in lines.by_ref() {
                        cur.push('\n');
                        cur.push_str(body);
                        let end = if strip_tabs { body.trim_start_matches('\t') } else { body };
                        if end.trim_end() == delim {
                            break;
                        }
                    }
                }
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
        }
    }
    out
}

/// The heredocs a line opens, in order: `<<EOF`, `<<-EOF` (strips leading tabs), `<<'EOF'`, `<<"EOF"`.
/// The name must start with a letter or `_`, so a shift like `1<<2` is not one; `<<<` is a here-string.
fn heredocs(line: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    let mut rest = line;
    while let Some(i) = rest.find("<<") {
        rest = &rest[i + 2..];
        if let Some(r) = rest.strip_prefix('<') {
            rest = r;
            continue;
        }
        let strip_tabs = rest.starts_with('-');
        let r = rest.trim_start_matches('-').trim_start().trim_start_matches(['\'', '"']);
        let name: String = r.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            out.push((name, strip_tabs));
        }
    }
    out
}

/// `$VAR` / `${VAR}` against variables seen so far (unset expands to "").
fn subst(s: &str, env: &[(String, String)]) -> String {
    let mut out = s.to_string();
    for (k, v) in env.iter().rev() {
        out = out.replace(&format!("${{{k}}}"), v).replace(&format!("${k}"), v);
    }
    // Remaining references to unset variables.
    let mut cleaned = String::new();
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|n| n.is_ascii_uppercase() || *n == '{' || *n == '_') {
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next();
            }
            while chars.peek().is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_') {
                chars.next();
            }
            if braced {
                chars.next();
            }
        } else {
            cleaned.push(c);
        }
    }
    cleaned
}

fn json_or_words(rest: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(rest).unwrap_or_else(|_| rest.split_whitespace().map(String::from).collect())
}

/// What a Dockerfile builds: base image, the steps to replay, final env and workdir.
struct Image {
    from: String,
    steps: Vec<Step>,
    env: Vec<(String, String)>,
    workdir: String,
}

fn parse_dockerfile(text: &str) -> Result<Image> {
    let mut image = None;
    // Images set PATH; `ENV PATH=/x:$PATH` must extend it, not replace it.
    let (mut steps, mut env) = (vec![], vec![("PATH".to_string(), "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string())]);
    let mut workdir = "/".to_string();
    for line in logical_lines(text) {
        let (op, rest) = line.split_once(char::is_whitespace).unwrap_or((&line, ""));
        let rest = rest.trim();
        match op.to_ascii_uppercase().as_str() {
            "FROM" => {
                if image.is_some() {
                    bail!("multi-stage Dockerfiles are not supported");
                }
                image = rest.split_whitespace().find(|w| !w.starts_with("--")).map(String::from);
            }
            "WORKDIR" => {
                let w = subst(rest, &env);
                workdir = if w.starts_with('/') { w } else { format!("{}/{w}", workdir.trim_end_matches('/')) };
                steps.push(Step::Workdir(workdir.clone()));
            }
            "ENV" | "ARG" => {
                let pairs: Vec<(String, String)> = if rest.contains('=') {
                    rest.split_whitespace().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string())).collect()
                } else {
                    let (k, v) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                    vec![(k.to_string(), v.trim().to_string())]
                };
                for (k, v) in pairs {
                    let v = subst(&v, &env);
                    env.push((k, v));
                }
            }
            "COPY" | "ADD" => {
                let words: Vec<String> = json_or_words(rest).into_iter().filter(|w| !w.starts_with("--chown") && !w.starts_with("--chmod")).collect();
                if words.iter().any(|w| w.starts_with("--")) {
                    bail!("COPY {rest}: flags are not supported");
                }
                let (dst, srcs) = words.split_last().ok_or_else(|| anyhow!("COPY without arguments"))?;
                steps.push(Step::Copy { srcs: srcs.to_vec(), dst: dst.clone() });
            }
            "RUN" => steps.push(Step::Run(match serde_json::from_str::<Vec<String>>(rest) {
                Ok(argv) => argv.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" "),
                // `RUN <<EOF` with no command: BuildKit runs the heredoc's body as the script.
                Err(_) if rest.starts_with("<<") => format!("sh {rest}"),
                Err(_) => rest.to_string(),
            })),
            "CMD" | "ENTRYPOINT" | "EXPOSE" | "LABEL" | "USER" | "SHELL" | "VOLUME" | "HEALTHCHECK" | "STOPSIGNAL" => {}
            other => bail!("unknown Dockerfile instruction {other}"),
        }
    }
    Ok(Image { from: image.ok_or_else(|| anyhow!("no FROM"))?, steps, env, workdir })
}

fn secs(v: &toml::Value) -> Option<u64> {
    v.as_float().or(v.as_integer().map(|i| i as f64)).map(|s| s as u64)
}

/// An environment from a Dockerfile, sized by a `cpus` / `memory_mb` table.
fn env_from(dockerfile: &Path, size: Option<&toml::Value>) -> Result<Env> {
    let Image { from, steps, env, workdir } = parse_dockerfile(&std::fs::read_to_string(dockerfile).with_context(|| dockerfile.display().to_string())?)?;
    let get = |k: &str| size.and_then(|s| s.get(k)).and_then(|v| v.as_integer());
    Ok(Env {
        image: from,
        steps,
        env,
        workdir,
        cpus: get("cpus").unwrap_or(1).clamp(1, 4) as u8,
        memory_mb: get("memory_mb").unwrap_or(2048).max(2048) as u32,
        context: dockerfile.parent().unwrap().to_path_buf(),
    })
}

pub fn load_task(dir: &Path) -> Result<TbTask> {
    let t: toml::Table = toml::from_str(&std::fs::read_to_string(dir.join("task.toml"))?)?;
    let get = |sec: &str, k: &str| t.get(sec).and_then(|s| s.get(k)).cloned();
    if get("environment", "gpus").and_then(|g| g.as_integer()).unwrap_or(0) > 0 {
        bail!("needs a GPU");
    }
    let environment = env_from(&dir.join("environment/Dockerfile"), t.get("environment"))?;
    let verifier = match get("verifier", "environment_mode").as_ref().and_then(|v| v.as_str()) {
        Some("separate") => Some(env_from(&dir.join("tests/Dockerfile"), t.get("verifier").and_then(|v| v.get("environment")))?),
        Some(other) if other != "shared" => bail!("verifier environment_mode {other:?} is not supported"),
        _ => None,
    };
    let collect = t
        .get("verifier")
        .and_then(|v| v.get("collect"))
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c.get("command")?.as_str()?.to_string(), c.get("timeout_sec").and_then(secs).unwrap_or(300))))
        .collect();
    let strings = |v: Option<&toml::Value>| v.and_then(|v| v.as_array()).into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
    let artifacts = t
        .get("artifacts")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| match a.as_str() {
            Some(path) => Some(Artifact { path: path.into(), exclude: vec![], service: None }),
            None => Some(Artifact {
                path: a.get("source")?.as_str()?.into(),
                exclude: strings(a.get("exclude")),
                service: a.get("service").and_then(|s| s.as_str()).map(String::from),
            }),
        })
        .collect();
    Ok(TbTask {
        name: dir.file_name().unwrap().to_string_lossy().into(),
        dir: dir.to_path_buf(),
        difficulty: get("metadata", "difficulty").and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
        workdir: environment.workdir.clone(),
        environment,
        agent_timeout: get("agent", "timeout_sec").as_ref().and_then(secs).unwrap_or(900),
        verifier_timeout: get("verifier", "timeout_sec").as_ref().and_then(secs).unwrap_or(900),
        verifier,
        collect,
        artifacts,
        offline: get("agent", "network_mode").is_some_and(|m| m.as_str() == Some("no-network")),
    })
}

/// Where a task environment runs: a local microVM (`--sandbox microsandbox`, the default), or a Daytona
/// cloud sandbox (`--sandbox daytona`), which needs no virtualization on the host and runs amd64 images.
#[derive(Clone)]
pub enum Vm {
    Micro(Box<Sandbox>),
    Daytona(daytona_bench::Remote),
}

/// A task's images on Daytona: its environment, and its separate verifier's when it has one. Built once per
/// task; every run gets a sandbox of its own from them.
pub struct Images {
    env: String,
    verifier: Option<String>,
}

impl Vm {
    fn name(&self) -> &str {
        match self {
            Vm::Micro(s) => s.name(),
            Vm::Daytona(r) => &r.name,
        }
    }
}

/// Run a shell script in a VM. Returns (exit code, combined output); timeouts are code 124.
async fn sh(vm: &Vm, script: &str, cwd: &str, stdin: Option<Vec<u8>>, timeout: u64) -> Result<(i32, String)> {
    let vm = match vm {
        Vm::Micro(vm) => vm,
        Vm::Daytona(r) => return r.sh(script, cwd, stdin, timeout).await,
    };
    let (script, cwd) = (script.to_string(), cwd.to_string());
    let r = vm
        .exec_with("/bin/sh", move |e| {
            let e = e.args(["-c".to_string(), script]).cwd(cwd).timeout(Duration::from_secs(timeout));
            match stdin {
                Some(d) => e.stdin_bytes(d),
                None => e.stdin_null(),
            }
        })
        .await;
    match r {
        Ok(o) => Ok((o.status().code, format!("{}{}", String::from_utf8_lossy(o.stdout_bytes()), String::from_utf8_lossy(o.stderr_bytes())))),
        Err(microsandbox::MicrosandboxError::ExecTimeout(_)) => Ok((124, format!("timed out after {timeout}s"))),
        Err(e) => Err(e.into()),
    }
}

/// A tar stream of `dir` (or of one path inside it), for copying into a VM over stdin.
fn tar_of(dir: &Path, inner: &str) -> Result<Vec<u8>> {
    let o = std::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-cf").arg("-").arg("-C").arg(dir).arg(inner).output()?;
    anyhow::ensure!(o.status.success(), "tar {}/{inner}: {}", dir.display(), String::from_utf8_lossy(&o.stderr));
    Ok(o.stdout)
}

/// Copy a host directory's contents to `dst` in the VM.
async fn upload(vm: &Vm, dir: &Path, dst: &str) -> Result<()> {
    let (code, out) = sh(vm, &format!("mkdir -p '{dst}' && tar -xf - -C '{dst}'"), "/", Some(tar_of(dir, ".")?), 300).await?;
    anyhow::ensure!(code == 0, "upload to {dst}: {out}");
    Ok(())
}

/// Boot the base image and replay the Dockerfile. The result is the task's environment.
pub async fn build(task: &TbTask, name: &str, log: &mut String) -> Result<Vm> {
    build_env(&task.environment, name, log).await
}

/// A local microVM from the base image with the Dockerfile's steps replayed. (On Daytona the Dockerfile is
/// built as it is, into an image: `daytona_bench::build_image`.)
async fn build_env(spec: &Env, name: &str, log: &mut String) -> Result<Vm> {
    let mut b =
        Sandbox::builder(name).image(spec.image.as_str()).cpus(spec.cpus).memory(spec.memory_mb).network(|n| n.policy(NetworkPolicy::allow_all())).replace();
    for (k, v) in &spec.env {
        b = b.env(k, v);
    }
    let vm = Vm::Micro(Box::new(b.create().await.with_context(|| format!("booting {}", spec.image))?));
    // A half-built environment is removed here, so a failed step leaves no sandbox running (and billed).
    if let Err(e) = replay(&vm, spec, log).await {
        teardown(&vm).await;
        return Err(e);
    }
    Ok(vm)
}

/// The Daytona image of an environment: its Dockerfile built by Daytona as it is, or, when Daytona's builder
/// refuses it (a build whose layers add over 100,000 files, as DeepSWE's module caches do), the base image
/// booted with the steps replayed into it, as a local microVM is, and its disk captured.
async fn daytona_image(app: &App, spec: &Env, name: &str, log: &mut String) -> Result<String> {
    match daytona_bench::build_image(app, &spec.context).await {
        Ok(id) => Ok(id),
        // The builder caps the filesystem delta an image may export (entries, regular-file binds).
        Err(e) if format!("{e:#}").contains("filesystem delta") => {
            log.push_str(&format!("daytona's builder refused {}: replaying it in a sandbox\n", spec.context.display()));
            let base = daytona_bench::pull_image(app, &spec.image).await?;
            let sb = daytona_bench::create(app, name, &base, &spec.env, spec.cpus.max(2), spec.memory_mb.max(4096), false, 4 * 3600, None).await?;
            let vm = Vm::Daytona(sb);
            let built = async {
                // The sandbox has no ssh key, so npm's github: deps (ssh first) would fail; fetch them over https.
                sh(&vm, "git config --global url.https://github.com/.insteadOf ssh://git@github.com/ || true", "/", None, 60).await?;
                replay(&vm, spec, log).await?;
                let Vm::Daytona(sb) = &vm else { unreachable!() };
                daytona_bench::capture(sb).await
            }
            .await;
            teardown(&vm).await;
            built
        }
        Err(e) => Err(e),
    }
}

/// Replay the Dockerfile's WORKDIR / COPY / RUN steps into a booted VM.
async fn replay(vm: &Vm, spec: &Env, log: &mut String) -> Result<()> {
    let vm = vm.clone();
    let env_dir = &spec.context;
    let mut cwd = "/".to_string();
    for step in &spec.steps {
        match step {
            Step::Workdir(w) => {
                sh(&vm, &format!("mkdir -p '{w}'"), "/", None, 60).await?;
                cwd = w.clone();
            }
            Step::Copy { srcs, dst } => {
                for src in srcs {
                    let tar = tar_of(env_dir, src.trim_end_matches('/'))?;
                    let base = src.trim_end_matches('/').rsplit('/').next().unwrap_or(src);
                    // Docker semantics: a directory's contents land in dst; a file lands at dst,
                    // or inside it when dst is a directory (trailing slash, or several sources). Copied files
                    // belong to root, as Docker's are: tar as root keeps the host's uid, and git then refuses a
                    // repository owned by someone else ("dubious ownership", Terminal-Bench's fix-git).
                    let into_dir = dst.ends_with('/') || srcs.len() > 1;
                    let script = format!(
                        "set -e; t=$(mktemp -d); tar -xf - -C \"$t\"; chown -R 0:0 \"$t\"; s=\"$t/{src}\"; d='{dst}';
                         if [ -d \"$s\" ]; then mkdir -p \"$d\"; cp -a \"$s/.\" \"$d/\";
                         elif [ {into} = 1 ] || [ -d \"$d\" ]; then mkdir -p \"$d\"; cp -a \"$s\" \"$d/{base}\";
                         else mkdir -p \"$(dirname \"$d\")\"; cp -a \"$s\" \"$d\"; fi; rm -rf \"$t\"",
                        src = src.trim_end_matches('/'),
                        into = into_dir as u8,
                    );
                    let (code, out) = sh(&vm, &script, &cwd, Some(tar), 300).await?;
                    if code != 0 {
                        bail!("COPY {src} {dst}: {out}");
                    }
                }
            }
            Step::Run(cmd) => {
                // A multi-line step (a heredoc) goes in as a script file, so no backend's command
                // quoting can touch its newlines or quotes.
                let script = if cmd.contains('\n') {
                    use base64::Engine;
                    let f = format!("/tmp/.egin-run-{}.sh", crate::new_id());
                    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd);
                    format!("echo {b64} | base64 -d > {f} && sh {f}; c=$?; rm -f {f}; exit $c")
                } else {
                    cmd.clone()
                };
                let (code, out) = sh(&vm, &script, &cwd, None, 1800).await?;
                log.push_str(&format!("$ {cmd}\n{}\n", out.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>()));
                if code != 0 {
                    bail!("RUN failed ({code}): {cmd}\n{}", out.chars().rev().take(1500).collect::<String>().chars().rev().collect::<String>());
                }
            }
        }
    }
    Ok(())
}

/// Copy tests/ in, run test.sh, read the reward. The verifier dir and tests are wiped first: the
/// agent ran in this VM, and a reward.txt or test file it left behind must not count. A task with a
/// separate verifier is graded in an environment of its own instead (`verify_separate`).
async fn verify(app: &App, vm: &Vm, task: &TbTask, name: &str, images: Option<&Images>) -> Result<(bool, String)> {
    if let Some(spec) = &task.verifier {
        return verify_separate(app, vm, task, spec, name, images).await;
    }
    sh(vm, "rm -rf /logs/verifier /tests && mkdir -p /logs/verifier /logs/agent", "/", None, 30).await?;
    upload(vm, &task.dir.join("tests"), "/tests").await?;
    let (_, out) = sh(vm, "bash /tests/test.sh", &task.workdir, None, task.verifier_timeout).await?;
    Ok((reward(vm).await?, out))
}

/// The verifier's reward: `reward.json`'s `reward` field, else `reward.txt`. A pass is 1 or more.
async fn reward(vm: &Vm) -> Result<bool> {
    let (_, json) = sh(vm, "cat /logs/verifier/reward.json 2>/dev/null", "/", None, 30).await?;
    let (_, txt) = sh(vm, "cat /logs/verifier/reward.txt 2>/dev/null", "/", None, 30).await?;
    let value = serde_json::from_str::<serde_json::Value>(&json).ok().and_then(|v| v["reward"].as_f64()).or_else(|| txt.trim().parse::<f64>().ok());
    Ok(value.is_some_and(|r| r >= 1.0))
}

/// Harbor's separate verifier: run the `collect` commands where the agent worked, carry the
/// `artifacts` (the agent's diff, for DeepSWE) into a fresh environment built from tests/Dockerfile,
/// and run its tests/test.sh there. Nothing else the agent did reaches the tests.
async fn verify_separate(app: &App, agent_vm: &Vm, task: &TbTask, spec: &Env, name: &str, images: Option<&Images>) -> Result<(bool, String)> {
    let mut out = String::new();
    for (cmd, timeout) in &task.collect {
        let (code, o) = sh(agent_vm, cmd, "/", None, *timeout).await?;
        out += &format!("$ {cmd}\n[exit {code}] {o}\n");
    }
    // base64 keeps a binary diff intact through the exec channel. A directory travels as a tar of its contents, so
    // its files, modes and `exclude` survive: copying only plain files once left six FrontierHarness tasks (whose
    // artifacts are directories such as /app/) graded on an empty tree, 0 passes for every harness and model.
    let mut files = vec![];
    for a in &task.artifacts {
        let path = &a.path;
        if let Some(service) = &a.service {
            out += &format!("artifact {path} (service {service}) not copied: egin bench runs no service containers\n");
            continue;
        }
        let is_dir = sh(agent_vm, &format!("test -d '{path}'"), "/", None, 30).await?.0 == 0;
        let script = if is_dir {
            let exclude: String = a.exclude.iter().map(|e| format!(" --exclude='{e}'")).collect();
            format!("cd '{path}' && tar -cf /tmp/egin-artifact.tar{exclude} . && base64 < /tmp/egin-artifact.tar | tr -d '\\n'; rm -f /tmp/egin-artifact.tar")
        } else {
            format!("test -f '{path}' && base64 < '{path}' | tr -d '\\n'")
        };
        let (code, b64) = sh(agent_vm, &script, "/", None, 600).await?;
        if code == 0 {
            use base64::Engine;
            files.push((path.clone(), is_dir, base64::engine::general_purpose::STANDARD.decode(b64.trim()).context("decoding an artifact")?));
        }
    }
    let mut log = String::new();
    let vm = match images.and_then(|i| i.verifier.as_deref()) {
        Some(image) => Vm::Daytona(
            daytona_bench::create(app, &format!("{name}-verifier"), image, &spec.env, spec.cpus, spec.memory_mb, false, task.verifier_timeout + 3600, None)
                .await
                .context("building the verifier environment")?,
        ),
        None => build_env(spec, &format!("{name}-verifier"), &mut log).await.context("building the verifier environment")?,
    };
    let graded = async {
        sh(&vm, "mkdir -p /logs/verifier /logs/artifacts", "/", None, 30).await?;
        for (path, is_dir, bytes) in files {
            let script =
                if is_dir { format!("mkdir -p '{path}' && tar -xf - -C '{path}'") } else { format!("mkdir -p \"$(dirname '{path}')\" && cat > '{path}'") };
            let (code, o) = sh(&vm, &script, "/", Some(bytes), 600).await?;
            anyhow::ensure!(code == 0, "copying {path} to the verifier: {o}");
        }
        let (_, o) = sh(&vm, "bash /tests/test.sh", &spec.workdir, None, task.verifier_timeout).await?;
        out += &o;
        reward(&vm).await
    }
    .await;
    teardown(&vm).await;
    Ok((graded?, out))
}

/// `[agent] network_mode = "no-network"`: take the agent's VM off the network before it starts. A fork
/// keeps its source's network device, so this is done inside the guest; the agent runs as root and could
/// bring it back, so it guards against accidents, not against an agent set on fetching the answer.
///
/// Task images often have neither `ip` nor `ifconfig` (DeepSWE's base image has neither), so the link is
/// brought down with whatever the image has, down to a raw SIOCSIFFLAGS ioctl from Python or Perl. Then
/// it is checked: a run that still has an interface up or a default route fails as infrastructure,
/// rather than running online and scoring as if it had not.
async fn take_offline(vm: &Vm) -> Result<()> {
    // A Daytona sandbox of an offline task is created with its network blocked, outside the VM.
    if let Vm::Daytona(_) = vm {
        return Ok(());
    }
    sh(vm, OFFLINE_SCRIPT, "/", None, 30).await?;
    let (_, state) = sh(vm, OFFLINE_CHECK, "/", None, 30).await?;
    let state = state.trim();
    anyhow::ensure!(state == "offline", "could not take the VM offline for a no-network task: {state}");
    Ok(())
}

const OFFLINE_SCRIPT: &str = r#"for i in $(ls /sys/class/net); do
  [ "$i" = lo ] && continue
  ip link set "$i" down 2>/dev/null || ifconfig "$i" down 2>/dev/null || busybox ip link set "$i" down 2>/dev/null \
  || python3 -c 'import fcntl,socket,struct,sys; fcntl.ioctl(socket.socket(), 0x8914, struct.pack("16sH", sys.argv[1].encode(), 0))' "$i" 2>/dev/null \
  || perl -e 'use Socket; socket(my $s, PF_INET, SOCK_DGRAM, 0) or die; ioctl($s, 0x8914, pack("a16 s", $ARGV[0], 0)) or die' "$i" 2>/dev/null
done
ip route del default 2>/dev/null; busybox ip route del default 2>/dev/null; true"#;

/// Prints "offline", or what is still connected.
const OFFLINE_CHECK: &str = r#"up=""; for i in $(ls /sys/class/net); do [ "$i" = lo ] && continue
  [ "$(cat /sys/class/net/$i/operstate 2>/dev/null)" = up ] && up="$up $i"; done
route=$(awk 'NR > 1 && $2 == "00000000" { print $1 }' /proc/net/route 2>/dev/null | tr '\n' ' ')
if [ -z "$up" ] && [ -z "$route" ]; then echo offline; else echo "up:${up:- none} default-route:${route:-none}"; fi"#;

#[derive(Clone)]
pub struct Run {
    pub variant: String,
    pub overrides: toml::Table,
    /// Loaded once when the run starts, so editing the agent folder mid-run changes nothing.
    pub agent: Arc<Agent>,
    /// External harness driver folder (the [eve] arm).
    pub driver: Option<PathBuf>,
    /// The external harness runs inside the task's sandbox (`in_sandbox = true`: Codex CLI, ZCode), not on the host
    /// with its tools sent in, so the sandbox gets the router key as a Daytona secret.
    pub in_sandbox: bool,
}

/// The Daytona project secret holding the router key, for harnesses that run inside the sandbox
/// (EGIN_DAYTONA_MODEL_SECRET, default YDC_API_KEY). The sandbox sees it under the same name, as a placeholder.
fn model_secret() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| std::env::var("EGIN_DAYTONA_MODEL_SECRET").unwrap_or_else(|_| "YDC_API_KEY".into()))
}

impl Run {
    pub fn new(agent_dir: &Path, variant: &str, mut overrides: toml::Table) -> Result<Run> {
        let driver = overrides.remove("external").and_then(|v| v.as_str().map(|p| agent_dir.join(p)));
        let in_sandbox = overrides.remove("in_sandbox").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut o = overrides.clone();
        // Each run sets its own sandbox (a microVM fork, or Daytona with --sandbox daytona). The value here only has to
        // pass loading, which refuses microsandbox on a host without KVM, such as a Daytona orchestrator.
        let placeholder = if crate::sandbox::vm_supported() { "microsandbox" } else { "local" };
        o.insert("sandbox".into(), placeholder.into());
        o.insert("approval".into(), "never".into());
        let agent = Agent::load(agent_dir, &o).with_context(|| format!("arm {variant}"))?;
        Ok(Run { variant: variant.into(), overrides, agent: Arc::new(agent), driver, in_sandbox })
    }
}

/// One task: build its environment once, then run every variant in its own fork of it, in
/// parallel up to the shared `slots` limit. Daytona cannot fork its sandboxes, so there each run
/// builds an environment of its own instead.
pub async fn run_task(
    app: &Arc<App>,
    task: &TbTask,
    runs: &[Run],
    oracle: bool,
    results: &Path,
    slots: Arc<tokio::sync::Semaphore>,
    on_daytona: bool,
) -> Vec<Outcome> {
    let env_name = format!("egin-tb-{}-{}", task.name, crate::new_id());
    let mut log = String::new();
    let started = Instant::now();
    // Locally: one environment, forked per run. On Daytona: the Dockerfiles built into images, a sandbox per run.
    let (env, images) = if on_daytona {
        let images = async {
            let env = daytona_image(app, &task.environment, &env_name, &mut log).await?;
            let verifier = match &task.verifier {
                Some(spec) => Some(daytona_image(app, spec, &format!("{env_name}-verifier"), &mut log).await.context("building the verifier image")?),
                None => None,
            };
            anyhow::Ok(Images { env, verifier })
        }
        .await;
        match images {
            Ok(i) => {
                log += &format!("daytona image {} (verifier {})\n", i.env, i.verifier.as_deref().unwrap_or("none"));
                (Ok(None), Some(i))
            }
            Err(e) => (Err(e.context("environment build failed")), None),
        }
    } else {
        (build(task, &env_name, &mut log).await.map(Some), None)
    };
    let build_ms = started.elapsed().as_millis() as u64;
    let dir = results.join(&task.name);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("build.log"), &log);
    let fail = |variant: &str, model: &str, e: String| Outcome {
        eval: task.name.clone(),
        variant: variant.into(),
        model: model.into(),
        split: String::new(),
        category: "env_error".into(),
        pass: false,
        turns: 0,
        tool_calls: 0,
        prompt_tokens: 0,
        cached_tokens: 0,
        completion_tokens: 0,
        cost: 0.0,
        wall_ms: build_ms,
        error: Some(e),
        trace: dir.to_string_lossy().into(),
    };
    let env = match env {
        Ok(vm) => vm,
        Err(e) => {
            let _ = Sandbox::remove(&env_name).await;
            eprintln!("ENV  {} build failed: {}", task.name, format!("{e:#}").lines().next().unwrap_or(""));
            return runs.iter().map(|r| fail(&r.variant, r.overrides.get("model").and_then(|m| m.as_str()).unwrap_or(""), format!("{e:#}"))).collect();
        }
    };

    let arms = runs.iter().map(|r| {
        let (slots, env, images, dir) = (slots.clone(), env.as_ref(), images.as_ref(), &dir);
        async move {
            let _slot = slots.acquire_owned().await;
            let o = one(app, task, env, images, r, oracle, dir).await;
            let o = o.unwrap_or_else(|e| fail(&r.variant, r.overrides.get("model").and_then(|m| m.as_str()).unwrap_or(""), format!("{e:#}")));
            eprintln!(
                "{} {} / {} / {}{}",
                if o.pass { "PASS" } else { "FAIL" },
                o.eval,
                o.variant,
                o.model,
                o.error.as_deref().map(|e| format!(" ({})", e.lines().next().unwrap_or(""))).unwrap_or_default()
            );
            o
        }
    });
    let out = futures_util::future::join_all(arms).await;
    if let Some(env) = &env {
        teardown(env).await;
    }
    out
}

/// Stop and remove a VM within bounded time. A VM that never acknowledges a stop held a whole bench run
/// for hours after its result was in, so: a graceful stop, then a kill, then removal, each with a deadline.
/// A VM that survives all three is left behind and named, and the run's result still counts.
async fn teardown(vm: &Vm) {
    const WAIT: Duration = Duration::from_secs(30);
    let vm = match vm {
        Vm::Micro(vm) => vm,
        Vm::Daytona(r) => {
            if let Err(e) = r.delete().await {
                eprintln!("egin: {e:#}; Daytona deletes it after an hour without a command");
            }
            return;
        }
    };
    if vm.stop_with_timeout(WAIT).await.is_err() && vm.kill_with_timeout(WAIT).await.is_err() {
        eprintln!("egin: {} did not stop; left running", vm.name());
    }
    if !matches!(tokio::time::timeout(WAIT, Sandbox::remove(vm.name())).await, Ok(Ok(()))) {
        eprintln!("egin: {} was not removed", vm.name());
    }
}

/// The session an external harness works in, for `/tool/{name}`.
type ToolCtx = (Arc<App>, Arc<Agent>, String);

/// A local proxy in front of the router that counts the usage of every model call it forwards,
/// so an external harness is metered by the router's own numbers, not by its self-report. It also
/// serves egin's own `web_search` and `web_fetch` at `/tool/{name}`, so a harness compared with egin
/// searches with the same engine and the same exclude_domains / exclude_terms, or not at all when the
/// agent's web is off.
async fn meter(
    upstream: String,
    key: String,
    http: reqwest::Client,
    tools: ToolCtx,
) -> Result<(String, Arc<std::sync::Mutex<crate::model::Usage>>, Arc<std::sync::Mutex<Option<axum::body::Bytes>>>, tokio::task::JoinHandle<()>)> {
    use axum::{body::Body, extract::State, http::StatusCode, response::Response, routing::post, Json, Router};
    async fn tool(
        State((app, agent, sid)): State<ToolCtx>,
        axum::extract::Path(name): axum::extract::Path<String>,
        Json(args): Json<serde_json::Value>,
    ) -> (StatusCode, String) {
        if !matches!(name.as_str(), "web_search" | "web_fetch") {
            return (StatusCode::NOT_FOUND, format!("no tool {name}"));
        }
        if agent.cfg.web == "off" {
            return (StatusCode::FORBIDDEN, "web access is off for this task".into());
        }
        // Recorded as egin's own loop records them, so traces and leak audits cover the other harness too.
        app.emit(&sid, "tool_call", serde_json::json!({"tool": name, "args": args, "harness": "external"}));
        let out = crate::tools::execute(&app, &agent, &sid, &name, &args).await;
        app.emit(&sid, "tool_result", serde_json::json!({"tool": name, "output": out, "harness": "external"}));
        (StatusCode::OK, out)
    }
    use futures_util::StreamExt;
    type Shared = (String, String, reqwest::Client, Arc<std::sync::Mutex<crate::model::Usage>>);
    let total = Arc::new(std::sync::Mutex::new(crate::model::Usage::default()));
    async fn forward(State((up, key, http, total)): State<Shared>, body: axum::body::Bytes) -> Response {
        // The external harness's calls share egin's rate-limit breaker, and a 429 is retried here as egin's own
        // calls retry it, so the router's shared rate limit does not score against one harness only.
        let mut attempt = 0;
        let r = loop {
            crate::model::breaker::gate().await;
            let r = match http
                .post(format!("{up}/chat/completions"))
                .bearer_auth(&key)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return Response::builder().status(502).body(Body::from(e.to_string())).unwrap(),
            };
            match r.status().as_u16() {
                429 if attempt < 8 => {
                    crate::model::breaker::trip(r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()));
                    attempt += 1;
                }
                200..=299 => {
                    crate::model::breaker::ok();
                    break r;
                }
                _ => break r,
            }
        };
        let status = r.status().as_u16();
        let mut tail = String::new();
        let stream = r.bytes_stream().map(move |chunk| {
            if let Ok(c) = &chunk {
                tail.push_str(&String::from_utf8_lossy(c));
                // Usage arrives in one SSE line (or the JSON body); scan complete lines only.
                while let Some(i) = tail.find('\n') {
                    let line: String = tail.drain(..=i).collect();
                    record(&total, line.trim().trim_start_matches("data:").trim());
                }
                if !tail.contains("data:") && tail.trim_start().starts_with('{') && record(&total, tail.trim()) {
                    tail.clear();
                }
            }
            chunk
        });
        Response::builder().status(status).header("content-type", "text/event-stream").body(Body::from_stream(stream)).unwrap()
    }
    fn record(total: &std::sync::Mutex<crate::model::Usage>, data: &str) -> bool {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return false };
        let u = &v["usage"];
        if !u.is_object() || u["prompt_tokens"].is_null() {
            return false;
        }
        let mut t = total.lock().unwrap();
        t.prompt += u["prompt_tokens"].as_u64().unwrap_or(0);
        t.cached += u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
        t.completion += u["completion_tokens"].as_u64().unwrap_or(0);
        t.reasoning += u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0);
        true
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    // No body limit: axum's default 2 MB refused a long-context request with 413 before the router saw it,
    // failing the other harness on a limit egin's own calls never meet. The router enforces its own limits.
    /// A model the router does not serve (Claude or GPT on Bedrock, say) speaks another wire protocol. The
    /// harness still sends chat completions; each call goes out through egin's own client for that provider
    /// and comes back as a chat-completions stream, so both harnesses reach the model the same way.
    type Via = (ToolCtx, Arc<std::sync::Mutex<crate::model::Usage>>, Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>);
    /// What identifies an assistant turn when the harness sends it back: its first tool call, else its text.
    fn turn_key(m: &serde_json::Value) -> String {
        m["tool_calls"][0]["id"].as_str().or(m["content"].as_str()).unwrap_or("").to_string()
    }
    async fn via_client(State(((app, agent, _), total, turns)): State<Via>, Json(mut body): Json<serde_json::Value>) -> Response {
        let fail = |status: u16, e: String| Response::builder().status(status).body(Body::from(json!({"error": {"message": e}}).to_string())).unwrap();
        let ep = match crate::model::Endpoint::resolve(&app, &agent.cfg.model, Some(&agent.cfg)) {
            Ok(ep) => ep,
            Err(e) => return fail(400, format!("{e:#}")),
        };
        // The harness keeps only text and tool calls. The provider's own blocks (signed thinking, encrypted
        // reasoning) are put back on each turn it returns, as egin's loop replays them.
        for m in body["messages"].as_array_mut().into_iter().flatten().filter(|m| m["role"] == "assistant") {
            if let Some(raw) = turns.lock().unwrap().get(&turn_key(m)) {
                for k in ["anthropic_content", "anthropic_container", "openai_output"] {
                    if !raw[k].is_null() {
                        m[k] = raw[k].clone();
                    }
                }
            }
        }
        let reply = match crate::model::chat(&app, &ep, &body, &mut |_, _| {}).await {
            Ok(r) => r,
            Err(e) => return fail(502, format!("{e:#}")),
        };
        {
            let mut t = total.lock().unwrap();
            t.prompt += reply.usage.prompt;
            t.cached += reply.usage.cached;
            t.completion += reply.usage.completion;
            t.reasoning += reply.usage.reasoning;
        }
        let m = &reply.message;
        turns.lock().unwrap().insert(turn_key(m), m.clone());
        let calls: Vec<serde_json::Value> = m["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, c)| json!({"index": i, "id": c["id"], "type": "function", "function": c["function"]}))
            .collect();
        let mut delta = json!({"role": "assistant", "content": m["content"]});
        if !calls.is_empty() {
            delta["tool_calls"] = json!(calls);
        }
        let u = &reply.usage;
        let usage = json!({"prompt_tokens": u.prompt, "completion_tokens": u.completion, "total_tokens": u.prompt + u.completion,
            "prompt_tokens_details": {"cached_tokens": u.cached}, "completion_tokens_details": {"reasoning_tokens": u.reasoning}});
        let chunk =
            |choices: serde_json::Value| json!({"id": "egin", "object": "chat.completion.chunk", "created": 0, "model": body["model"], "choices": choices});
        let finish = if calls.is_empty() { "stop" } else { "tool_calls" };
        if body["stream"] != json!(true) {
            let whole = json!({"id": "egin", "object": "chat.completion", "created": 0, "model": body["model"], "usage": usage,
                "choices": [{"index": 0, "message": delta, "finish_reason": finish}]});
            return Response::builder().header("content-type", "application/json").body(Body::from(whole.to_string())).unwrap();
        }
        let mut last = chunk(json!([]));
        last["usage"] = usage;
        let sse =
            [chunk(json!([{"index": 0, "delta": delta, "finish_reason": null}])), chunk(json!([{"index": 0, "delta": {}, "finish_reason": finish}])), last]
                .iter()
                .map(|c| format!("data: {c}\n\n"))
                .collect::<String>();
        Response::builder().header("content-type", "text/event-stream").body(Body::from(format!("{sse}data: [DONE]\n\n"))).unwrap()
    }
    let (app, agent, _) = &tools;
    let native = crate::model::Endpoint::resolve(app, &agent.cfg.model, Some(&agent.cfg)).is_ok_and(|ep| ep.provider != "you");
    let calls = if native {
        Router::new().route("/chat/completions", post(via_client)).with_state((tools.clone(), total.clone(), Default::default()))
    } else {
        Router::new().route("/chat/completions", post(forward)).with_state((upstream, key, http, total.clone()))
    };
    // The harness's latest request carries its whole conversation so far: kept, it is the run's transcript.
    type Last = Arc<std::sync::Mutex<Option<axum::body::Bytes>>>;
    async fn keep(State(last): State<Last>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
            return Response::builder().status(400).body(Body::from("unreadable body")).unwrap();
        };
        *last.lock().unwrap() = Some(bytes.clone());
        next.run(axum::extract::Request::from_parts(parts, Body::from(bytes))).await
    }
    let last: Last = Default::default();
    let calls = calls.layer(axum::middleware::from_fn_with_state(last.clone(), keep));
    let router = calls.merge(Router::new().route("/tool/{name}", post(tool)).with_state(tools)).layer(axum::extract::DefaultBodyLimit::disable());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((url, total, last, task))
}

/// An external harness (the [eve] arm), working in microVM `vm` at `workdir`. Its driver gets the VM name
/// and the task's message, and prints {"status","reply"} as its last line. Its model calls and web tools go
/// through `meter`. `instructions` replaces the harness's own system instructions when the task brings
/// its own (an eval's answer format). After `timeout` seconds the driver is killed; the run's metered
/// usage is still recorded. Returns the error, if the run did not complete, and the reply.
#[allow(clippy::too_many_arguments)] // one call per harness, each argument a different fact about the run
pub(crate) async fn external(
    app: &Arc<App>,
    agent: &Arc<Agent>,
    sid: &str,
    driver: &Path,
    vm: &str,
    toolbox: Option<&str>,
    workdir: &str,
    instruction: &str,
    instructions: Option<&str>,
    timeout: Option<u64>,
) -> Result<(Option<String>, String)> {
    let file = std::env::temp_dir().join(format!("egin-instruction-{sid}.md"));
    std::fs::write(&file, instruction)?;
    let system = std::env::temp_dir().join(format!("egin-system-{sid}.md"));
    if let Some(text) = instructions {
        std::fs::write(&system, text)?;
    }
    let started = Instant::now();
    let info = app.model_info(&agent.cfg.model).await.unwrap_or_default();
    let (proxy, metered, last_request, proxy_task) =
        meter(app.base_url.clone(), app.key.clone(), app.http.clone(), (app.clone(), agent.clone(), sid.to_string())).await?;
    // The driver runs eve in its own copy of the project (eve allows one server per folder). The copy is
    // ours to delete: a timed-out driver is killed before it can clean up.
    let project = std::env::temp_dir().join(format!("egin-eve-{sid}"));
    let mut cmd = tokio::process::Command::new("node");
    cmd.arg(driver.join("run.mjs"))
        .arg(&file)
        .env("EGIN_EVE_PROJECT", &project)
        .env("EGIN_EVE_VM", vm)
        .env("EGIN_EVE_WORKDIR", workdir)
        .env("EGIN_TOOL_URL", &proxy)
        .env("EGIN_EVE_MODEL", &agent.cfg.model)
        .env("EGIN_EVE_MAX_COST", agent.cfg.max_cost.to_string())
        .env("EGIN_EVE_CONTEXT", agent.cfg.context_limit.unwrap_or(if info.context > 0 { info.context } else { 128_000 }).to_string())
        .env("EGIN_EVE_PRICE", json!({"input": info.input, "cached": info.cached, "output": info.output}).to_string())
        .env("YDC_API_KEY", "metered-by-egin")
        .env("EGIN_BASE_URL", &proxy)
        // For a harness that runs inside the sandbox and so cannot reach the meter: the router itself, and the
        // variable that holds the key's placeholder there. It reports its own usage.
        .env("EGIN_ROUTER_URL", &app.base_url)
        .env("EGIN_ROUTER_KEY_ENV", model_secret())
        .kill_on_drop(true);
    if instructions.is_some() {
        cmd.env("EGIN_EVE_INSTRUCTIONS", &system);
    }
    // A Daytona sandbox: eve's commands go to its toolbox instead of through microsandbox.
    if let Some(t) = toolbox {
        cmd.env("EGIN_EVE_TOOLBOX", t);
    }
    // Timing out drops the child (kill_on_drop) but keeps us here, so the meter is still recorded.
    let out = match timeout {
        Some(s) => tokio::time::timeout(Duration::from_secs(s), cmd.output()).await.ok(),
        None => Some(cmd.output().await),
    };
    proxy_task.abort();
    // The other harness's transcript, for diagnosing its runs as egin's are: its last request's messages.
    if let Some(b) = last_request.lock().unwrap().take() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
            app.emit(sid, "external_transcript", json!({"messages": v["messages"]}));
        }
    }
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&system);
    let usage = metered.lock().unwrap().clone();
    let mut stats = app.store.stats(sid)?;
    stats.prompt_tokens = usage.prompt;
    stats.cached_tokens = usage.cached;
    stats.completion_tokens = usage.completion;
    stats.reasoning_tokens = usage.reasoning;
    stats.cost = crate::model::cost(&info, &usage);
    stats.wall_ms = started.elapsed().as_millis() as u64;
    app.store.put_stats(sid, &stats)?;
    let Some(out) = out else {
        return Ok((Some(format!("agent timed out after {}s", timeout.unwrap_or_default())), String::new()));
    };
    let out = out?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last: serde_json::Value = stdout.lines().rev().find_map(|l| serde_json::from_str(l).ok()).unwrap_or_default();
    // A harness inside the sandbox called the router directly; its own count is the run's usage.
    if usage.prompt == 0 && last["usage"].is_object() {
        let n = |k: &str| last["usage"][k].as_u64().unwrap_or(0);
        let own = crate::model::Usage { prompt: n("prompt"), cached: n("cached"), completion: n("completion"), reasoning: n("reasoning") };
        stats.prompt_tokens = own.prompt;
        stats.cached_tokens = own.cached;
        stats.completion_tokens = own.completion;
        stats.reasoning_tokens = own.reasoning;
        stats.cost = crate::model::cost(&info, &own);
        app.store.put_stats(sid, &stats)?;
    }
    app.store.push_message(sid, &json!({"role": "assistant", "content": last["reply"]}))?;
    app.emit(
        sid,
        "external",
        json!({"result": last, "stderr": String::from_utf8_lossy(&out.stderr).chars().rev().take(3000).collect::<String>().chars().rev().collect::<String>()}),
    );
    let error = match last["status"].as_str() {
        Some("completed") | Some("succeeded") | Some("done") => None,
        Some(s) => Some(format!("eve: {s} {}", last["error"])),
        None => Some(format!("eve driver failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""))),
    };
    Ok((error, last["reply"].as_str().unwrap_or("").to_string()))
}

/// One run: the agent works in a fork of the local `env`, or in a Daytona sandbox of its own from the task's
/// `images`.
async fn one(app: &Arc<App>, task: &TbTask, env: Option<&Vm>, images: Option<&Images>, r: &Run, oracle: bool, dir: &Path) -> Result<Outcome> {
    let driver = r.driver.clone();
    let mut agent = (*r.agent).clone();
    agent.cfg.workdir = Some(task.workdir.clone());
    // The task's time limit is enforced from outside the run; the run hears about it before it hits.
    agent.cfg.deadline = task.agent_timeout;
    if images.is_some() {
        // The agent's tools reach the run's Daytona sandbox through the session (`app.daytona`).
        agent.cfg.sandbox = "daytona-v2".into();
    }
    // The system prompt names the working directory, which is per task.
    let agent = Arc::new(agent);
    let sid = run::new_session(app, &agent)?;
    // The environment is ours (built above) and its only host resource is the open network,
    // which every run of the task should have, as Terminal-Bench's allow_internet says.
    let vm = match (env, images) {
        (Some(Vm::Micro(env)), _) => {
            Vm::Micro(Box::new(env.fork(crate::sandbox::vm_name(&sid)).dangerously_inherit_resources().fork().await.context("forking the task environment")?))
        }
        (_, Some(images)) => {
            // The run's whole span (agent, collect commands, grading) plus an hour; a crash cannot outlast it.
            let lifetime = task.agent_timeout + task.verifier_timeout + task.collect.iter().map(|(_, t)| t).sum::<u64>() + 3600;
            let spec = &task.environment;
            let host = reqwest::Url::parse(&app.base_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            let model = r.in_sandbox.then(|| (model_secret(), host.as_str()));
            let name = crate::sandbox::vm_name(&sid);
            // EGIN_E2B_ONLY skips Daytona for the run's sandbox, to try the fallback on purpose.
            let daytona = if crate::setting("E2B_ONLY").is_some_and(|v| !v.is_empty()) {
                Err(anyhow!("EGIN_E2B_ONLY is set"))
            } else {
                daytona_bench::create(app, &name, &images.env, &spec.env, spec.cpus, spec.memory_mb, task.offline, lifetime, model).await
            };
            match daytona {
                Ok(d) => Vm::Daytona(d),
                // E2B takes the run when Daytona cannot. Not for a harness inside the sandbox: E2B has no secret
                // proxy, so the router key would sit in the guest.
                Err(e) if model.is_none() && std::env::var("E2B_API_KEY").is_ok_and(|k| !k.is_empty()) => {
                    eprintln!("egin: daytona could not boot {name} ({e:#}); running it on E2B");
                    let vm = Vm::Daytona(
                        daytona_bench::e2b_fallback(app, &name, &spec.image, &spec.env, spec.cpus, task.offline, lifetime)
                            .await
                            .map_err(|e2| anyhow!("booting the task environment: daytona: {e:#}; e2b: {e2:#}"))?,
                    );
                    let mut log = String::new();
                    if let Err(e2) = replay(&vm, spec, &mut log).await {
                        teardown(&vm).await;
                        bail!("booting the task environment on E2B: {e2:#}\n{log}");
                    }
                    vm
                }
                Err(e) => bail!("booting the task environment: {e:#}"),
            }
        }
        _ => bail!("a run needs a local environment or Daytona images"),
    };
    match &vm {
        Vm::Micro(m) => {
            app.vms.lock().await.insert(sid.clone(), (**m).clone());
        }
        Vm::Daytona(d) => {
            app.daytona.lock().await.insert(sid.clone(), (d.id.clone(), d.toolbox.clone()));
        }
    }
    if task.offline {
        take_offline(&vm).await?;
    }

    let started = Instant::now();
    let error = if oracle {
        upload(&vm, &task.dir.join("solution"), "/solution").await?;
        let (code, out) = sh(&vm, "bash /solution/solve.sh", &task.workdir, None, task.agent_timeout).await?;
        app.emit(&sid, "oracle", json!({"code": code, "output": out}));
        None
    } else if let Some(driver) = &driver {
        let instruction = std::fs::read_to_string(task.dir.join("instruction.md"))?;
        run::send(app, &sid, &instruction)?;
        let toolbox = match &vm {
            Vm::Daytona(d) => Some(d.toolbox.as_str()),
            Vm::Micro(_) => None,
        };
        match external(app, &agent, &sid, driver, vm.name(), toolbox, &task.workdir, &instruction, None, Some(task.agent_timeout)).await {
            Ok((e, _)) => e,
            Err(e) => Some(format!("{e:#}")),
        }
    } else {
        let instruction = std::fs::read_to_string(task.dir.join("instruction.md"))?;
        run::send(app, &sid, &instruction)?;
        match tokio::time::timeout(Duration::from_secs(task.agent_timeout), run::run_session(app.clone(), agent.clone(), sid.clone())).await {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(_) => {
                app.store.set_status(&sid, "timeout")?;
                Some(format!("agent timed out after {}s", task.agent_timeout))
            }
        }
    };
    let agent_ms = started.elapsed().as_millis() as u64;
    let (pass, verifier_out) = verify(app, &vm, task, vm.name(), images).await?;
    app.emit(&sid, "verifier", json!({"pass": pass, "output": verifier_out.chars().rev().take(4000).collect::<String>().chars().rev().collect::<String>()}));

    let variant_dir = dir.join(format!("{}__{}", r.variant, agent.cfg.model));
    save_trace(app, &sid, &variant_dir)?;
    // The event keeps the tail; the whole log is where the failing assertions are.
    let _ = std::fs::write(variant_dir.join("verifier.log"), &verifier_out);
    app.vms.lock().await.remove(&sid);
    app.daytona.lock().await.remove(&sid);
    teardown(&vm).await;

    let mut out = Outcome {
        eval: task.name.clone(),
        variant: r.variant.clone(),
        model: if oracle { "oracle".into() } else { agent.cfg.model.clone() },
        category: task.difficulty.clone(),
        ..crate::eval::outcome(app, &sid, pass, error, &variant_dir)?
    };
    if out.wall_ms == 0 || oracle {
        out.wall_ms = agent_ms;
    }
    Ok(out)
}

/// What `egin bench` runs.
pub struct BenchOpts {
    /// Folder of task folders (each with task.toml).
    pub tasks: PathBuf,
    /// Agent folder whose evals/variants.toml defines the arms.
    pub agent_dir: PathBuf,
    /// Only these task names; empty means all.
    pub names: Vec<String>,
    pub variants: Vec<String>,
    pub models: Vec<String>,
    /// Run the reference solutions instead of the agent.
    pub oracle: bool,
    /// Task environments built and kept at a time.
    pub jobs: usize,
    /// Agent runs at a time across all tasks.
    pub parallel: usize,
    pub results: PathBuf,
    /// Run task environments as Daytona cloud sandboxes instead of local microVMs.
    pub daytona: bool,
}

/// Run every arm on every task. Results are appended to `<results>/summary.jsonl` as each task
/// finishes, so a long run can be read part way.
pub async fn run_bench(app: &Arc<App>, opts: &BenchOpts) -> Result<Vec<Outcome>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&opts.tasks)?.flatten().map(|e| e.path()).filter(|p| p.join("task.toml").exists()).collect();
    dirs.retain(|d| opts.names.is_empty() || opts.names.iter().any(|n| d.ends_with(n)));
    dirs.sort();

    let mut runs = vec![];
    for (name, o) in crate::eval::variants(&opts.agent_dir)? {
        if !opts.variants.is_empty() && !opts.variants.contains(&name) {
            continue;
        }
        let models: Vec<Option<&String>> = if opts.models.is_empty() { vec![None] } else { opts.models.iter().map(Some).collect() };
        for m in models {
            let mut o = o.as_table().cloned().unwrap_or_default();
            if let Some(m) = m {
                o.insert("model".into(), m.as_str().into());
            }
            runs.push(Run::new(&opts.agent_dir, &name, o)?);
        }
        if opts.oracle {
            break; // the reference solution does not depend on the arm
        }
    }

    std::fs::create_dir_all(&opts.results)?;
    eprintln!("{} tasks x {} runs -> {}", dirs.len(), runs.len(), opts.results.display());
    let envs = Arc::new(tokio::sync::Semaphore::new(opts.jobs.max(1)));
    let slots = Arc::new(tokio::sync::Semaphore::new(opts.parallel.max(1)));
    let mut handles = vec![];
    for d in dirs {
        let task = match load_task(&d) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("SKIP {}: {e:#}", d.display());
                continue;
            }
        };
        let (app, runs, results, envs, slots, oracle, on_daytona) =
            (app.clone(), runs.clone(), opts.results.clone(), envs.clone(), slots.clone(), opts.oracle, opts.daytona);
        handles.push(tokio::spawn(async move {
            let _env = envs.acquire_owned().await;
            let out = run_task(&app, &task, &runs, oracle, &results, slots, on_daytona).await;
            let lines: String = out.iter().filter_map(|o| serde_json::to_string(o).ok()).map(|l| l + "\n").collect();
            let appended = std::fs::OpenOptions::new().create(true).append(true).open(results.join("summary.jsonl"));
            if let Err(e) = appended.and_then(|mut f| std::io::Write::write_all(&mut f, lines.as_bytes())) {
                eprintln!("egin: writing results for {}: {e}", task.name);
            }
            out
        }));
    }
    let mut all = vec![];
    for h in handles {
        all.extend(h.await?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heredocs() {
        let image = parse_dockerfile(
            "# syntax=docker/dockerfile:1.6\nFROM python:3.11-slim\nRUN python - <<'PYEOF'\nimport site, os\n# a comment in the script\nprint(1<<2)\nPYEOF\nRUN <<EOF\necho hi\nEOF\nRUN cat <<-END > /x\n\tindented\n\tEND\nRUN echo $((1<<3)) && echo after\n",
        )
        .unwrap();
        let runs: Vec<&str> = image.steps.iter().filter_map(|s| if let Step::Run(c) = s { Some(c.as_str()) } else { None }).collect();
        assert_eq!(runs[0], "python - <<'PYEOF'\nimport site, os\n# a comment in the script\nprint(1<<2)\nPYEOF");
        assert_eq!(runs[1], "sh <<EOF\necho hi\nEOF");
        assert_eq!(runs[2], "cat <<-END > /x\n\tindented\n\tEND");
        assert_eq!(runs[3], "echo $((1<<3)) && echo after");
        assert_eq!(runs.len(), 4);
    }

    #[test]
    fn loads_a_separate_verifier_task() {
        let dir = std::env::temp_dir().join(format!("egin-sep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("environment")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("environment/Dockerfile"), "FROM alpine:3.20\nWORKDIR /app\n").unwrap();
        std::fs::write(dir.join("tests/Dockerfile"), "FROM python:3.13-slim\nWORKDIR /src\nCOPY test.sh /tests/test.sh\n").unwrap();
        std::fs::write(
            dir.join("task.toml"),
            r#"artifacts = ["/logs/artifacts/model.patch", { source = "/app/lib", exclude = ["__pycache__", "*.pyc"] }, { source = "/var/hub.json", service = "hub" }]
[agent]
network_mode = "no-network"
[verifier]
environment_mode = "separate"
timeout_sec = 1800.0
[[verifier.collect]]
command = "git diff > /logs/artifacts/model.patch"
timeout_sec = 60
"#,
        )
        .unwrap();
        let t = load_task(&dir).unwrap();
        let v = t.verifier.as_ref().expect("separate verifier");
        assert_eq!((v.image.as_str(), v.workdir.as_str()), ("python:3.13-slim", "/src"));
        assert_eq!(v.context, dir.join("tests"));
        assert_eq!(t.collect, vec![("git diff > /logs/artifacts/model.patch".to_string(), 60)]);
        let artifact = |path: &str, exclude: &[&str], service: Option<&str>| Artifact {
            path: path.into(),
            exclude: exclude.iter().map(|e| e.to_string()).collect(),
            service: service.map(String::from),
        };
        assert_eq!(
            t.artifacts,
            vec![
                artifact("/logs/artifacts/model.patch", &[], None),
                artifact("/app/lib", &["__pycache__", "*.pyc"], None),
                artifact("/var/hub.json", &[], Some("hub"))
            ]
        );
        assert!(t.offline);
        assert_eq!(t.verifier_timeout, 1800);

        // Shared (the default) keeps grading in the agent's VM.
        std::fs::write(dir.join("task.toml"), "[verifier]\ntimeout_sec = 60\n").unwrap();
        let t = load_task(&dir).unwrap();
        assert!(t.verifier.is_none() && !t.offline);
        std::fs::remove_dir_all(&dir).ok();
    }
}
