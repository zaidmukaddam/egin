---
title: Observability
description: JSON logs and OpenTelemetry traces for every session.
group: Run
order: 6
---

Observability is seeing what an agent did and why: every model call, tool call and approval, with its time and cost. egin shows it in four ways, least setup first:

| Way | Good for |
|---|---|
| [JSON logs](#json-logs) | Shipping every event to the log tool you already use |
| [Traces](#traces) | Timelines in Honeycomb, Langfuse, Axiom, Sentry or Jaeger, over OTLP |
| [Trace view](#trace-view) | Looking at one session in the web template, live |
| [The database](#inside-the-database) | Querying sessions, stats and messages yourself |

## JSON logs

```bash
EGIN_LOG=json egin serve my-agent
```

Every stored event is written to stderr as one JSON line: model replies, tool calls and results, approvals, errors. Pipe it into whatever already collects your logs. egin's own messages, such as "resuming session" or a schedule starting, are plain text on the same stream, so keep the lines that start with `{`.

## Traces

Set an OTLP endpoint and egin exports spans over OTLP/HTTP:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 egin serve my-agent
```

`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` works too, and `OTEL_EXPORTER_OTLP_HEADERS` (or `OTEL_EXPORTER_OTLP_TRACES_HEADERS`) adds headers as `key=value,…`, for a backend that needs a key.

To set trace destinations in the agent folder, add a file to `instrumentation/`. Each file adds one destination, alongside any the environment sets:

```toml
# instrumentation/honeycomb.toml
endpoint = "https://api.honeycomb.io/v1/traces"
headers = { x-honeycomb-team = "${HONEYCOMB_API_KEY}" }
```

`${VAR}` reads the host's environment when a span is sent, so the file holds no secret. `egin add instrumentation/honeycomb` writes this one; the catalog also has Sentry, Langfuse, Axiom and a local Jaeger. `egin info` lists where traces go.

Each session is one trace: a `session.run` span for each run, and under it a span per model call and per tool call, so you can see which tool was slow or which turn cost the most. A subagent is a session of its own, so it exports a trace of its own.

## Trace view

The web template (`egin init my-chat --template web`) lists every session at `/traces`, and shows each one's trace at `/traces/<session id>`. A trace is one timeline with each model call, tool call and subagent, their time and cost, and the time spent waiting for an approval. It updates live while the run goes on. The chat's header links to the current conversation's trace.

The view reads the session's own event stream, so it needs nothing else running. Every stored event includes `at`, the time it happened in Unix milliseconds. Each `assistant` event also includes the call's `model`, `cost` in dollars, and `ms`, how long the call took.

## Inside the database

Everything is also in `.egin/egin.db`. `GET /sessions/{id}` returns a session's status, token and cost stats, and its messages.

## What to read next

- [HTTP API](/docs/http-api#follow-it): every event type and what it includes.
- [Evals](/docs/evals): measure the agent across many runs, not one.
