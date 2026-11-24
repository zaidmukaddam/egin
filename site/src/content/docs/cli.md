---
title: CLI
description: Every egin command and its flags.
group: Reference
order: 1
---

Most commands take the agent folder as their first argument. `models`, `report`, `bench` and `docs` don't; `add` and `connect` take it as `--dir` (default `.`), and `serve` defaults it to the current folder.

## egin init

Write a starter agent folder.

```bash
egin init <dir> [--template basic|research|ops|support|web]
```

## egin add

Add an integration from egin's catalog to an agent folder: an MCP server connection, a channel, a trace exporter or an example tool.

```bash
egin add                                 # list the catalog
egin add connection/github [--dir .] [--force]
egin add channel https://provider.com/docs/webhooks   # a guide to describe a channel for any other platform
```

It writes the item's files (such as `connections/github.toml`), keeps any that already exist unless `--force`, and says which environment variables to set and what to do next. For a misspelt item, it lists the close matches. The catalog is part of the binary, so nothing is downloaded and an item always matches your egin. Most connections authenticate with tokens from the environment; one with `auth = "oauth"` needs `egin connect`.

`egin add channel <url>` writes nothing. It prints instructions for a coding agent: read the platform's webhook docs at the URL, write a [described channel](/docs/channels#described-channels) for it, and test it with a signed request.

## egin connect

Sign in to an MCP connection that uses OAuth (`auth = "oauth"` in `connections/<name>.toml`).

```bash
egin connect vercel [--dir .]
```

It opens the browser at the server's sign-in page, waits for the redirect on `127.0.0.1:33418`, and stores the tokens in `.egin/oauth/<name>.json`, readable only by you. Run it again when a sign-in expires for good; until then egin refreshes the token itself. See [OAuth sign-in](/docs/connections#oauth-sign-in).

## egin docs

Print this documentation from the binary, so it matches the installed version.

```bash
egin docs              # the list of pages
egin docs channels     # one page
```

## egin info

Show what an agent folder resolves to: the merged config, tools, skills, subagents, schedules, MCP connections and channels. Use it when a setting is not doing what you expect.

```bash
egin info <dir>
```

## egin trajectory

Write a session as an [ATIF](https://docs.harborframework.com/agents/atif) trajectory, the format Harbor validates and [Repo2RLEnv](https://github.com/huggingface/Repo2RLEnv) turns into RL tasks. egin builds it from the session's event log, so it keeps every reply and tool result even when egin elided or summarised them in the model's context. `edit_file` calls are written as `edit` with `old_string`/`new_string`, the names trajectory tools replay, and egin's own form stays in the call's `extra`. Evals and `egin bench` also write each run's trajectory as `trajectory.json` next to its transcript.

```bash
egin trajectory <dir>                         # the most recent session, to stdout
egin trajectory <dir> --session <id> -o run.json
```

For a file the session only read, Repo2RLEnv rebuilds the contents from a line-numbered read, and egin's `read_file` does not return one. An edit to such a file replays as lost. Files the session wrote replay in full.

## egin models

List the router's models with their context limits and prices.

## egin run

Run one prompt to completion.

```bash
egin run <dir> "<prompt>" [--yes] [--session <id>] [--set key=value] [--schema schema.json]
```

| Flag | Meaning |
|---|---|
| `--yes` | Approve every tool call for this run |
| `--schema schema.json` | Answer with a value that fits this JSON Schema, printed as JSON; see [Structured output](/docs/http-api#structured-output) |
| `--session <id>` | Resume an existing session; the prompt is optional then |
| `--set key=value` | Override an `agent.toml` key, for example `--set model=glm-5.3` |

Pass `-` as the prompt to read it from stdin, which keeps a long prompt off the process list.

## egin chat

Start an interactive session in the terminal. Takes `--yes` and `--set`.

## egin serve

Serve the agent over HTTP with an event stream, channels and schedules. Interrupted sessions resume on start.

```bash
egin serve [dir] [--port 8787] [--host 127.0.0.1]
```

`dir` defaults to the current folder, and can be an app with its agent in `agent/`. The port defaults to `$PORT` when the host sets one, else 8787.

Any `--host` other than loopback needs `EGIN_TOKEN`. See [HTTP API](/docs/http-api#authentication).

## egin eval

Run `evals/` across the variants in `evals/variants.toml`.

```bash
egin eval <dir> [--model m] [--variant v] [--split train|heldout] [--task t] [--jobs 4] [--set key=value]
```

## egin evolve

Edit a model's profile files from eval traces, keeping only edits that score better.

```bash
egin evolve <dir> --model <m> [--proposer <m>] [--rounds 4] [--jobs 4]
```

## egin bench

Run Terminal-Bench 2 tasks, or converted SWE-bench tasks, in local microVMs or Daytona cloud sandboxes.

```bash
egin bench <tasks> [--agent bench/tb2] [--subset file] [--task t] [--model m] [--variant v] [--oracle] [--jobs 3] [--parallel 8] [--out dir] [--sandbox microsandbox|daytona]
```

`--sandbox daytona` runs task environments in Daytona cloud sandboxes (x86, so amd64-only images run as they are) and needs `DAYTONA_API_KEY`.

## egin report

Build one HTML page from results folders.

```bash
egin report <results>... [--title "..."] [--method method.md] [-o results.html]
```

## Environment

egin also loads variables from `.env.local` and `.env` in the current folder, without overriding ones already set. Provider keys are listed in [Providers](/docs/providers).

| Variable | Meaning |
|---|---|
| `YDC_API_KEY` | Key for the router, search and contents APIs |
| `EGIN_BASE_URL` | Model endpoint, default `https://api.you.com/v1` |
| `EGIN_SEARCH_URL` | Search endpoint, default `https://ydc-index.io/v1` |
| `EGIN_TOKEN` | Bearer token required by the session routes of `egin serve` |
| `EGIN_STORE_URL` | A libSQL database that holds the sessions in place of `<agent>/.egin/egin.db`, shared by every egin process pointed at it; see [Deploy](/docs/deploy#several-instances-and-serverless) |
| `EGIN_STORE_TOKEN` | The auth token for `EGIN_STORE_URL` |
| `EGIN_LOG` | `json` writes every stored event to stderr |
| `EGIN_OAUTH_CLIENT` | Client metadata document URL that `egin connect` uses as its client ID, default `https://egin.dev/oauth/client.json` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Export sessions, model calls and tool calls as OTLP spans; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` takes precedence |
| `ANTHROPIC_BASE_URL` | Claude API endpoint, default `https://api.anthropic.com` |
