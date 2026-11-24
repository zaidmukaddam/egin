---
title: Middleware and hooks
description: Block a tool call before it runs, react when a session stores an event, and send the agent back when its work isn't done.
group: Build
order: 7
---

Middleware and hooks are scripts in the agent folder that egin runs on the host, outside the sandbox. Middleware decides whether a tool call may run. Hooks run after something has happened: a tool returned, an approval is waiting, the agent answered.

## Middleware

`middleware/pre_tool` runs before every tool call, with `{"tool", "args"}` as JSON on stdin. Exit non-zero to block the call; what the script printed is shown to the model, so it can try something else.

```sh
#!/bin/sh
# middleware/pre_tool: no destructive commands without a person.
input=$(cat)
if printf '%s' "$input" | grep -Eq 'rm -rf|mkfs|dd if=|shutdown|reboot'; then
  echo "destructive command blocked by policy; ask a human"
  exit 1
fi
```

It covers every tool: built-in ones, your scripts and [MCP tools](/docs/connections). Use it for rules that must hold whatever the model decides, such as allowing only reads of one folder.

## Hooks

A file in `hooks/` named after an [event](/docs/http-api#follow-it) runs each time a session stores that event: `hooks/tool_result`, `hooks/approval_request`, `hooks/question`, `hooks/error`, and so on. The event arrives as JSON on stdin, with `session` added.

```sh
#!/bin/sh
# hooks/approval_request: tell the on-call channel a call is waiting.
curl -s -X POST "$SLACK_WEBHOOK_URL" -H 'content-type: application/json' \
  -d "$(jq -c '{text: "Waiting for approval: \(.tool) in session \(.session)"}')"
```

Hooks for one session run in order, alongside the run rather than holding it up, so they suit notifications, audit logs and metrics. They run in the agent's workspace with a two-minute limit, and without egin's own API keys in their environment.

## Keep going until it's done

`hooks/done` is different: it runs when the model has answered, before the session is marked done, with `{"type": "done", "session", "text"}` on stdin. Whatever it prints becomes the next message, and the run goes on. Printing nothing, or exiting non-zero, lets the answer stand. So a hook can check the work and send the agent back until it passes:

```sh
#!/bin/sh
# hooks/done: done means the tests pass.
cat > /dev/null
cd workspace
if ! out=$(npm test 2>&1); then
  printf 'The tests still fail. Fix them, then answer again.\n\n%s\n' "$(printf '%s' "$out" | tail -40)"
fi
```

A run repeats this at most 20 times, within `max_turns` and `max_cost`.

## What to read next

- [Sandboxing](/docs/sandboxing): limit what tools can reach, as well as what they may run.
- [Approvals and questions](/docs/approvals): put a person in front of a call instead of a rule.
- [HTTP API](/docs/http-api#follow-it): every event a hook can be named after.
