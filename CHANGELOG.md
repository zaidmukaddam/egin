# Changelog

Every release of the `egin` binary and the `egin-ts` npm package, newest first.

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
