---
title: Durability
description: What happens when a run is killed halfway, and why no model call is ever repeated.
group: Run
order: 2
---

Durability means a run survives its process dying: a crash, a redeploy, a laptop lid closing. egin saves every step of a session before it takes the next one, so a session picks up where it stopped, a model call that already finished is never paid for twice, and approvals and questions that were waiting are still waiting. There is nothing to set up; it is how every session runs.

## Stored before it moves on

Sessions live in `<agent>/.egin/egin.db`, a SQLite database, or in a database every egin process shares when `EGIN_STORE_URL` is set (see [Deploy](/docs/deploy#several-instances-and-serverless)). Every user message, model reply, tool call and tool result is written there before the loop takes the next step.

The loop works only from those stored messages and keeps no state in memory that the database lacks, so when a process dies, only the step in flight is lost.

## Resuming

`egin serve` resumes interrupted sessions when it starts. From the command line, resume one with its id:

```bash
egin run my-agent --session <id>
```

A completed model call is never made twice: its reply is already stored, so the loop picks up from the first step that did not finish.

## Side effects of an interrupted call

The hard case is a tool call that was running when the process died. It may have finished, half-finished, or never started.

egin does not re-run it blindly. It tells the model the call was interrupted and its effects are unknown, and the model checks the state first: lists the directory, reads the file, queries the service. Then it decides what to do. Read-only tools have no effects to worry about, so egin runs them again. It does the same for [durable tools](/docs/tools#durable-tools): each keeps a journal of the steps it finished, and skips them when it runs again.

## Approvals survive too

An approval request is stored like everything else. If the server restarts while a request waits for an answer, the request is still there. The answer is stored when it comes, even if it comes before the run resumes. The same holds for `ask_user` questions, and for messages sent during a run: they wait in the session's inbox until the run takes them up.

## Several processes, one store

Several egin processes can share one agent folder, for example `egin serve` and an `egin eval` run. SQLite serialises their writes; a writer waits up to 60 seconds for the lock. Processes on different machines share a store through `EGIN_STORE_URL` instead.

A session has one runner at a time. The process running it holds a lease and renews it every 10 seconds. Another process takes the session over only once the lease has lapsed for 30 seconds, and taking it is a single statement in the store, so only one process succeeds.

`egin serve` takes over lapsed sessions when it starts, when a request arrives about a session whose runner is gone, and on `POST /resume`. So a session interrupted moments before a restart resumes up to 30 seconds later.

A runner can be paused instead of killed: a laptop asleep, or a serverless instance frozen between requests. Such a runner finds at its next renewal that the session is no longer its own, and stops without writing to it. Cancelling works the same way from any process: the runner sees the session cancelled at its next renewal and stops.

## Subagents

A subagent's session runs inside its parent's. Cancelling the parent cancels any subagent it is running. After a restart, egin doesn't resume a subagent's session on its own: the parent resumes, its interrupted `task` call tells the model the subagent didn't finish, and the model decides whether to start it again.

## What to read next

- [Tools](/docs/tools#durable-tools): make a tool safe to run again after an interruption.
- [HTTP API](/docs/http-api#follow-it): follow a session's events, and pick up again after a dropped connection.
- [Sandboxing](/docs/sandboxing): what happens to a session's sandbox after a crash.
