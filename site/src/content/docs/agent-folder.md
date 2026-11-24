---
title: The agent folder
description: Every file egin reads, what each one is for, and where to read more. Only agent.toml and instructions.md are needed.
group: Build
order: 1
---

An agent is a folder. egin reads the files below when it loads the agent; every one is optional except `agent.toml` and `instructions.md`, and `egin init` writes a folder that has the common ones.

```
my-agent/
  agent.toml               settings
  instructions.md          what the agent is and how it works
  memory/                  notes added to the prompt
  tools/                   one script per tool
  skills/                  procedures loaded when needed
  connections/             MCP servers
  subagents/               agents this one can hand work to
  middleware/pre_tool      a check before every tool call
  hooks/                   scripts that run when something happens
  channels/                Slack, webhooks, GitHub and more
  schedules/               prompts on a cron
  profiles/                settings and instructions per model
  evals/                   prompts with a check
  instrumentation/         where traces go
  workspace/               the files the agent works on
  .egin/                   sessions and indexes (egin writes this)
```

| File | What it is for | Read |
|---|---|---|
| `agent.toml` | The model and any setting you change from its default | [Configuration](/docs/configuration) |
| `instructions.md` | The system prompt, in plain Markdown | [Instructions and memory](/docs/instructions) |
| `memory/*.md` | Notes added to the prompt; the agent can keep its own in `memory/notes.md` | [Instructions and memory](/docs/instructions#memory) |
| `tools/<name>.py` (any language) | A tool: one script that describes itself in its first comments | [Tools](/docs/tools) |
| `skills/<name>/SKILL.md` | A procedure the agent reads only when it needs it | [Skills](/docs/skills) |
| `connections/<name>.toml` | An MCP server whose tools the agent can use | [Connections](/docs/connections) |
| `subagents/<name>/` | A whole agent folder this agent can hand a task to | [Subagents](/docs/subagents) |
| `middleware/pre_tool` | A script that can block a tool call before it runs | [Middleware and hooks](/docs/middleware-and-hooks) |
| `hooks/<event>` | A script that runs each time a session stores that event | [Middleware and hooks](/docs/middleware-and-hooks#hooks) |
| `channels/<name>.toml` | Where people reach the agent: Slack, Telegram, Discord, GitHub, a webhook, MCP | [Channels and schedules](/docs/channels) |
| `schedules/<name>.toml` | A prompt that runs on a cron while `egin serve` runs | [Channels and schedules](/docs/channels#schedules) |
| `profiles/<model>.toml`, `.md` | Settings and extra instructions for one model | [Instructions and memory](/docs/instructions#per-model-instructions) |
| `evals/<name>.toml` | A prompt and a check; `evals/variants.toml` adds sets of settings to compare | [Evals](/docs/evals) |
| `instrumentation/<name>.toml` | Where to send traces (OTLP) | [Observability](/docs/observability) |

## agent.toml

Every setting has a default, so this file holds only what you change. `egin info my-agent` prints every setting the agent ends up with.

```toml
model = "anthropic/claude-sonnet-5-5"
planning = true
approval = "never"
network = []
```

## workspace/

The files the agent works on: runbooks, a knowledge base, a repository. In a microVM it is mounted at `/workspace`; a cloud sandbox (E2B, Vercel, Daytona) gets a copy; with `sandbox = "local"` tools run in this folder itself.

## .egin/

egin's own state: the SQLite database of sessions and events, OAuth tokens and the file index. Add it to `.gitignore`. Tool output too long for the conversation is saved in full under `/tmp/.egin/out/` in the sandbox, where the agent can read it.

## Which setting wins

egin merges settings in this order, and later ones win:

1. Built-in defaults
2. `profiles/<model>.toml`
3. `agent.toml`
4. `--set key=value` on the command line

Set `profile = false` in `agent.toml` to ignore the model's profile.

## What to read next

- [Instructions and memory](/docs/instructions): write the prompt.
- [Tools](/docs/tools): what the agent can run.
- [Configuration](/docs/configuration): every setting.
