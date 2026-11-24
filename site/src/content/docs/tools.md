---
title: Tools
description: Write a tool as one script in any language, and see the tools every agent already has.
group: Build
order: 3
---

A tool is an action the model can ask for: check a URL, query a database, send a message. Each tool is one script in `tools/`, in any language, and the file name is the tool name the model sees. The script describes itself in its first comments, reads its arguments as JSON on stdin, and whatever it prints is the result.

## Write a tool

```
my-agent/
  tools/
    check_url.py   # the model calls this tool `check_url`
```

```python
#!/usr/bin/env python3
# description: GET a URL and report the status code and latency.
# safe: true
# param url (string, required): The URL to check
import json, sys, time, urllib.request

url = json.load(sys.stdin)["url"]
start = time.time()
with urllib.request.urlopen(url, timeout=3) as r:
    status = r.status
print(json.dumps({"url": url, "status": status, "ms": int((time.time() - start) * 1000)}))
```

That's the whole tool. Run the agent and the model can call it:

```bash
egin run my-agent "Is https://egin.dev up?"
```

The same pattern in JavaScript:

```js
#!/usr/bin/env node
// description: Turn a title into a URL slug.
// safe: true
// param title (string, required): The title to slugify

let input = "";
process.stdin.on("data", (c) => (input += c));
process.stdin.on("end", () => {
  const { title } = JSON.parse(input);
  console.log(title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, ""));
});
```

Comments can start with `#`, `//` or `--`, so any language works. A script without a `description:` line is a helper the tools can call, not a tool. egin makes the script executable if it isn't.

## The header

| Line | What it does |
|---|---|
| `description: ...` | Tells the model what the tool does. Required |
| `param name (type, required): ...` | One argument. The type is `string`, `number`, `integer`, `boolean`, `array` (of strings) or `object`; leave out `required` for an optional one |
| `safe: true` | The tool only reads, so it runs without asking for approval, and alongside other safe calls |
| `durable: true` | An interrupted call is run again on resume. See [Durable tools](#durable-tools) |
| `locked: true` | Hidden until another tool or skill unlocks it. See [Locked tools](#locked-tools) |
| `unlocks: a, b` | Running this tool successfully makes those locked tools available |

For a schema the header can't express, put a JSON description next to the script with the same name, `tools/check_url.json`, and egin uses it instead:

```json
{
  "description": "GET a URL and report the status code and latency.",
  "safe": true,
  "parameters": {
    "type": "object",
    "properties": { "url": { "type": "string" } },
    "required": ["url"]
  }
}
```

A misspelt key in that file is an error, not a tool with no parameters. In JSON the other keys are `durable`, `locked` and `unlocks` (a list).

## Where a tool runs

A tool runs in the session's [sandbox](/docs/sandboxing), so its language has to be installed there. The default image, `python:3.13-slim`, has Python and `sh`. For Node, set `image = "node:24-slim"` in `agent.toml`, or use any image with what your tools need.

Inside the sandbox, `tools/` is at `/egin/tools`: mounted read-only in a microVM, copied in a cloud sandbox. With `sandbox = "local"`, tools run from the folder itself.

A tool that isn't marked `safe` asks for approval before it runs, unless `approval = "never"` in `agent.toml`. To block some calls outright, use [middleware](/docs/middleware-and-hooks).

## Durable tools

A call that was running when egin stopped is not run again on resume: the model is told its outcome is unknown and checks first. That is the safe default for a tool that charges a card or sends an email. A tool that can tell what it already did marks itself `durable: true`, and egin runs the interrupted call again instead.

egin gives every call of a durable tool its own journal file, `$EGIN_STEPS`, in the sandbox's working folder. The script notes each step there once it is done and skips the steps the journal already lists:

```sh
#!/bin/sh
# description: Charge the card, then email the receipt.
# durable: true
# param order (string, required): The order id
input=$(cat)
did() { grep -q "\"$1\"" "$EGIN_STEPS" 2>/dev/null; }
if ! did charge; then
  ./charge.sh "$input" && echo '{"step":"charge"}' >> "$EGIN_STEPS"
fi
if ! did email; then
  ./email.sh "$input" && echo '{"step":"email"}' >> "$EGIN_STEPS"
fi
echo "receipt sent"
```

The journal belongs to the call, so it survives a restart, and the next run of the same call picks it up. Safer still is a step the provider can make idempotent, with an idempotency key built from the call. See [Durability](/docs/durability).

## Locked tools

Some tools should only exist after something else has happened: refunds after the customer is verified, deploys after the tests pass. Mark such a tool `locked: true`, and name it in `unlocks:` on the tool or skill that unlocks it:

```sh
# tools/verify_customer.sh
# description: Check the customer's identity.
# unlocks: refund, change_address
```

```sh
# tools/refund.sh
# description: Refund an order.
# locked: true
```

A locked tool is left out of what the model sees until a tool that unlocks it succeeds (exits 0) in the session, or a skill that unlocks it is loaded. The result tells the model what became available. A call to a tool that is still locked gets an error, and an `unlocks` that names anything but a locked tool stops the agent from loading.

## Data for the UI

A tool can send structured data to your app alongside its result: a chart, a card, a table. Each line it prints as `::data <json>` becomes a `data` event and is left out of what the model reads:

```sh
echo '::data {"kind": "chart", "values": [3, 5, 8]}'
echo "Revenue grew 60% over three months."
```

The `egin` client adds these to the turn as `{ type: "data", tool, callId, data }` parts for your UI to draw. See [Frontends](/docs/frontends).

## Built-in tools

Every agent has some tools without writing any. [Built-in tools](/docs/built-in-tools) lists each one's arguments and whether it asks for approval.

| Tool | When it's available |
|---|---|
| `bash` | Always |
| `read_file`, `write_file`, `edit_file` | Unless `tool_style = "bash"` |
| `web_search`, `web_fetch` | With `web = "client"` |
| `plan` | With `planning = true` |
| `ask_user` | With `ask = true`: a question to the user, answered over the [HTTP API](/docs/http-api#questions) or in the terminal |
| `memory` | With `memory_max` above 0. See [Memory](/docs/instructions#memory) |
| `search_files` | With `index = true`: full-text search over the workspace |
| `skill` | When `skills/` has at least one [skill](/docs/skills) |
| `task` | When `subagents/` has at least one [subagent](/docs/subagents) |
| `tool_search`, `tool_call` | When [MCP tools](/docs/connections) are deferred: more than 20 of them, or `mcp_tools = "deferred"` |
| `helper` | With `helper = true`: sends a self-contained prompt to a fresh copy of the agent on the same sandbox and files, and returns its answer |

`bash` waits up to `bash_wait` seconds (60 by default). A command still running then keeps going in the background, and the result says where its output will appear.

### Reading documents and images

`read_file` reads more than text:

| File | What the model gets |
|---|---|
| Text, code, CSV, JSON | The file as it is |
| PDF | The text of each page, marked `--- page N of M ---` |
| Word (`.docx`) | The text, one paragraph per line |
| PowerPoint (`.pptx`) | The text of each slide, in order |
| Excel and OpenDocument spreadsheets | Each sheet as CSV |
| Images (`.png`, `.jpg`, `.gif`, `.webp`) | The image itself, when the model can see images |
| Audio (`.mp3`, `.wav`, `.m4a`, `.ogg`, `.flac`, `.aac`) | A transcript |
| Other binary files | A note saying so, instead of garbled bytes |

Documents are converted on the host, so the sandbox needs no extra packages. Files up to 20 MB are read; images over 5 MB are refused. Whether the model can see images comes from the provider's model list; for an endpoint that doesn't say, set `vision = true`. Audio is transcribed by a model that takes audio, the session's own if it can, and billed to the session. Set `audio_model` to choose one.

### Long results

A page, a document or a set of search results enters the conversation up to `page_max` characters (30,000 by default):

- `web_fetch` and `read_file` return a longer result a part at a time. Each part ends with the offset that continues it.
- `web_search` gives each hit an equal share, so a long first result does not push out the rest.
- `web_fetch` with `format: "html"` returns the page's HTML as served, for when the Markdown drops links or tables the agent needs.
- Command output over `tool_output_max` characters (50,000 by default) keeps its start and end.

Whatever was cut is saved in full under `/tmp/.egin/out/` in the sandbox, where the agent can search it with `grep`. When command output is cut, its note names the line of the saved file where the omitted part starts, so `read_file` can continue from there.

## Ready-made tools

`egin add tool/board` writes six tools that work a [Vercel Board](https://vercel.com/docs/board) through the Vercel CLI, so the agent can take part in work shared across agents and people:

| Tool | Does |
| --- | --- |
| `board_read` | Lists a channel's latest threads, or reads one thread in full |
| `board_post` | Starts a thread in a channel, or replies in a thread |
| `board_tasks` | Lists tasks, open ones by default |
| `board_task_claim` | Claims a task by its ID, or the oldest ready task in a channel |
| `board_task_update` | Completes, fails, blocks, releases or renews a task the agent holds |
| `board_schedule` | Posts later, once or repeating, such as a reminder to check CI |

The two that only read run without approval. Sign in with `vercel login`, or set `BOARD_TOKEN`, and set `BOARD_PROFILE` to the profile the agent writes as. To reach the agent from the board while `egin serve` runs, add the [Board channel](/docs/channels#vercel-board) too.

## When to use something else

- To give the agent the tools of an existing service, such as GitHub or Linear, add a [connection](/docs/connections) instead of writing them.
- For written steps the agent follows, not code it runs, write a [skill](/docs/skills).
- To check or block calls before they run, use [middleware](/docs/middleware-and-hooks).

## What to read next

- [Sandboxing](/docs/sandboxing): where tools run and what they can reach.
- [Skills](/docs/skills): procedures the agent loads when it needs them.
- [Durability](/docs/durability): what happens to a call when the process dies.
