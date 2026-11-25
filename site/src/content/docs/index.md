---
title: Getting started
description: Create an agent, give it a model key and run your first prompt.
group: Start
order: 1
---

An egin agent is a folder of plain files: what it should do, the tools it can use, and a few settings. The `egin` binary runs that folder, in your terminal, as an HTTP server, in Slack or as a benchmark run.

## Prerequisites

- An API key for a model: Anthropic, OpenAI, the Vercel AI Gateway, Amazon Bedrock or You.com.
- Node.js for `npx`, or Rust's `cargo` to install the binary instead (see [Other ways to install](#other-ways-to-install)).

## Create an agent

```bash
npx egin-ts@latest init my-agent
```

`npx` doesn't put `egin` on your `PATH`, so the commands on this page all start with `npx egin-ts@latest`. A bare `egin` command fails with `egin: not found` or `command not found: egin` until you install it with `npm install -g egin-ts` or Cargo. After that, type `egin` instead.

This writes a starter folder:

```
my-agent/
  agent.toml        settings: the model, and anything else you change
  instructions.md   what the agent is and how it works, in plain words
  tools/            one script per tool, in any language
  skills/           procedures it reads only when it needs them
  memory/           notes it keeps from one session to the next
  evals/            prompts with a check, to test it
  workspace/        the files it works on
```

## Add a key

Set the key for your model provider, in your shell or in a `.env.local` file in the folder you run egin from:

```bash
export ANTHROPIC_API_KEY=...
```

egin picks a model for the key it finds. To see what it picked and whether anything is missing:

```bash
npx egin-ts@latest info my-agent
```

## Run it

```bash
npx egin-ts@latest run my-agent "say hello"
```

```
Hello! I'm ready to help. What would you like me to work on?
[session 660c9c4cd517400 · 1 model calls · 0 tool calls · 7382 prompt tok (0 cached) · 23 completion tok · $0.0150 · 1.9s]
```

The last line is the run's receipt: its session id, how many model and tool calls it made, and what it cost. Every session is saved, so you can continue it:

```bash
npx egin-ts@latest run my-agent --session 660c9c4cd517400 "now write it in French"
```

For a back-and-forth conversation, use `npx egin-ts@latest chat my-agent`.

Before running a tool that isn't marked safe, such as a shell command, egin asks you first. Pass `--yes` to approve everything for one run.

## Next steps

1. Change what it does: edit `instructions.md` and run it again. See [Instructions and memory](/docs/instructions).
2. Give it a tool: put a script in `tools/`. See [Tools](/docs/tools).
3. Check that it works: run `npx egin-ts@latest eval my-agent`. See [Evals](/docs/evals).
4. Put it online: run `npx egin-ts@latest serve my-agent`. See the [HTTP API](/docs/http-api), then [Deploy](/docs/deploy).

To start from something closer to what you want to build, pick a template: `npx egin-ts@latest init my-agent --template research`. [Use cases](/docs/use-cases) shows each one. Then go to the page for what you need:

| Goal | Read |
|---|---|
| Understand what happens during a run | [How it works](/docs/how-it-works) |
| Give the model code it can run | [Tools](/docs/tools) |
| See the tools it already has | [Built-in tools](/docs/built-in-tools) |
| Teach it a procedure it loads only when needed | [Skills](/docs/skills) |
| Connect GitHub, Linear or another MCP server | [Connections](/docs/connections) |
| Hand part of the work to another agent | [Subagents](/docs/subagents) |
| Block risky calls, or react when something happens | [Middleware and hooks](/docs/middleware-and-hooks) |
| Make a person approve calls that change something | [Approvals and questions](/docs/approvals) |
| Choose a model or provider | [Providers](/docs/providers) |
| Reach it from Slack, Discord, GitHub or a webhook | [Channels and schedules](/docs/channels) |
| Give it a web chat | [Frontends](/docs/frontends) |
| Limit what its tools can touch | [Sandboxing](/docs/sandboxing) |
| Check it before it goes to production | [Security](/docs/security) |

## Other ways to install

`pnpm dlx` and `bunx` work like `npx`. To keep `egin` on your `PATH`, install it globally with npm or Cargo:

```bash
npm install -g egin-ts
```

```bash
cargo install egin
```

The rest of these docs write commands as `egin ...`. Without a global install, put `npx egin-ts@latest` in place of `egin`.

To build from source, run `cargo build --release` in the repository. The binary is written to `target/release/egin`.

What changed in each release is in the [changelog](/changelog), the same file as `CHANGELOG.md` in the repository. Release posts are on the [blog](/blog).

*egin* is Basque for "to do, to make".
