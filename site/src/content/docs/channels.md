---
title: Channels and schedules
description: Reach the agent from Slack, Telegram, Discord, GitHub, Vercel Board, a signed webhook or MCP, and run prompts on a cron.
group: Run
order: 5
---

A channel is a place people reach the agent: a Slack thread, a Telegram or Discord chat, a comment on a GitHub or Linear issue, a Vercel Board thread, a signed webhook, or an MCP client. `egin add` lists them all. A schedule is a prompt that runs on its own, on a cron. Each channel is one file in `channels/` and each schedule one file in `schedules/`, and both run while `egin serve` is running.

```bash
egin add channel/slack    # writes channels/slack.toml and says what to set
egin serve my-agent
```

Every conversation on a channel, and every scheduled run, is an ordinary session: saved, resumable, and visible over the [HTTP API](/docs/http-api).

## Webhook

```toml
# channels/webhook.toml
secret = "${OPS_WEBHOOK_SECRET}"
```

Send a POST to `/channels/webhook` with `{"text", "thread"?}`. Sign the body with HMAC-SHA256 using the secret and send the signature as `X-Egin-Signature: sha256=<hex>`. Unsigned or wrongly signed requests are refused.

Each `thread` gets its own session, so a conversation keeps its context across messages. A request with no `thread` starts a new session every time.

The request waits for the agent and answers `{"session", "text"}`. For long runs, add `reply_url = "https://…"` to the file: the request then returns `{"accepted": true}` at once, and the answer is POSTed to that URL as `{"thread", "session", "text"}`, or `{"thread", "error"}` if the run failed.

Without `reply_url`, the answer goes back on the open HTTP request. If a restart closes that request, the answer is stored in the session but not sent. Use `reply_url` when runs are long enough for that to matter.

## Slack

Create the Slack app from the manifest in the `support` template, then:

```toml
# channels/slack.toml, Socket Mode: no public URL needed
app_token = "${SLACK_APP_TOKEN}"
bot_token = "${SLACK_BOT_TOKEN}"
```

Or use the HTTP Events API: turn Socket Mode off, point the request URL at `https://<your server>/channels/slack`, and set:

```toml
signing_secret = "${SLACK_SIGNING_SECRET}"
bot_token = "${SLACK_BOT_TOKEN}"
```

Signatures and replays are checked. The agent answers mentions and direct messages in the thread, with one session per thread.

## Telegram

`egin add channel/telegram` writes this file:

```toml
# channels/telegram.toml
bot_token = "${TELEGRAM_BOT_TOKEN}"
secret_token = "${TELEGRAM_SECRET}"
username = "my_egin_bot"   # optional
```

Create the bot with @BotFather, pick a secret, and register the webhook once:

```bash
curl "https://api.telegram.org/bot$TELEGRAM_BOT_TOKEN/setWebhook" \
  -d url=https://<your server>/channels/telegram -d secret_token=$TELEGRAM_SECRET
```

Telegram sends the secret back as `X-Telegram-Bot-Api-Secret-Token`; requests without it are refused. The agent answers every message in a private chat. In a group it answers replies to its own messages, and `@username` mentions when `username` is set, as a reply to that message.

Telegram only sends a group's mentions to a bot with privacy mode off (@BotFather, `/setprivacy`) or one that is an admin there.

Each chat is one session, and each topic in a forum group is one too. Answers over 4,096 characters are sent as several messages.

## Discord

`egin add channel/discord` writes this file:

```toml
# channels/discord.toml
public_key = "${DISCORD_PUBLIC_KEY}"
bot_token = "${DISCORD_BOT_TOKEN}"   # optional
```

The agent answers a slash command. Register one called `/ask` with a string option once:

```bash
curl -X POST https://discord.com/api/v10/applications/<application id>/commands \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"ask","description":"Ask the agent","options":[{"type":3,"name":"prompt","description":"Your question","required":true}]}'
```

Then set the app's Interactions Endpoint URL to `https://<your server>/channels/discord`. Every request is checked against `public_key` (Ed25519), as Discord requires before it accepts the URL.

Discord waits only three seconds for a response, so egin defers it (the user sees the bot thinking) and edits the answer in when the run ends. Answers over 2,000 characters continue in follow-up messages.

Each channel is one session. Discord allows edits to the interaction for 15 minutes. A run cut off by a restart finishes later, so `bot_token` lets it post the answer to the channel instead.

## GitHub

`egin add channel/github` writes this file:

```toml
# channels/github.toml
secret = "${GITHUB_WEBHOOK_SECRET}"
token = "${GITHUB_TOKEN}"
username = "${GITHUB_BOT_LOGIN}"
```

Add a webhook to the repository (Settings, then Webhooks) with the payload URL `https://<your server>/channels/github`, content type `application/json`, the secret, and the events "Issue comments" and "Pull request review comments". `X-Hub-Signature-256` is checked on every delivery.

The agent answers comments that mention `@<username>`, the account `token` belongs to. A comment on an issue or pull request gets a comment back; a review comment gets a reply in its thread. Each issue or pull request is one session. Comments from bots, and from `username` itself, are ignored so two bots can't answer each other forever.

## Vercel Board

`egin add channel/board` writes this file:

```toml
# channels/board.toml
channel = "dev"        # a channel, or a subtree such as "dev/"
profile = "egin"       # the profile the agent writes as
```

[Vercel Board](https://vercel.com/docs/board) is a message board where people and agents post in channels and threads. While `egin serve` runs, egin follows `channel` with `vercel board tail` and answers each new message in its thread, one session per thread. Board's own watchers wake only a Vercel deployment; following the tail works from any machine, with no public URL.

It needs the [Vercel CLI](https://vercel.com/docs/cli), signed in with `vercel login`, or `BOARD_TOKEN` set where there is no login. `BOARD_NAME` or `BOARD_URL` in the environment picks a board other than the team's General board.

The agent writes as the `profile` profile, which the board shows as `@<your handle>/egin`, and never answers its own messages. To answer only some messages, pass more `tail` filters:

```toml
args = ["--mentions", "@ada/egin"]   # only messages that mention the agent
```

If the tail drops, egin restarts it from the last event it printed, so no message is missed or answered twice. Answers over 10,000 characters are posted as several replies. For tools that let the agent post, read and work board tasks itself, see [Tools](/docs/tools#ready-made-tools).

## Described channels

Any platform that sends signed JSON webhooks can reach the agent without code in egin. A file describes how to check the signature, where the message is, and how to reply. `egin add channel/linear` writes one for Linear:

```toml
# channels/linear.toml, served at /channels/linear
[verify]
header = "Linear-Signature"            # hex HMAC-SHA256 of the raw body
secret = "${LINEAR_WEBHOOK_SECRET}"

[message]
text = "/data/body"                    # JSON pointers into the event
thread = "/data/issueId"               # one session per issue
id_header = "Linear-Delivery"          # a redelivered webhook is skipped
only = { "/type" = "Comment", "/action" = "create" }
skip = { "/actor/id" = "${LINEAR_AGENT_USER_ID}" }   # the agent's own comments

[reply]
url = "https://api.linear.app/graphql"
headers = { Authorization = "${LINEAR_API_KEY}" }
body = '''{"query": "mutation($issueId: String!, $body: String!) { commentCreate(input: { issueId: $issueId, body: $body }) { success } }", "variables": {"issueId": {{thread}}, "body": {{answer}}}}'''
```

| Key | What it does |
|---|---|
| `verify.header`, `verify.secret` | The signature header, and the secret it is checked with |
| `verify.scheme` | `hmac-sha256` (default), `hmac-sha1`, or `token` (the header contains the secret itself) |
| `verify.encoding`, `verify.prefix` | `hex` (default) or `base64`; a prefix to strip first, such as `sha256=` |
| `message.text`, `message.thread` | JSON pointers to the text and to what makes one conversation (without `thread`, one session per request) |
| `message.id`, `message.id_header` | A delivery id, from the body or a header: a repeat is acknowledged and skipped |
| `message.only`, `message.skip` | Answer only events where every pointer has its value; ignore any where one does |
| `message.mention` | Answer only text that contains it, which is then removed |
| `reply.url`, `reply.method`, `reply.headers`, `reply.body` | The request that sends the answer. `{{answer}}` and `{{thread}}` become JSON strings in the body and are URL-encoded in the URL |

Without `[reply]`, the request waits and gets `{"session", "thread", "text"}` back. A mistake in the file stops the agent from loading, and the error gives the reason. The signature is checked over the raw body, so a platform that signs a timestamp or form fields needs a built-in channel instead.

For a platform not in the catalog, `egin add channel <its webhook docs URL>` prints a guide for a coding agent: what to find in the provider's docs, the file to write, and how to test it with a signed request.

## MCP server

With `channels/mcp.toml`, `egin serve` is also an MCP server at `/mcp` (Streamable HTTP), so another agent, an IDE or Claude can send the agent work. `egin add channel/mcp` writes the file:

```toml
# channels/mcp.toml
description = "Checks a release: changelog, tests, open issues"   # optional: shown to the calling model
```

It offers two tools:

| Tool | What it does |
|---|---|
| `send_message` | `{message, session?, wait?}`: runs a turn and returns the answer, with the session id in `structuredContent`. Pass `session` to continue a conversation. With `wait: false` it returns at once, for runs longer than the caller will wait |
| `get_session` | `{session}`: the status and latest answer |

The endpoint is protected by `EGIN_TOKEN` like the session routes, so a client sends `Authorization: Bearer <token>`. Another egin agent connects to it like any MCP server:

```toml
# connections/release-checker.toml
url = "https://checker.example.com/mcp"
headers = { Authorization = "Bearer ${CHECKER_TOKEN}" }
```

A message to a session that is still running joins its run, as on the HTTP API. With `X-Egin-User`, a caller sees only its own sessions. Approvals and questions in these runs are answered over the HTTP API.

## Approvals and restarts

With `approval = "ask"`, a channel or scheduled session waits for its approvals to be answered over the HTTP API before it replies. Set `approval = "never"` for an agent that only talks through channels, or mark its tools safe.

A turn cut off by a restart is resumed and still answers where it was asked: in the Slack thread, the Telegram chat, the Discord channel (with `bot_token`), the GitHub issue, a described channel's `[reply]`, or at the webhook's `reply_url`.

## Schedules

Each file in `schedules/` runs a prompt on a timer.

```toml
# schedules/health.toml
cron = "*/15 * * * *"
prompt = "Run the HTTP health check runbook."
```

`cron` runs in the server's local time. Use `every_secs = 900` instead for a plain interval.

## What to read next

- [Deploy](/docs/deploy): keep `egin serve` running somewhere channels can reach it.
- [Middleware and hooks](/docs/middleware-and-hooks): post to a channel when an approval is waiting.
- [HTTP API](/docs/http-api#approvals): answer approvals from your own tools.
