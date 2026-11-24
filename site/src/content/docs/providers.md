---
title: Providers
description: Run an agent on any model, search the web with any provider, and run tools in any sandbox.
group: Build
order: 10
---

egin depends on no single vendor. Models, web search and sandboxes are each a setting in `agent.toml`, and an agent needs keys only for the providers it uses. `egin run`, `chat` and `serve` check the model's key before starting and name the variable to set when it is missing.

## The model egin picks

With no `model` in `agent.toml`, egin picks one for the key it finds, in your shell or in `.env.local` or `.env` in the folder you run it from:

| Key | Model |
|---|---|
| `YDC_API_KEY` | `kimi-k3.1-pro` on the You.com router, with You.com web search |
| `ANTHROPIC_API_KEY` | `anthropic/claude-sonnet-5-5`, searching with Claude's own web search |
| `OPENAI_API_KEY` | `openai/gpt-6-luna` |
| `AI_GATEWAY_API_KEY` | `anthropic/claude-sonnet-5.5` through the Vercel AI Gateway |
| `AWS_BEDROCK_API_KEY` | `us.anthropic.claude-sonnet-5-5` on Amazon Bedrock |

A Gemini key alone picks no model: name a `gemini/…` model in `agent.toml`. To use any other model, name it:

```toml
model = "openai/gpt-6.1-sol"
```

## Models

| `provider` | Models | Key |
|---|---|---|
| `you` (default) | The You.com Inference Router | `YDC_API_KEY` |
| `openai` | OpenAI | `OPENAI_API_KEY` |
| `gateway` | Vercel AI Gateway: hundreds of models as `provider/model` | `AI_GATEWAY_API_KEY`, or `VERCEL_OIDC_TOKEN` |
| `gemini` | Google Gemini | `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) |
| `anthropic` | Claude on the Claude API | `ANTHROPIC_API_KEY` |
| `bedrock` | Every Amazon Bedrock model, Claude included | `AWS_BEDROCK_API_KEY` (or `AWS_BEARER_TOKEN_BEDROCK` or `AMAZON_BEDROCK_API_KEY`); `AWS_REGION` or `AWS_DEFAULT_REGION`, default `us-east-1` |

You don't need to set `provider` when the model id names it:

| Model id | Provider |
|---|---|
| `anthropic/claude-sonnet-5-5` | the Claude API, or the AI Gateway when only a Gateway key is set |
| `claude-sonnet-5-5` | the Claude API |
| `openai/gpt-6-luna` | OpenAI, or the AI Gateway when only a Gateway key is set |
| `gpt-6-luna`, `o3…`, `o4…` | OpenAI |
| `gemini/…`, `google/…` | Google Gemini |
| `you/…` | the You.com router |
| `bedrock/…`, `us.anthropic.claude-…`, `anthropic.claude-…`, `openai.gpt-…` | Amazon Bedrock |
| any other `vendor/model`, such as `xai/grok-4.7` | the AI Gateway |
| anything else, such as `kimi-k3.1-pro` | the You.com router |

Set `provider` to override that. Set `base_url` and `api_key_env` to point a provider elsewhere: `openai` with a `base_url` reaches anything that speaks its API, such as OpenRouter, Groq, Together, vLLM, Ollama or LiteLLM.

```toml
model = "claude-opus-5-5"
provider = "bedrock"
```

```toml
model = "qwen3:32b"
provider = "openai"
base_url = "http://localhost:11434/v1"
api_key_env = "OLLAMA_KEY"   # any non-empty value
```

### Notes per provider

- Vercel AI Gateway:
  - Key: with no `AI_GATEWAY_API_KEY`, egin uses the Vercel OIDC token of the project it runs in. It reads the token again on every call, so a rotated token is picked up.
  - Model info: context windows, prices and image support come from the gateway's model list.
  - Web search: `web = "server"` gives the model the gateway's Perplexity search, run on Vercel's side.
- Amazon Bedrock:
  - Claude goes through Claude in Amazon Bedrock with the Messages API. Models Bedrock serves only through an inference profile, such as `us.anthropic.claude-sonnet-5-5`, are called on Bedrock's runtime endpoint.
  - GPT models (`openai.gpt-…`) go through Bedrock's Responses API, the only way they take tools.
  - Other models (Llama, Mistral, Nova, DeepSeek, gpt-oss and others) go through Bedrock's OpenAI-compatible endpoint. That works for models whose model card lists Chat Completions.
- OpenAI: GPT models go through the Responses API, the only way they take tools; reasoning is replayed between turns in its encrypted form. Other models go through Chat Completions, with `max_completion_tokens`, which OpenAI's reasoning models require.
- Claude:
  - Thinking: adaptive thinking is on for current models, and `reasoning_effort` sets its depth. It stays off for Haiku 4.5, Sonnet 4.5 and Opus 4.5, which take a token budget instead.
  - Replay: replies are sent back exactly as Claude returned them, so thinking and web results stay valid.
  - Caching: requests are cached automatically.
  - Refusals: on the Claude API, a refusal from Fable 5.1 or 5.2, Opus 5 or 5.5, or Sonnet 5.5 is retried on a fallback model in the same call.
  - Web tools: `web = "server"` uses Claude's own web search and web fetch. Claude in Amazon Bedrock has no server tools; use `web = "client"` there.

For an endpoint that doesn't publish model details, set `context_limit`, and `vision = true` if the model takes images.

## Web search

With `web = "client"`, the agent's `web_search` and `web_fetch` tools go to the provider in `search`.

| `search` | Key | `web_fetch` |
|---|---|---|
| `you` (default) | `YDC_API_KEY` | You.com Contents |
| `parallel` | `PARALLEL_API_KEY` | Parallel Extract |
| `firecrawl` | `FIRECRAWL_API_KEY` | Firecrawl Scrape |
| `context.dev` | `CONTEXT_DEV_API_KEY` | context.dev Scrape |
| `brave` | `BRAVE_API_KEY` | Direct fetch |
| `searxng` | `SEARXNG_URL`, your own instance | Direct fetch |

A direct fetch downloads the page and turns HTML into text, and PDFs into text page by page.

`exclude_domains` and `exclude_terms` apply to every provider. egin filters what comes back itself, so a result a provider lets through still never reaches the model.

For anything else, set `web = "off"` and write your own `tools/web_search.sh`; see [Tools and skills](/docs/tools).

## Sandboxes

| `sandbox` | Where tools run | Needs |
|---|---|---|
| `auto` (default) | `microsandbox` where the host supports it, else `local` | |
| `microsandbox` | A local microVM per session, the workspace mounted | KVM (Linux) or Apple Silicon |
| `e2b` | An E2B sandbox per session | `E2B_API_KEY`; `E2B_TEMPLATE` (default `base`) |
| `vercel` | A Vercel Sandbox per session | `VERCEL_TOKEN` with `VERCEL_TEAM_ID` and `VERCEL_PROJECT_ID`, or `VERCEL_OIDC_TOKEN`; `VERCEL_SANDBOX_IMAGE` and `VERCEL_SANDBOX_REGION` optional |
| `daytona` | A Daytona sandbox per session | `DAYTONA_API_KEY`; `DAYTONA_API_URL` optional |
| `daytona-macos` | A Daytona macOS VM per session (Xcode, iPhone simulator); a first call can start a billed 24-hour Mac reservation | `DAYTONA_API_KEY`; `DAYTONA_MACOS_REGION` (default `us-east-bos-1`) and `DAYTONA_MACOS_API_URL` optional |
| `docker`, `docker-cloud` | Docker Sandboxes | the signed-in `sbx` CLI |
| `local` | The host itself | |

Cloud sandboxes copy the workspace up once and don't sync it back. egin finds them again by name after it restarts, and deletes them with the session. E2B and Vercel can't limit network access to a list of hosts, so they take `network = ["*"]` or `[]` (Vercel only `["*"]`). `secrets` need `microsandbox`. See [Sandboxing](/docs/sandboxing) for the rest.
