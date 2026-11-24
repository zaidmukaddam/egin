---
title: Use cases
description: What people build with egin, and the template to start each one from.
group: Start
order: 3
---

egin suits work a backend agent does on its own: answering from your documents, researching, checking systems, and taking requests from Slack, a web app or a webhook. Each kind below has a template. Start from one with `egin init my-agent --template <name>`, then change its files.

## Research agent

Answers questions with current facts, each with its source, and writes longer reports to the workspace. It remembers which sources turned out to be wrong or paywalled, so it avoids them next time.

```bash
egin init my-agent --template research
```

What makes it work:

- Web search and page reading through `web_search` and `web_fetch`, while its tools run with no network at all. See [Providers](/docs/providers#web-search).
- A report skill, loaded only when it writes a report. See [Skills](/docs/skills).
- Memory that keeps notes about sources from one session to the next. See [Instructions and memory](/docs/instructions).

## Operations agent

Follows the runbooks in `workspace/runbooks/`, checks hosts every 15 minutes, and writes an incident note when something is down. Destructive commands are blocked unless a person approves them.

```bash
egin init my-agent --template ops
```

What makes it work:

- A schedule that runs a prompt on a cron while `egin serve` runs. See [Channels and schedules](/docs/channels#schedules).
- Middleware that refuses `rm -rf` and other destructive commands. See [Middleware and hooks](/docs/middleware-and-hooks).
- Network limited to the hosts it checks, and a spend cap per session. See [Sandboxing](/docs/sandboxing#network).

## Support agent

Answers customers in Slack from a knowledge base in `workspace/kb/`, in three sentences or fewer, and names the article it used. When the articles don't cover a question, it hands it to a person instead of guessing.

```bash
egin init my-agent --template support
```

What makes it work:

- File search over the knowledge base with `search_files`. See [Tools](/docs/tools#built-in-tools).
- A Slack channel, so the agent answers in threads. See [Channels and schedules](/docs/channels#slack).
- Evals that check it escalates when it should and doesn't when it shouldn't. See [Evals](/docs/evals).

## Web chat

A Next.js chat app built on shadcn/ui, with the agent in `agent/`. It streams answers, asks before running shell commands, and lets people attach files and pick a model. `npm run dev` starts the app and the agent together.

```bash
egin init my-app --template web
```

What makes it work:

- The `egin` npm package, whose `useAgent` hook turns the agent's event stream into messages. See [Frontends](/docs/frontends).
- Approvals and questions that wait in the chat for a click or an answer. See the [HTTP API](/docs/http-api#approvals).
- The app on Vercel, talking to the agent running in a container. See [Deploy](/docs/deploy).

## Starting from nothing

The `basic` template, the default, has one example tool, two evals and two variants: the smallest agent that shows every part. [Getting started](/docs) walks through it.
