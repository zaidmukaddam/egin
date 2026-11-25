---
title: Subagents
description: Hand a self-contained task to another agent, with its own instructions, tools and model.
group: Build
order: 6
---

A subagent is a second agent this one can hand a task to: a researcher for a writer, a reviewer for a coder. Each subagent is a complete agent folder inside `subagents/`, with its own instructions, tools and model. The parent sends it work with the `task` tool and gets back its final answer.

## Add a subagent

```
my-agent/
  instructions.md
  subagents/
    reviewer/         # the parent can hand tasks to `reviewer`
      agent.toml
      instructions.md
      tools/
```

The subagent folder is an agent like any other, so you can run it on its own with `egin run my-agent/subagents/reviewer "..."` while you build it.

The parent's prompt names its subagents and tells the model to use the `task` tool for self-contained work. The model writes the task as a prompt, so give each subagent instructions that make sense without the parent's conversation.

## How it runs

- The subagent runs as its own session, saved and resumable like any other, inside its parent's.
- It gets a [sandbox](/docs/sandboxing) of its own, named for its own session.
- It inherits what the parent was run with (`--model`, `--set`, an eval variant, the model a session picked) for every setting its own `agent.toml` leaves out. A subagent that names its own model keeps it.
- Cancelling the parent cancels any subagent it is running.
- Its approval requests arrive on its own session's event stream; the `egin` client follows them for you. See the [HTTP API](/docs/http-api#approvals).
- After a restart, egin doesn't resume a subagent on its own: the parent resumes, its interrupted `task` call tells the model the subagent didn't finish, and the model decides whether to start it again.

## Subagent or helper

For a fresh copy of the same agent rather than a different one, set `helper = true` instead: the `helper` tool hands a prompt to a copy of this agent on the same sandbox and files. See [Tools](/docs/tools#built-in-tools).

## What to read next

- [Durability](/docs/durability): what happens to sessions when the process dies.
- [Tools](/docs/tools): the built-in tools, `task` and `helper` among them.
