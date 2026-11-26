# Changelog

Every release of the `egin` binary and the `egin-ts` npm package, newest first.

## 0.1.6

November 26, 2026

- The marketing template's `writer` has a `programmatic-seo` skill: one page pattern, such as `<product> vs <competitor>`, filled in from a table of sourced facts, one page per row. It drops rows it can't fill with real facts, writes three sample pages for you to check before the rest, then a hub page that links them all.
- Under `deadline`, the low-effort retry after a `model_timeout` no longer waits out the task's time when its stream goes dead. It waits up to 2 minutes for its first output, then cuts after 60 seconds of silence and asks again, emitting `model_stalled`. After three in a row the turn fails.
- Tool-call arguments as they stream count as progress, so a model slowly writing a large file through a tool is not cut as stalled.

## 0.1.5

November 25, 2026

- A marketing template: `egin init my-team --template marketing` writes a lead agent and three specialists, `positioning`, `writer` and `social`, that draft launch posts, blog posts and social posts into `workspace/drafts/`. Its middleware refuses a draft with an em dash, a hashtag, a hype phrase, a word from the brand's "Never say" list or an X post over 280 characters, and tells the model what to fix. Nothing is published.
- A subagent runs with the model and settings you gave the parent (`--model`, `--set`, an eval variant, the model a session picked) unless its own `agent.toml` sets them. Subagents used to ignore them and fall back to their own defaults, so an eval of one model ran its subagents on another.
- `egin init` marks a subagent's `tools/` and `middleware/` scripts executable, as it already did the top-level agent's.

## 0.1.4

November 25, 2026

- Under `npx`, `egin init` suggests `npx egin-ts@latest run ...` as the next step, and the getting-started page writes every command that way. The bare `egin` they printed before isn't on `PATH` after `npx`, so the second command failed with `egin: not found`.
- On a platform with no prebuilt binary, the npm launcher points at `cargo install egin`.

## 0.1.3

November 25, 2026

- `egin chat` shows the right session cost after Ctrl+R resumes a session. Replaying the transcript added every model call's cost a second time, so a resumed session showed double what it had spent. Nothing was billed twice.
- `egin chat` says "1 line" instead of "1 lines" on approval cards, collapsed output, pastes and the scroll indicator.
- egin is on crates.io: `cargo install egin` puts it on your `PATH`.

## 0.1.2

November 24, 2026

- HTTPS works to the hosts in a `network = [...]` allowlist on the local microVM. microsandbox's strict hostname check refused every TLS connection to an allowed domain unless it could read the request, so egin now turns on TLS interception for domain allowlists. The check that stops domain fronting stays on, and microsandbox points Python, curl, pip and Node at its certificate. `network = []` and `network = ["*"]` are unchanged.
- The ops template's health check reports a healthy site as OK. It reported every HTTPS target as DOWN because of the bug above.
- The web template shows a link to a file the agent wrote, such as `notes.md`, as the file's name instead of "[blocked]".

## 0.1.1

November 24, 2026

- The web template's approval card names the action it asks about: "Write notes.md?" or "Edit app.ts?" instead of "Run" for every tool.
- On a platform with no prebuilt binary, the npm launcher now points at `cargo install --git https://github.com/zaidmukaddam/egin`. egin is not on crates.io, so the old `cargo install egin` failed.
- The npm package links to this repository.

## 0.1.0

November 24, 2026

The first public release.

- One Rust binary runs an agent folder (`agent.toml`, `instructions.md`, `tools/`, `skills/`, `evals/`) as a CLI, a chat, or an HTTP server with `egin serve`.
- Sessions are durable: every step is saved before the next one, so a killed run resumes where it stopped and never repeats a finished model call.
- Models from the You.com router, OpenAI, Vercel AI Gateway, Gemini, Claude and Amazon Bedrock, or any OpenAI-compatible endpoint.
- Tools run in a microVM per session, or in E2B, Vercel Sandbox, Daytona or Docker Sandboxes.
- Channels for Slack, Telegram, Discord, GitHub, Linear, Vercel Board, signed webhooks and MCP, plus schedules on a cron.
- Evals with variants, and `egin bench` for Terminal-Bench 2, FrontierHarness, GAIA and DeepSWE.
- Templates: basic, research, ops, support and web (Next.js).
- `egin-ts` on npm: the CLI with prebuilt binaries for macOS arm64, Linux x64 and Linux arm64, a client for `egin serve`, Next.js integration, a proxy, React, Vue and Svelte bindings, and Vercel Workflow support.
