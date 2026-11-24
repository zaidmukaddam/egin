---
title: How it works
description: Sessions, turns and tool calls, the loop that runs them, and where everything is kept.
group: Start
order: 2
---

egin runs an agent folder as a loop: it sends the conversation to a model, runs the tools the model asks for, saves everything, and repeats until the model answers.

## Sessions, turns and tool calls

Work nests in three levels:

- Session: one conversation or task. It is saved in `my-agent/.egin/egin.db`, so you can continue it later by its id.
- Turn: one message from you and all the work it triggers, until the agent answers.
- Tool call: the model asking to run something, such as a shell command, a file read or one of your scripts in `tools/`.

## The loop

Each turn goes like this:

1. egin builds the system prompt from `instructions.md`, the files in `memory/`, and a one-line description of each skill.
2. It sends the conversation and the list of tools to the model.
3. The model answers, or asks for one or more tool calls.
4. For each call, egin checks whether it needs your approval and runs [`middleware/pre_tool`](/docs/middleware-and-hooks) if you have one. Then it runs the call in the session's sandbox and saves the result.
5. Back to step 2, until the model answers or a limit stops it: `max_turns` (60 by default) or `max_cost`.

## Where tools run

Each session gets its own sandbox, created on its first tool call. That is a microVM where the machine supports one (Linux with KVM, or Apple silicon), or a cloud sandbox from E2B, Vercel, Daytona or Docker. The agent's `workspace/` is mounted inside it, and you choose which hosts it can reach. See [Sandboxing](/docs/sandboxing).

## Saving and resuming

Every message, tool call and result is saved before the loop takes the next step. If the process is killed, the session picks up from the last saved step, and no model call is ever made twice. Approval requests and questions that were waiting are still waiting. See [Durability](/docs/durability).

## One folder, every way to run it

The same folder runs in every mode, with the same tools and the same sandboxes:

| Command | What it does |
|---|---|
| `egin run` | Runs one prompt and prints the answer |
| `egin chat` | Opens a conversation in the terminal |
| `egin serve` | Serves the agent over HTTP, with its [channels](/docs/channels) and schedules |
| `egin eval` | Runs the [evals](/docs/evals) in `evals/` and reports pass or fail |
| `egin bench` | Runs a full [benchmark](/docs/benchmarks) suite |

## What to read next

- [The agent folder](/docs/agent-folder): every file egin reads.
- [Tools](/docs/tools): the built-in tools and how to write your own.
- [Configuration](/docs/configuration): every setting in `agent.toml`.
