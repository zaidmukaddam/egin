---
title: Configuration
description: Every switch agent.toml accepts, with its default.
group: Build
order: 9
---

## Model and web

| Key | Default | Meaning |
|---|---|---|
| `model` | picked for the key that is set | Any model id; `anthropic/…`, `openai/…`, `gemini/…` (or `google/…`), `bedrock/…` and `you/…` name the provider. See [Providers](/docs/providers#models) |
| `reasoning_effort` | the provider's | How hard the model thinks. Claude takes `low` to `max`, OpenAI's GPT models `none` to `max`, others `low`, `medium` or `high` |
| `max_tokens` | the provider's | Cap on output tokens per model call |
| `models` | `[]` | Other models a client may pick per session (the web chat's picker, or `model` in the HTTP API) |
| `provider` | from the model id | `you`, `openai`, `anthropic`, `gateway`, `gemini` or `bedrock`; see [Providers](/docs/providers) |
| `base_url`, `api_key_env` | the provider's | Point the provider at another endpoint or key |
| `context_limit` | from the provider | Override the model's context window |
| `vision` | from the provider | Whether the model can see images |
| `audio_model` | the session's model if it hears, else the cheapest that does | The model that transcribes audio for `read_file` |
| `web` | picked for the keys that are set | `client` gives the agent `web_search` and `web_fetch`; `server` lets the model provider search; `off` |
| `search` | the first search key set, You.com first | Web search provider: `you`, `parallel`, `firecrawl`, `context.dev`, `brave` or `searxng` |
| `search_mode` | `highlights` | You.com Search mode: `highlights` or `snippets` |
| `search_count` | 5 | Results per search |
| `freshness`, `include_domains` | | Passed to You.com Search |
| `exclude_domains` | `[]` | Never search or fetch these domains |
| `exclude_terms` | `[]` | Withhold results and pages containing any of these strings |
| `exclude_echo` | `false` | Withhold results and pages that quote the task word for word |

`exclude_terms` and `exclude_echo` exist for evals whose answers are republished online. With `exclude_terms`, list the task ids or the dataset name and the agent cannot read an answer key by accident. `exclude_echo` covers copies no list can name: a page that repeats twelve or more words of the task in a row is withheld, because a page that reproduces a benchmark question usually prints its answer. Words the task puts in quotation marks, such as a title, don't count.

## Tools and turns

| Key | Default | Meaning |
|---|---|---|
| `tool_style` | `typed` | `bash` leaves only the bash tool |
| `batching` | `true` | Several tool calls per turn; safe ones run concurrently |
| `planning` | `false` | Adds the `plan` tool |
| `memory_max` | 0 (off) | Adds the `memory` tool, capped at this many bytes |
| `index` | `false` | Adds `search_files`: full-text search over the workspace |
| `approval` | `ask` | `ask` waits for an answer before any tool not marked safe (bash, file writes, custom and MCP tools); `once` asks the first time each tool is called in a session and remembers a yes; `never` runs them without asking |
| `ask` | `false` | Adds `ask_user`: the agent puts a question to the user and waits for the answer |
| `mid_run` | `steer` | A message sent during a run: `steer` passes it to the model at its next step; `queue` holds it until the current answer is done |
| `max_turns` | 60 | Per-session turn limit |
| `max_cost` | 0 (off) | Per-session limit in dollars |
| `deadline` | 0 (off) | Seconds a run has when something outside egin will stop it, such as a job runner or a benchmark's time limit. At 60% the model is told to get a working result in place, at 85% to save its work. Under a deadline a bash call blocks for at most 180s before moving to the background, and a model call that runs past 30% of the time left is asked again at low reasoning effort. `egin bench` sets it from each task's timeout |
| `check_in` | 0 (off) | Every this many model calls, the model is asked to give its final answer if the work is in place, or to name the requirement that is still unmet and work on that only. For unattended task runs that keep testing finished work |
| `helper` | false | Adds the `helper` tool: a fresh copy of the agent, working on the same sandbox and files, answers one self-contained prompt. It gets no helper of its own, no review pass and at most 60 turns. Under a `deadline` it gets half the parent's time left and is stopped there. Its tokens and cost count toward the parent session |
| `review` | false | Before a run ends, send the model back once to check its work against the task: each requirement, edge cases, differences it saw, stray files. For unattended task runs |
| `bash_timeout` | 600 | Seconds a bash command may run in all |
| `bash_wait` | 60 | Seconds a bash call waits for its command before returning |
| `bash_background` | `true` | A command still running after `bash_wait` keeps going as a background job; `false` stops it |
| `read_before_write` | `true` | `write_file` refuses to overwrite a file the agent hasn't read, or that changed since |
| `mcp_tools` | `auto` | `listed` shows every MCP tool to the model; `deferred` puts them behind `tool_search` and `tool_call`; `auto` defers above 20 tools |
| `profile` | `true` | `false` ignores `profiles/<model>.*` |

## Context

| Key | Default | Meaning |
|---|---|---|
| `compaction` | `true` | Truncate long tool output as it enters the conversation |
| `tool_output_max` | 50000 | Characters of command output kept, from its start and end; the full text is saved in `/tmp/.egin/out/` in the sandbox, and the note says which line of it the omitted part starts on |
| `page_max` | 30000 | Characters of a page, document or search result shown at once. Longer pages and files come a part at a time |
| `elision` | `true` | Stub out old tool output once the context fills |
| `elide_at`, `keep_recent` | 0.5, 6 | Start at this share of the limit; keep this many recent results whole. Old results go in batches worth at least a tenth of the limit, since each elision makes the provider re-read the prompt from that point uncached |
| `context_focus` | 256000 | Elision and summarisation treat a larger window as this many tokens, so a 1M-token model still gets old output trimmed from 128k on; 0 uses the full window |
| `summarisation` | `true` | Summarise old turns |
| `summarise_at` | 0.75 | Start at this share of the limit |

## Sandbox

| Key | Default | Meaning |
|---|---|---|
| `sandbox` | `auto` | `auto`, `microsandbox`, `e2b`, `vercel`, `daytona`, `daytona-macos`, `docker`, `docker-cloud` or `local` |
| `image` | `python:3.13-slim` | The VM image |
| `cpus`, `memory_mb` | 2, 2048 | VM size |
| `idle_timeout` | 600 | Seconds before an idle VM stops |
| `workdir` | | Working directory inside the VM |
| `network` | `["*"]` | Egress: `["*"]` all, `[]` none, or a list of domains |
| `secrets` | `[]` | Host secrets substituted only on requests to a named host |

See [Sandboxing](/docs/sandboxing) for how each backend behaves.

## Per-model profiles

One model follows typed tools well, another does better with only bash. Put switches that suit one model in `profiles/<model>.toml`; they apply whenever that model runs, under `agent.toml`.

```toml
# profiles/muse-ember-1.0.toml
tool_style = "bash"
batching = false
```

`egin evolve` writes these files for you from eval results. See [Evals](/docs/evals).
