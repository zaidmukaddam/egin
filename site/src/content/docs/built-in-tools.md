---
title: Built-in tools
description: Every tool an agent has without writing one, with its arguments, when it is available and whether it asks for approval.
group: Reference
order: 2
---

An agent has these tools without a script in `tools/`. `bash` is always there. The rest depend on settings in `agent.toml` and on what the agent folder holds. To write your own, see [Tools](/docs/tools).

## All of them

| Tool | Does | Available | Asks for approval |
|---|---|---|---|
| [`bash`](#bash) | Runs a shell command in the sandbox | Always | Yes |
| [`read_file`](#read_file) | Reads a file, document, image or audio file | Unless `tool_style = "bash"` | No |
| [`write_file`](#write_file) | Creates or overwrites a file | Unless `tool_style = "bash"` | Yes |
| [`edit_file`](#edit_file) | Replaces one exact piece of text in a file | Unless `tool_style = "bash"` | Yes |
| [`web_search`](#web_search) | Searches the web | With `web = "client"` | No |
| [`web_fetch`](#web_fetch) | Fetches a page as Markdown or HTML | With `web = "client"` | No |
| [`ask_user`](#ask_user) | Asks the user a question and waits | With `ask = true` | No |
| [`plan`](#plan) | Records or updates the plan for the task | With `planning = true` | No |
| [`memory`](#memory) | Edits the notes kept between sessions | With `memory_max` above 0 | Yes |
| [`search_files`](#search_files) | Full-text search over the workspace | With `index = true` | No |
| [`skill`](#skill) | Loads a skill's instructions | When `skills/` has a skill | No |
| [`task`](#task) | Hands a task to a subagent | When `subagents/` has a subagent | Yes |
| [`helper`](#helper) | Hands a prompt to a fresh copy of the agent | With `helper = true` | Yes |
| [`tool_search`](#tool_search-and-tool_call) | Finds a tool from a connection | When MCP tools are deferred | No |
| [`tool_call`](#tool_search-and-tool_call) | Calls a tool found with `tool_search` | When MCP tools are deferred | As the tool it calls |
| [`final_answer`](#final_answer) | Ends the run with a value that fits a schema | When the session has an `output_schema` | No |

"Asks for approval" is what happens with `approval = "ask"`, the default. With `approval = "once"` a tool asks the first time it is called in a session, and with `approval = "never"` nothing asks. Tools that don't ask have no side effects, so several of them in one turn run at the same time. To block particular calls whatever the setting, use [middleware](/docs/middleware-and-hooks).

## Commands and files

### `bash`

| Argument | Type | |
|---|---|---|
| `command` | string, required | The shell command, run in the working directory |
| `timeout_secs` | integer | How long to wait for it. Default `bash_wait` (60), at most `bash_timeout` (600) |

Returns stdout, stderr and the exit code. A command still running when the wait ends keeps going in the background: the result gives its process group and the files its output and exit code go to, under `/tmp/.egin/jobs/` in the sandbox. The model checks on it or stops it with later `bash` calls. With `bash_background = false` the command is stopped at the wait instead.

Under a [`deadline`](/docs/configuration), a call waits at most 180 seconds, so one command can't use up the run's time. Output over `tool_output_max` characters (50,000) keeps its start and end, and the full text is saved in `/tmp/.egin/out/`.

### `read_file`

| Argument | Type | |
|---|---|---|
| `path` | string, required | The file to read |
| `offset` | integer | The line to start from, for a long result |
| `limit` | integer | How many lines to return |

Text comes back as it is. PDFs, Word, PowerPoint and spreadsheet files come back as text, images are shown to a model that can see them, and audio comes back as a transcript. [Reading documents and images](/docs/tools#reading-documents-and-images) has the full table and the size limits.

### `write_file`

| Argument | Type | |
|---|---|---|
| `path` | string, required | The file to create or overwrite |
| `content` | string, required | Its whole content |

With `read_before_write = true`, the default, it refuses to overwrite a file the agent hasn't read, or one that changed since the agent read it.

### `edit_file`

| Argument | Type | |
|---|---|---|
| `path` | string, required | The file to change |
| `old` | string, required | The exact text to replace. It must occur once |
| `new` | string, required | What replaces it |

`tool_style = "bash"` removes `read_file`, `write_file` and `edit_file` and leaves the model to do the same work through `bash`.

## Web

### `web_search`

| Argument | Type | |
|---|---|---|
| `query` | string, required | What to search for |
| `count` | integer | How many results. Default `search_count` (5) |
| `freshness` | string | `day`, `week`, `month`, `year`, or a range as `YYYY-MM-DDtoYYYY-MM-DD` |

Returns ranked results with the passages that match. Each result gets an equal share of `page_max` characters, so a long first result does not push out the rest. `exclude_domains` in `agent.toml` keeps domains out of every search and fetch.

### `web_fetch`

| Argument | Type | |
|---|---|---|
| `url` | string, required | The page to fetch |
| `offset` | integer | Where to continue a long page, from the previous result |
| `format` | `markdown` or `html` | `html` returns the page as served, for when the Markdown drops links or tables |

A page longer than `page_max` characters (30,000) comes a part at a time, and each part ends with the offset that continues it.

With `web = "server"` the agent has neither tool: the model provider searches and reads pages itself. With `web = "off"` there is no web access.

## Talking to the user

### `ask_user`

| Argument | Type | |
|---|---|---|
| `question` | string, required | The question |
| `options` | array of strings | When given, the answer is one of them |

The run pauses until someone answers, over the [HTTP API](/docs/http-api#questions) or in the terminal.

### `final_answer`

Its arguments are the session's `output_schema`. A session given a schema, with `output_schema` on the HTTP API or `--schema` on `egin run`, ends its runs by calling this tool. A value that doesn't fit the schema goes back to the model to fix. See [Structured output](/docs/http-api#structured-output).

## Planning and memory

### `plan`

| Argument | Type | |
|---|---|---|
| `steps` | array, required | Each step is `{"step": "...", "status": "pending"}`; the status is `pending`, `in_progress` or `done` |

Each call replaces the plan with the steps it gives.

### `memory`

| Argument | Type | |
|---|---|---|
| `action` | `add` or `replace`, required | `add` appends a line; `replace` rewrites the whole file |
| `text` | string, required | The line, or the new content |

Edits `memory/notes.md`, which the agent is shown at the start of later sessions. `memory_max` is the most bytes the file may hold. See [Memory](/docs/instructions#memory).

### `search_files`

| Argument | Type | |
|---|---|---|
| `query` | string, required | Words to find in file content and titles. An empty query lists the files changed most recently |
| `limit` | integer | How many results. Default 10 |

Returns each file's path, size, word count and title, with a matching snippet.

## Skills, subagents and connections

### `skill`

| Argument | Type | |
|---|---|---|
| `name` | string, required | The skill to load |

Returns the skill's instructions. See [Skills](/docs/skills).

### `task`

| Argument | Type | |
|---|---|---|
| `agent` | string, required | One of the folder's subagents |
| `prompt` | string, required | A self-contained task |

Returns the subagent's final answer. See [Subagents](/docs/subagents).

### `helper`

| Argument | Type | |
|---|---|---|
| `prompt` | string, required | A self-contained question or piece of work |

Runs a fresh copy of the agent on the same sandbox and files, with none of the conversation, and returns its answer. The copy has no helper of its own and at most 60 turns, and its tokens and cost count toward the session.

### `tool_search` and `tool_call`

| Tool | Argument | Type | |
|---|---|---|---|
| `tool_search` | `query` | string, required | A tool name, a connection name or keywords |
| `tool_search` | `limit` | integer | How many results |
| `tool_call` | `name` | string, required | The tool's full name, `<connection>__<tool>` |
| `tool_call` | `input` | object | The tool's arguments |

With more than 20 MCP tools, or with `mcp_tools = "deferred"`, the tools from [connections](/docs/connections) are not listed to the model. It finds one with `tool_search`, which returns each match's name, description and parameters, and calls it with `tool_call`. A `tool_call` asks for approval when the tool it calls would.

## What to read next

- [Tools](/docs/tools): write your own, in any language.
- [Configuration](/docs/configuration): every setting named here.
- [Sandboxing](/docs/sandboxing): where `bash` and the file tools run.
