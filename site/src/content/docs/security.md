---
title: Security
description: What an agent can reach, where secrets live, who may call the server, and what to check before production.
group: Run
order: 8
---

An agent runs commands a model wrote, and the model may have written them after reading text from the web or from a user. Treat what the model decides as untrusted, and rely on the limits egin enforces around it: the sandbox, the network list, secret placeholders, approvals, middleware and the server token. This page says what each one protects, and what it doesn't.

## What runs where

| Part | Runs | Can reach |
|---|---|---|
| The model's commands and your tools (`bash`, the file tools, `tools/`) | In the session's [sandbox](/docs/sandboxing) | The workspace, and the hosts `network` allows |
| `web_search` and `web_fetch` | On the host | The web, except `exclude_domains` |
| [Middleware and hooks](/docs/middleware-and-hooks) | On the host, outside the sandbox | Whatever the egin process can. Hooks run without egin's own API keys in their environment |
| [Connections](/docs/connections) to MCP servers | From the host | The server in the connection file, with its headers or OAuth tokens |

Middleware, hooks and connection files are your code and your choices, so egin trusts them. The sandbox holds what the model writes.

## The sandbox is the boundary

Each session has its own sandbox, so a command can't touch your files, another session or a host you haven't allowed. Two things weaken that:

- `sandbox = "local"` is no isolation. Tools run on the host as ordinary processes. `auto` falls back to `local`, with a warning, on a machine that can't run a microVM. In production, name the backend you mean.
- Not every backend can limit the network. E2B and Vercel can't restrict egress to a list of hosts. An agent whose `network` names hosts gets an error there, instead of a rule that isn't enforced.

Set `network` to the hosts the agent needs. The default is everything:

```toml
sandbox = "microsandbox"
network = ["api.github.com", "pypi.org"]
```

## Secrets

List each secret with the one host it is for:

```toml
secrets = [{ name = "GITHUB_TOKEN", host = "api.github.com" }]
```

Inside the sandbox the variable holds a placeholder. The real value is put in only on requests to that host, so a command that sends the variable anywhere else sends the placeholder. This needs the microsandbox backend; every other backend refuses `secrets`, because tools there could read the real value.

Keep secrets out of files the model reads:

- Connection files take `${VAR}` from the host's environment, so they hold no secret and can be committed.
- OAuth tokens are stored in `.egin/oauth/`, readable only by you.
- `.egin/` also holds the session database: every message, tool call and result. Add it to `.gitignore`, and protect it like the data that passes through the agent.
- Instructions, skills and the workspace are shown to the model. Don't put keys in them.

## Text the agent reads

A web page, a file or a tool result can contain instructions, and the model may follow them. Nothing in a prompt reliably prevents that. The limits that hold whatever the model decides are the ones outside it:

- `network` stops the sandbox from reaching hosts you didn't list.
- Secret placeholders keep a real key from being sent anywhere but its host.
- [Approvals](/docs/approvals) put a person in front of calls that change something.
- [`middleware/pre_tool`](/docs/middleware-and-hooks) blocks calls by rule.
- `exclude_domains` keeps domains out of every search and fetch.

## Who can call the server

`egin serve` listens on `127.0.0.1` by default. It refuses any other `--host` unless `EGIN_TOKEN` is set, because anyone who can reach the port could run the agent and approve its tool calls.

- `EGIN_TOKEN`: when set, every route except `/health` and the channel routes needs `Authorization: Bearer <token>`.
- Channel routes check each request themselves: a signature over the body, or the platform's secret token. Unsigned or wrongly signed requests are refused.
- `X-Egin-User`: your server sends it after its own sign-in check, and each user then reaches only their own sessions. The browser must never set it. Requests without the header are the operator's and reach every session.

Put egin behind your own server, and keep the token there. See the [HTTP API](/docs/http-api#authentication) and [Deploy](/docs/deploy).

## Limits on a run

A run that goes wrong should stop on its own:

| Setting | Limits |
|---|---|
| `max_turns` | Model calls in a run |
| `max_cost` | Spend per session, in USD |
| `bash_timeout` | Seconds one command may run |
| `idle_timeout` | Seconds a sandbox stays up without a call |

## Before production

- `sandbox` names a backend that isolates, not `local` and not `auto`.
- `network` lists only the hosts the agent needs.
- Every key the tools use is in `secrets`, with its host. None is in instructions, skills, tools or the workspace.
- `approval` is `ask` or `once` for an agent a person watches. Only tools that read are marked safe.
- An agent nobody watches has `middleware/pre_tool` rules for what it must never do.
- `EGIN_TOKEN` is set, and only your server holds it.
- Your server sets `X-Egin-User` after sign-in, and the browser can't.
- Every channel has its signing secret set.
- `max_turns` and `max_cost` are set.
- `.egin/` is out of version control and stored where your data may live.
- Logs and traces go only where the conversations in them may go: they include tool calls and results.

## What to read next

- [Sandboxing](/docs/sandboxing): backends, the network list and secrets in full.
- [Approvals and questions](/docs/approvals): which calls wait for a person.
- [Middleware and hooks](/docs/middleware-and-hooks): rules that hold without one.
