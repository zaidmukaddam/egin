---
title: Approvals and questions
description: Choose which tool calls wait for a person, answer them from the terminal, the HTTP API or a chat, and let the agent ask the user a question.
group: Build
order: 8
---

An approval is a pause before a tool call that changes something: the call waits until a person allows or denies it. A question is the agent asking the user for a decision or a fact only they have. Both are stored with the session, so a request that is waiting when egin restarts is still waiting afterwards.

## Which calls ask

`approval` in `agent.toml` sets the rule:

| `approval` | What happens before a call to a tool that isn't safe |
|---|---|
| `ask` (default) | Every call waits for an answer |
| `once` | The first call to each tool in a session waits; a yes holds for that tool's later calls |
| `never` | Nothing waits |

A safe tool never asks, whatever the setting. Safe tools only read, so several of them in one turn also run at the same time:

- Built-in tools that only read: `read_file`, `web_search`, `web_fetch`, `search_files`, `skill`, `plan` and `tool_search`. [Built-in tools](/docs/built-in-tools) marks each one.
- Your tools with `safe: true` in their [header](/docs/tools#the-header).
- MCP tools named in a connection's [`safe` list](/docs/connections).

Everything else asks: `bash`, `write_file`, `edit_file`, `memory`, `task`, `helper`, your tools without `safe: true`, and MCP tools that aren't listed.

```toml
# agent.toml
approval = "once"
```

## Answer a request

How a request reaches a person depends on how the agent runs:

| How it runs | How a request is answered |
|---|---|
| `egin run`, `egin chat` | In the terminal: `allow bash {...}? [y/N]`. `--yes` approves every call for that run |
| `egin serve` | An `approval_request` event on the session's stream, answered with a POST |
| The web template, or the `useAgent` hook | A card in the chat; `approve(callId, allow)` in your own UI. See [Frontends](/docs/frontends) |
| Channels and schedules | Over the HTTP API, like `egin serve`. The session doesn't reply until its requests are answered |
| Evals and benchmarks | Every call is approved |

Over HTTP, the request includes a `call_id`, the tool, its arguments and the `policy` in force, so a UI can say whether a yes covers this call or the tool's later ones too:

```bash
curl -X POST localhost:8787/sessions/<id>/approvals \
  -H 'content-type: application/json' \
  -d '{"call_id": "<call id>", "allow": true}'
```

An agent that only talks through channels has nobody watching the stream. Set `approval = "never"` for it, or mark its tools safe, and put the rules that matter in [middleware](#rules-that-need-no-person).

## What a denial does

A denied call is not run. The model gets `error: the user denied this tool call` as the call's result and continues, so it can try another way or explain what it couldn't do.

## Subagents

A subagent asks on its own session. Its `approval_request` arrives on the stream of the session named in the parent's `subagent` event, and is answered at `POST /sessions/<subagent session>/approvals`. The `egin` client and the web template follow subagent sessions for you and show the request under the step that handed off to the subagent.

## Rules that need no person

An approval puts a person in front of each call. For a rule that should hold without one, use [`middleware/pre_tool`](/docs/middleware-and-hooks): it sees every call, safe or not, and blocks the ones it refuses whatever `approval` is set to. An approved call still goes through middleware.

To tell someone a call is waiting, add a [`hooks/approval_request`](/docs/middleware-and-hooks#hooks) script that posts to a channel.

## Questions

With `ask = true`, the agent has an [`ask_user`](/docs/built-in-tools#ask_user) tool. A call to it emits a `question` event with a `call_id`, the question and any options, and the run waits:

```bash
curl -X POST localhost:8787/sessions/<id>/answers \
  -H 'content-type: application/json' \
  -d '{"call_id": "<call id>", "answer": "staging"}'
```

The answer is one of the options or free text. With `egin run` and `egin chat` the question is asked in the terminal. Where nobody can answer, as with `egin run --yes` and evals, the tool tells the model to decide for itself and say what it assumed.

## After a restart

Requests and answers are stored like every other event:

- A request that was waiting is still waiting, and is sent again on the stream.
- An answer that arrives before the server has picked the session up again is stored, and the run uses it when it resumes.
- A decision made before the restart still stands; the person isn't asked twice.

[Durability](/docs/durability) covers the rest of what survives.

## What to read next

- [Middleware and hooks](/docs/middleware-and-hooks): block calls by rule, and react to a waiting request.
- [HTTP API](/docs/http-api#approvals): the routes and events in full.
- [Security](/docs/security): the other limits on what an agent can do.
