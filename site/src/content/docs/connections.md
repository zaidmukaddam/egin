---
title: Connections
description: Give the agent the tools of an MCP server, such as GitHub, Linear or your own, with a key or an OAuth sign-in.
group: Build
order: 5
---

A connection gives the agent the tools of an MCP server: GitHub's issues and pull requests, Linear's tickets, your own service. Each connection is one file in `connections/`, and its tools appear to the model as `<connection>__<tool>`, such as `github__search_repositories`.

## Add a known server

For a well-known server, `egin add` writes the file for you, with the right endpoint and auth header:

```bash
egin add connection/github
```

It covers GitHub, Linear, Supabase, Neon, Sentry, Stripe, Notion, Atlassian, PostHog, Zapier, Apify, Browserbase, Perplexity, Tavily, Firecrawl, Exa, Context7, Hugging Face, Cloudflare's docs and Vercel. `egin add` on its own lists them. It says which environment variable to set; most take an API token, and Vercel takes an [OAuth sign-in](#oauth-sign-in).

## Write one yourself

A server you run as a command (stdio):

```toml
# connections/github.toml
command = ["npx", "-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_TOKEN}" }
safe = ["search_repositories", "get_file_contents"]
```

A server you reach over HTTP:

```toml
# connections/docs.toml
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${DOCS_TOKEN}" }
```

| Key | What it does |
|---|---|
| `command` | Runs the server on the host and talks to it over stdio |
| `env` | Added to that command's environment |
| `url` | Talks to the server over HTTP instead |
| `headers` | Sent with every HTTP request |
| `safe` | Tools that only read, so they run without approval |
| `auth = "oauth"` | Signs in with OAuth instead of a key. See [OAuth sign-in](#oauth-sign-in) |

`${VAR}` reads the host's environment, so the file holds no secret and can be committed.

With more than 20 MCP tools in all, the model gets `tool_search` and `tool_call` instead of the full list, so a large server doesn't fill the prompt. Set `mcp_tools = "listed"` to always list them, or `"deferred"` to always defer.

## OAuth sign-in

For a server that signs users in with OAuth instead of taking a key, set `auth = "oauth"`:

```toml
# connections/vercel.toml
url = "https://mcp.vercel.com"
auth = "oauth"
```

Then sign in once:

```bash
egin connect vercel
```

egin opens the browser, waits for the redirect on `http://127.0.0.1:33418/callback`, and stores the tokens in `.egin/oauth/vercel.json`, readable only by you and never in `connections/`. It refreshes them before they expire. With no token, or a refresh that fails, the connection's tools are missing and the log says to run `egin connect` again.

egin follows the MCP authorization spec with no broker in between:

1. It asks the server without a token and reads the protected-resource metadata that the 401 points to.
2. It reads the authorization server's metadata, which must be issued by that server and support PKCE with S256.
3. It picks a client ID: `client_id` from the file when set, otherwise egin's [client metadata document](https://egin.dev/oauth/client.json) when the server accepts one, or else a dynamic registration.
4. It opens the browser and waits for the redirect.

For an authorization server that can't register clients, register an app there with the redirect URI above and set `client_id = "..."` in the connection file. `EGIN_OAUTH_CLIENT` points egin at a client metadata document of your own.

## What to read next

- [Tools](/docs/tools): write a tool when no server has what you need.
- [Middleware and hooks](/docs/middleware-and-hooks): check calls before they run, MCP calls included.
