---
title: Sandboxing
description: Each session runs its tools in its own microVM, with network and secrets under your control.
group: Run
order: 1
---

A sandbox is the machine an agent's tools run on, apart from yours. Each session gets its own, so a shell command the model writes can't touch your files, other sessions or hosts you haven't allowed. You choose the kind of sandbox with `sandbox` in `agent.toml`, which hosts it can reach with `network`, and which secrets it can use with `secrets`.

```toml
# agent.toml
sandbox = "auto"            # a microVM where this machine supports one
network = ["api.github.com"]
```

## One VM per session

Each session gets its own microVM, named `egin-<session>`, created on its first tool call. The agent's `workspace/` is mounted at `/workspace` and its `tools/` folder read-only at `/egin/tools`.

The VM stops after `idle_timeout` seconds without a call and starts again on the next one. After a crash, egin reconnects to it, or replaces it if its creation was cut off. It is removed when the session is deleted or an eval run ends. A subagent runs in a sandbox of its own, named for its own session.

## Backends

| `sandbox` | What runs your tools |
|---|---|
| `auto` | `microsandbox` where the host supports it, otherwise `local` with a warning |
| `microsandbox` | One local microVM per session; needs KVM on Linux, or Apple silicon |
| `local` | The host itself, as a process group that is killed whole on timeout |
| `e2b` | E2B sandboxes, with `E2B_API_KEY` (template from `E2B_TEMPLATE`, default `base`) |
| `vercel` | Vercel Sandbox, with `VERCEL_TOKEN` plus `VERCEL_TEAM_ID` and `VERCEL_PROJECT_ID`, or `VERCEL_OIDC_TOKEN` |
| `daytona` | Daytona cloud sandboxes, with `DAYTONA_API_KEY` |
| `daytona-macos` | Daytona macOS sandboxes on Apple silicon, with `DAYTONA_API_KEY`; see [macOS](#macos) |
| `docker` | Docker Sandboxes, as a local microVM, through the signed-in `sbx` CLI |
| `docker-cloud` | Docker Sandboxes in Docker's cloud |

Cloud backends copy the workspace up once and do not sync it back. A restarted egin finds a session's sandbox again by name, and the sandbox is deleted with its session.

E2B and Vercel can't limit network access to a list of hosts: an agent whose `network` names hosts gets an error instead of a rule that isn't enforced. Vercel also can't turn network off, so it needs `network = ["*"]`.

## macOS

`sandbox = "daytona-macos"` runs a session's tools on a Mac: a macOS VM on Daytona's Apple silicon, for agents that build and test iOS and Mac apps. The base image, `base-macos`, has Xcode with the iPhone simulator, Homebrew, git, Node.js and Python. `xcodebuild`, `swift` and `xcrun simctl` work from `bash` like any other command.

```toml
sandbox = "daytona-macos"
cpus = 6            # at least 4
memory_mb = 12288   # at least 4096
```

The workspace is at `/Users/lume/workspace` and the agent's tools at `/Users/lume/.egin/tools`; the user is `lume`, with `sudo`. `network` can name hosts here, unlike E2B and Vercel.

A Mac sandbox runs on a Mac your Daytona organization reserves for 24 hours at a time: about $60 a day for an M4 Pro. When the organization holds no reservation, Daytona buys one on the session's first tool call, and it renews while a sandbox on it runs. A reserved Mac holds two sandboxes, and later sessions use it until the day is up. `DAYTONA_MACOS_REGION` picks the region (default `us-east-bos-1`; `GET https://api.daytona.com/mac-catalog` lists where Macs are free).

## Network

`network` is the sandbox's egress allowlist.

```toml
network = ["*"]                          # everything (the default)
network = []                             # nothing
network = ["pypi.org", ".github.com"]    # these hosts; a leading dot allows subdomains
```

Web search and page fetches do not need the sandbox's network: `web_search` and `web_fetch` run on the host.

## Secrets

The VM never sees a real secret. List the secret and the one host it is for:

```toml
secrets = [{ name = "GITHUB_TOKEN", host = "api.github.com" }]
```

Inside the VM, `$GITHUB_TOKEN` is a placeholder. The real value from the host's environment is substituted only on requests to `api.github.com`. A prompt injection that tells the agent to send the token somewhere else sends the placeholder. egin allows the secret's host even when `network` doesn't list it, and a secret whose variable isn't set on the host is an error.

> Secrets need the microsandbox backend. Every other backend refuses them, `local` included (and `auto` when it falls back to `local`): tools there could read the real value.

## What to read next

- [Tools](/docs/tools): what runs inside the sandbox.
- [Middleware and hooks](/docs/middleware-and-hooks): block calls before they reach it.
- [Deploy](/docs/deploy): which sandbox to use when the agent runs in a container.
- [Security](/docs/security): the sandbox next to the other limits, and a checklist before production.
