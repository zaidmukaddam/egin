---
title: Instructions and memory
description: Write the agent's prompt in instructions.md, keep notes across sessions in memory/, and add instructions for one model.
group: Build
order: 2
---

Instructions are what you tell the model: who the agent is, how it should work, and what it must never do. They live in `instructions.md` at the root of the agent folder, as plain Markdown, and go to the model as the system prompt on every call.

## Write instructions

The smallest instructions are a few sentences:

```md
You are a support agent. Answer from the articles in `kb/` and nothing else.
Name the article you used. If no article covers the question, say so.
```

Write rules the agent can follow on every task: its role, where its files are, how to check its work, what to reply with. A procedure it needs only now and then belongs in a [skill](/docs/skills) instead, where it costs nothing until it is used.

egin adds to your text, in this order:

1. The model's own instructions from `profiles/<model>.md`, if that file exists.
2. The contents of every `.md` file in `memory/`.
3. One line per [skill](/docs/skills) and the names of any [subagents](/docs/subagents).
4. The working directory the agent's commands run in.

The prompt is built once when a session starts and stays the same for that session, so the model provider can cache it. An edit to `instructions.md` reaches new sessions, not ones already running.

## Memory

Every `.md` file in `memory/` is added to the prompt, so it suits facts every session should know: who the users are, what was decided, which sources were wrong.

To let the agent keep its own notes, set a size limit:

```toml
# agent.toml
memory_max = 6000
```

The agent then has a `memory` tool that edits `memory/notes.md`, up to that many bytes. What it writes there is in the prompt of every later session.

## Per-model instructions

Models differ. One needs reminding to check its work, another to stop after it answers. Put instructions for one model in `profiles/<model>.md` and settings in `profiles/<model>.toml`, named exactly after the model id. A model id with a slash is a folder: `anthropic/claude-sonnet-5-5` reads `profiles/anthropic/claude-sonnet-5-5.md`.

```toml
# profiles/muse-ember-1.0.toml
tool_style = "bash"
batching = false
```

They apply whenever that model runs, under what `agent.toml` sets. `egin evolve` writes these files for you from eval results; see [Evals](/docs/evals).

## What to read next

- [Skills](/docs/skills): procedures the agent loads only when it needs them.
- [Tools](/docs/tools): what the agent can run.
- [Evals](/docs/evals): check that a change to the instructions made things better.
