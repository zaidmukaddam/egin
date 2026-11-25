---
title: Frontends
description: Put an egin agent behind a web app with the client SDK and the useAgent React hook.
group: Run
order: 4
---

A frontend is a web app people use to talk to your agent, such as a chat. The `web` template takes the least work: a Next.js chat with the agent built in. To add an agent to an app you already have, use the `egin` npm package: it contains the CLI and the libraries for talking to `egin serve`:

- `egin-ts/next`: `withEgin` for `next.config.ts`, and a route handler for `/egin/*`.
- `egin-ts/proxy`: `createProxy`, the same route handler for any server that speaks web `Request` and `Response`: SvelteKit, Nuxt, Hono, Bun, Deno, a Cloudflare Worker.
- `egin-ts/client`: sessions, messages, approvals and the event stream, anywhere `fetch` runs.
- `egin-ts/react`, `egin-ts/vue`, `egin-ts/svelte`: `useAgent` (or `createAgent`), which turns the event stream into messages you can render.

## Start from the web template

```bash
npx egin-ts@latest init my-app --template web
cd my-app && npm install && npm run dev
```

You get a Next.js app with the agent in `agent/`. `npm run dev` starts both. The chat is built from [shadcn/ui](https://ui.shadcn.com)'s chat components in `components/`, so you change it like any other part of your app. It:

- pins your message near the top and streams the reply below it, as Markdown with highlighted code
- takes attachments from the file picker, paste, or drag and drop
- shows tool calls as steps you can open, and asks before running a call that needs approval, including a subagent's
- picks the model and reasoning effort per chat, from the models the agent offers
- keeps the conversation across page reloads
- links each chat to its trace at `/traces`: every model call, tool call and subagent on one timeline
- follows the system's light or dark theme

## Add an agent to a Next.js app

Put the agent in `agent/` (or run `npx egin-ts@latest init agent`), then two files:

```ts
// next.config.ts
import { withEgin } from "egin-ts/next";

export default withEgin({});
```

```ts
// app/egin/[...path]/route.ts
export { GET, POST, DELETE } from "egin-ts/next";
```

`withEgin` makes `next dev` and `next start` run `egin serve` for the agent. The route handler forwards the browser's `/egin/*` calls to it. Your app's pages call `useAgent({ baseUrl: "/egin" })`.

To use an agent hosted elsewhere, set `EGIN_URL` and `EGIN_TOKEN` in the app's environment. `withEgin` then starts nothing, and the route handler forwards there, adding the token on your server so it never reaches the page.

For per-user sessions, write the route with `createProxy` and say who is signed in:

```ts
// app/egin/[...path]/route.ts
import { createProxy } from "egin-ts/proxy";

const handler = createProxy({
  // The server withEgin starts in development, or one hosted elsewhere.
  target: process.env.EGIN_URL ?? "http://127.0.0.1:8787",
  token: process.env.EGIN_TOKEN,
  user: async () => (await auth())?.user?.id, // your own session check
});

export { handler as GET, handler as POST, handler as DELETE };
```

Each user then reaches only their own sessions, and a request with no user is refused.

## Build a chat

```tsx
import { useAgent } from "egin-ts/react";

export function Chat() {
  const agent = useAgent({ baseUrl: "/egin" });

  return (
    <form onSubmit={(e) => {
      e.preventDefault();
      const input = e.currentTarget.elements.namedItem("m") as HTMLInputElement;
      void agent.send(input.value);
      input.value = "";
    }}>
      {agent.messages.map((m) => (
        <article key={m.id}>
          {m.parts.map((p, i) => p.type === "text" ? <p key={i}>{p.text}</p> : <code key={i}>{p.tool}</code>)}
        </article>
      ))}
      <input name="m" />
    </form>
  );
}
```

## The hook's return value

| Value | What it is |
|---|---|
| `messages` | The conversation. Each message has `parts`: text, or a tool call with its state and output |
| `status` | `"idle"`, `"busy"` while a turn runs, or `"resuming"` while a saved session loads |
| `approvals` | Tool calls waiting for an answer, including the subagents' |
| `questions` | `ask_user` calls waiting for an answer; each has `question: { text, options }` |
| `error` | The latest failure, or `null` |
| `sessionId` | The current session, once the first message is sent |
| `info` | The agent's `model`, the `models` it offers and the `reasoning_efforts` each takes, for a model picker; `null` until loaded |
| `send(text, files?, opts?)` | Send a message with optional `File`s, creating the session on first use. `opts` is `{ model, reasoningEffort }`; a pick stays with the session for later turns. Sent during a turn, the message joins it, and shows as `pending` and `queued` until the run takes it up |
| `approve(callId, allow)` | Answer an approval request |
| `answer(callId, text)` | Answer a question |
| `loadEarlier()`, `hasEarlier` | With `historyLimit`, load the turns before the earliest one shown |
| `cancel()` | Stop the running turn |
| `newSession()` | Start over on the next `send` |

A tool part's `state` is `pending`, `approval`, `question`, `running`, `done` or `denied`. The web template shows a question as a card with its options and a text field, and keeps the composer open during a turn so a message can steer it.

A subagent runs as a session of its own, so its calls are not part of the conversation, except one that asks for an approval. That call appears in the turn that handed off to the subagent, right under the `task` step, with the subagent's name in `agent` and its session in `session`. `approve(callId, allow)` answers it on that session.

## Options

| Option | Meaning |
|---|---|
| `baseUrl` | Where the egin routes are, such as `"/egin"` |
| `headers` | Extra headers, or a function that returns them before every request |
| `sessionId` | Continue this session: the history is replayed and a running turn is followed |
| `onSessionChange` | Called with the new id, or `null`, so you can save it |
| `onEvent` | Called with every raw event, including token deltas |
| `historyLimit` | Open a saved session at its last turns, about this many events, instead of replaying its whole log; `loadEarlier()` brings in the turns before |

A tool's `::data` lines arrive in the turn as `{ type: "data", tool, callId, data }` parts, for your UI to draw as a chart or a card. `send(text, files, { outputSchema })` asks for a structured answer; see [Structured output](/docs/http-api#structured-output).

Options are read once, when the component mounts. Give the component a `key` to start over with different ones.

## Attachments

Pass files to `send`:

```tsx
<input type="file" multiple onChange={(e) => void agent.send("Summarise these", Array.from(e.target.files ?? []))} />
```

Each file is uploaded to the agent's workspace under `uploads/<session>/`, where the session's sandbox can read it. The model gets the paths at the end of the message, and the user message keeps them in `files` so you can show them. A file can be up to 25 MB.

With the cloud sandboxes (`e2b`, `vercel`, `daytona`, `daytona-macos`, `docker-cloud`), the workspace is copied up when the sandbox is created, so files uploaded after a session's first tool call don't reach that sandbox.

## Keep the conversation across reloads

Save the session id and pass it back:

```tsx
const agent = useAgent({
  baseUrl: "/egin",
  sessionId: localStorage.getItem("session"),
  onSessionChange: (id) => id ? localStorage.setItem("session", id) : localStorage.removeItem("session"),
});
```

Because egin stores every step, the hook replays the session and picks up a turn that is still running, including an approval that was waiting when the page closed.

## Vue and Svelte

`egin-ts/vue` has the same `useAgent`, with each field as a computed ref:

```ts
import { useAgent } from "egin-ts/vue";

const { messages, status, send, approve } = useAgent({ baseUrl: "/egin" });
```

`egin-ts/svelte` has `createAgent`, a store with the actions on it. It works in Svelte 4 and 5:

```svelte
<script>
  import { createAgent } from "egin-ts/svelte";
  const agent = createAgent({ baseUrl: "/egin" });
</script>

{#each $agent.messages as m (m.id)}
  <p>{m.parts.map((p) => (p.type === "text" ? p.text : p.tool)).join(" ")}</p>
{/each}
<button on:click={() => agent.send("Hello")}>Send</button>
```

The store follows the stream while it has subscribers.

## One app, many users

Keep each person's chats separate by having your proxy add `X-Egin-User` (see [HTTP API](/docs/http-api#users)), and list their past chats with `createClient({ baseUrl: "/egin" }).listSessions()` from `egin-ts/client`.

## Without React

`egin-ts/client` works in Node, Bun, Deno and the browser:

```ts
import { createClient } from "egin-ts/client";

const egin = createClient({ baseUrl: "http://127.0.0.1:8787", headers: { authorization: `Bearer ${token}` } });
const { id } = await egin.createSession("Check the HTTP health of every target.");

for await (const event of egin.events(id)) {
  if (event.type === "approval_request") await egin.approve(id, String(event.call_id), true);
  // A run ends with one of these three.
  if (event.type === "done" || event.type === "error" || event.type === "cancelled") break;
}
```

`events()` reconnects on its own after a dropped connection, from the last event it saw, so nothing is missed or repeated. For other frameworks, the `Agent` class from `egin` holds the same state as the hook: `subscribe` to it and read `getSnapshot()`.

## What to read next

- [HTTP API](/docs/http-api): the routes and events these libraries use.
- [Deploy](/docs/deploy): put the app on Vercel or Cloudflare and the agent in a container.
- [Observability](/docs/observability#trace-view): the web template's trace view.
