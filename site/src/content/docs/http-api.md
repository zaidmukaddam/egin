---
title: HTTP API
description: Start sessions, follow them over server-sent events, steer a run, and answer approvals and questions.
group: Run
order: 3
---

The HTTP API is how a program talks to an agent: start a session, send it messages, follow what it does as a stream of events, and answer its approvals and questions. `egin serve` serves one agent folder over HTTP:

```bash
egin serve my-agent --port 8787
```

For a web app, the `egin` npm package speaks this API for you; see [Frontends](/docs/frontends). This page is for calling it directly, from any language.

## Authentication

When `EGIN_TOKEN` is set, every route except `/health` and the channel routes needs `Authorization: Bearer <token>`. Requests without it get `401`. The channel routes keep their own signature checks.

```bash
EGIN_TOKEN=... egin serve my-agent --host 0.0.0.0
```

## Users

When many people share one agent, have your app's server send `X-Egin-User: <user id>` on every request, after its own sign-in check. A session created with that header belongs to that user, and every session route then reaches only that user's sessions. Someone else's session answers `404`, the same as one that doesn't exist, and `GET /sessions` lists only theirs. Requests without the header are the operator's and reach every session.

The browser must never set the header itself: your server sets it, and `EGIN_TOKEN` keeps anyone else from calling egin directly.

By default `egin serve` listens on `127.0.0.1` only. It refuses any other `--host` unless `EGIN_TOKEN` is set, because anyone who can reach the port could run the agent and approve its tool calls.

## Routes

| Route | Body | Result |
|---|---|---|
| `GET /sessions?limit=50` | | `{"sessions": [{"id", "status", "created", "title"}]}`, newest first, titled by the first message |
| `GET /agent` | | `{"model", "models", "reasoning_effort", "reasoning_efforts"}`: what a client may pick; `reasoning_efforts` maps each model to the efforts it takes |
| `POST /sessions` | `{"message"?, "model"?, "reasoning_effort"?, "output_schema"?}`, or no body | `{"id"}`; starts a run when a message is given |
| `POST /sessions/{id}/messages` | `{"message", "files"?, "model"?, "reasoning_effort"?}` | Continue a session; `files` are uploads from the route below. Sent while it runs, the message joins that run and the result is `{"ok": true, "queued": true}` (see [Messages during a run](#messages-during-a-run)) |
| `POST /sessions/{id}/files?name=<name>` | The raw file | `{"name", "path", "size"}`; saved under `uploads/<session>/` in the workspace, up to 25 MB |
| `GET /sessions/{id}` | | Status, stats and messages |
| `GET /sessions/{id}/events?after=<id>` | | Server-sent events |
| `GET /sessions/{id}/trajectory` | | The session as an ATIF trajectory (see `egin trajectory`) |
| `GET /sessions/{id}/history?before=<id>&limit=500` | | `{"events", "earlier"}`: a page of the stored log, oldest first, starting at a turn; `earlier` is the `before` for the next older page, or null |
| `POST /sessions/{id}/approvals` | `{"call_id", "allow"}` | Answer an approval request |
| `POST /sessions/{id}/answers` | `{"call_id", "answer"}` | Answer a question from `ask_user` |
| `POST /mcp` | JSON-RPC | The agent as an MCP server, with `channels/mcp.toml` (`GET /mcp` answers 405: there is no event stream); see [Channels](/docs/channels#mcp-server) |
| `POST /sessions/{id}/cancel` | | Stop a running session |
| `DELETE /sessions/{id}` | | Stop it, remove its sandbox, delete its records |
| `POST /resume` | | `{"resumed"}`: continue every session that was still running when its process stopped. Call it from a cron job on a host where no process stays up (see [Deploy](/docs/deploy#several-instances-and-serverless)) |
| `GET /health` | | `ok`, with no token needed, for a host's health check |

A `model` must be the agent's own or one listed in its `models`. A `reasoning_effort` must be one the model takes: `low` to `max` for Claude, `none` to `max` for OpenAI's GPT models (both when called directly, not through the AI Gateway), and `low`, `medium` or `high` for the rest. Anything else is a `400`.

Either one stays with the session for later turns until a message changes it. Switching to a model that does not take the session's effort drops the effort.

## Start a session

```bash
curl -X POST localhost:8787/sessions \
  -H 'content-type: application/json' \
  -d '{"message": "Check the HTTP health of every target."}'
```

## Follow it

```bash
curl -N localhost:8787/sessions/<id>/events
```

The stream replays the stored log first, then sends live events as they happen. Reconnect with `?after=<last event id>` to pick up exactly where you left off.

| Event | Meaning |
|---|---|
| `user` | A message the session received, as the model sees it |
| `queued` | A message sent during a run, waiting for the run to take it up; its `user` event follows when it does |
| `question` | `ask_user` put a question (`question`, `options`) and waits for an answer |
| `answer` | The answer to a question |
| `hook` | `hooks/done` sent the run on with a new message |
| `data` | Structured data a tool sent for the UI (`::data` lines), with its `call_id` |
| `unlocked` | A tool or skill made locked tools available (`by`, `tools`) |
| `output` | A `final_answer` that fit the session's `output_schema` |
| `token` | A piece of the reply as it streams. Not stored, so it has no `id` and is not replayed |
| `reasoning` | A piece of the model's reasoning, where the model shows it. Not stored |
| `assistant` | A complete model reply, with its tool calls |
| `tool_call` | The agent called a tool |
| `approval_request` | A tool call is waiting for an answer |
| `approval` | The answer to an approval request |
| `tool_started` | A tool call began running |
| `tool_result` | The tool's result |
| `file_seen` | The agent looked at an image or document |
| `subagent` | A subagent started; `session` is its own session. Its answer comes back as the `task` call's `tool_result` |
| `web_search`, `web_extract` | The You.com router searched or read a page on the model's behalf (`web = "server"`) |
| `elided` | Old tool output was stubbed out to make room |
| `summarised` | Old turns were summarised to make room |
| `wrap_up` | A turn or spend limit was reached; one last answer follows |
| `deadline` | Time is running out under `deadline` (`left_secs`, `kind`) |
| `model_timeout` | A model call ran past its time cap under `deadline` and was asked again at low effort (`secs`) |
| `review`, `independent_review`, `test_guard` | With `review = true`: the check before the answer and what it found |
| `done` | The run finished |
| `error` | The run failed |
| `cancelled` | The run was stopped |

A run ends with exactly one of `done`, `error` or `cancelled`, so a client that follows a turn should stop on any of the three.

Every event includes `at`, when it happened in Unix milliseconds. An `assistant` event also includes `model`, `cost` in dollars, and `ms`, how long the model call took.

## Structured output

Give a session `output_schema`, a JSON Schema, and its runs end with a value that fits it instead of free text:

```bash
curl -X POST localhost:8787/sessions -H 'content-type: application/json' -d '{
  "message": "Triage this alert: disk at 97% on db-1",
  "output_schema": {"type": "object", "required": ["severity", "summary"],
    "properties": {"severity": {"enum": ["low", "high"]}, "summary": {"type": "string"}}}}'
```

The model gets a `final_answer` tool whose parameters are the schema. egin checks its value (types, required fields, enums, nested objects and arrays) and sends one that doesn't fit back to the model with what is wrong. A plain-text answer is sent back at most twice.

The run's `done` event then includes the value as `output`, and its `text` is the same value as JSON. The schema stays with the session for later turns; a message with `"output_schema": null` clears it. From the command line: `egin run my-agent "..." --schema schema.json` prints the JSON.

## Messages during a run

egin accepts a message sent while the session runs. It stores the message, emits `queued`, and adds it to the run the way `mid_run` in `agent.toml` says:

- `steer` (the default): the model sees it at its next step, after the tool calls in flight have their results. Use it to correct course: "also check the logs", "skip the tests".
- `queue`: the model finishes its current answer first, then takes the message as the next turn, in the same run.

Either way the run ends with one `done`. A message is stored before it is acknowledged, so a restart doesn't lose it. A change of `model` or `reasoning_effort` is refused (`409`) while the session runs.

## Approvals

[Approvals and questions](/docs/approvals) explains which calls ask and every way to answer; this section is the HTTP side. With `approval = "ask"`, a call to an unsafe tool emits `approval_request` with a `call_id` and waits. With `approval = "once"`, only the first call to each tool in a session asks; a yes holds for that tool's later calls. The request includes `policy` so a UI can say which. Answer it:

```bash
curl -X POST localhost:8787/sessions/<id>/approvals \
  -H 'content-type: application/json' \
  -d '{"call_id": "<call id>", "allow": true}'
```

The answer is stored, so it survives a restart. An answer that arrives after a restart but before the server has picked the session up again is stored too, and the run uses it when it resumes.

A subagent asks on its own session: its `approval_request` arrives on the stream of the session named in the parent's `subagent` event, and is answered at `POST /sessions/<subagent session>/approvals`. The subagent's session belongs to the parent's user. The `egin` client does this for you; see [Frontends](/docs/frontends).

## Questions

With `ask = true` in `agent.toml`, the agent has an `ask_user` tool for a decision or a fact only the user has. It emits `question` with a `call_id`, the `question` and any `options`, and waits:

```bash
curl -X POST localhost:8787/sessions/<id>/answers \
  -H 'content-type: application/json' \
  -d '{"call_id": "<call id>", "answer": "staging"}'
```

The answer can be one of the options or free text. Like approvals, it is stored and survives a restart. With `egin run`, the question is asked in the terminal. Where no one can answer (`egin run --yes`, evals), the tool tells the model to decide for itself and say what it assumed.

## What to read next

- [Frontends](/docs/frontends): build a chat on this API with `useAgent`.
- [Channels and schedules](/docs/channels): reach the agent from Slack, GitHub or a webhook instead.
- [Deploy](/docs/deploy): run the server somewhere other people can reach it.
