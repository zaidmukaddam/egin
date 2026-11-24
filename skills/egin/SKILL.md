---
name: egin
description: Build durable backend AI agents with egin, where an agent is a folder of files run by one binary. Use when creating, editing, running, serving or debugging an egin agent (agent.toml, instructions.md, tools/, skills/, connections/, channels/, subagents/, schedules/, instrumentation/, evals/), adding integrations with `egin add`, wiring an agent into a web app (egin/react, egin/next, egin/proxy), or deploying one.
---

# egin

egin is a filesystem-first framework for durable backend agents. An agent is a folder; the `egin`
binary runs it, serves it over HTTP, evaluates it and benchmarks it.

## Source of truth

The full documentation ships inside the binary and always matches the installed version. Read it
before writing egin code or config; don't rely on memory or on this file:

```bash
egin docs              # the list of pages
egin docs agent-folder # one page: every file egin reads
egin docs configuration
```

The same pages are at https://egin.dev/docs. If `egin` isn't installed: `npx egin@latest <command>`.

## The map

```
my-agent/
  agent.toml          model and switches (`egin docs configuration`)
  instructions.md     system prompt
  tools/<name>.py     a tool: one script, any language, described by `# description:` / `# param` comments
                      (also `# safe:`, `# durable:` with a $EGIN_STEPS journal, `# locked:` / `# unlocks:`)
  skills/<name>/SKILL.md   loaded on demand with the skill tool
  connections/<name>.toml  MCP servers
  channels/<name>.toml     webhook, Slack, Telegram, Discord, GitHub, mcp (the agent as an MCP server)
  hooks/<event>            host scripts per stored event; hooks/done's output sends the run on
  subagents/<name>/        a nested agent folder
  schedules/<name>.toml    cron prompts
  instrumentation/<name>.toml  where traces go
  evals/                   prompts and checks
  workspace/               the files the agent works on
```

## Add, don't hand-write, when the catalog has it

```bash
egin add                       # list connections, channels, exporters and tools
egin add connection/github     # write connections/github.toml, then say which variables to set
egin connect vercel            # sign in to a connection with auth = "oauth" (the user does this, in a browser)
```

Secrets never go in these files: they use `${VAR}`, read from the environment or `.env.local`.

## Check your work

```bash
egin info my-agent                      # what the folder loads to: config, tools, connections, channels
egin run my-agent "a prompt"            # one run, printing tool calls
egin eval my-agent                      # the evals, under every variant
```

`egin info` is the fastest check after any change: a typo in agent.toml, a bad tool header or an
unknown key is an error there, not at run time.
