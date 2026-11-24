---
title: Deploy
description: Run egin in a container, on several instances or in serverless functions, behind a web app on Vercel or a Cloudflare Worker.
group: Run
order: 7
---

An agent's sessions run for minutes, and where they keep their state decides how the agent deploys:

- One process, state on its disk: the default. `egin serve` runs as a container on any host that keeps a process up and mounts a volume: Fly.io, Railway, Render, a VM.
- Any number of processes, state in a shared store: with `EGIN_STORE_URL`, sessions live in a database every egin process reaches, so the agent runs on several instances, or in serverless functions where no process stays up. See [Several instances, and serverless](#several-instances-and-serverless).
- No process at all, state in Vercel Workflow: on Vercel a session can be a Workflow run, which Vercel stores. There is no server to keep up and no database to create. See [Vercel, with nothing else to run](#vercel-with-nothing-else-to-run).

With the first two, your app runs on Vercel or Cloudflare and forwards the browser's calls to the agent, adding its token on the server.

## The agent, in a container

The repository's `Dockerfile` builds `egin` and serves the folder mounted at `/agent`:

```bash
docker build -t egin .
docker run -p 8787:8787 -v "$PWD/my-agent:/agent" \
  -e EGIN_TOKEN=... -e YDC_API_KEY=... egin
```

- `EGIN_TOKEN` is required. The image listens on `0.0.0.0`, so without a token egin exits at start. With it, every route except `/health` and the channels answers `401` to a request without the token.
- Put `/agent` on a persistent volume. Sessions, uploads and memory live in that folder, and a session interrupted by a redeploy resumes from it.
- A container has no KVM, so it can't run microVMs. Set `sandbox` in `agent.toml` to a cloud sandbox (`e2b`, `vercel`, `daytona`), or to `local` to run tools inside the container itself. Left at `auto`, it falls back to `local` with a warning. `secrets` need a microVM, so an agent that uses them won't start in a container.
- Point the host's health check at `GET /health`. The server listens on `$PORT` when the host sets one.

Check the container before you connect an app:

```bash
curl -H "Authorization: Bearer $EGIN_TOKEN" https://agent.example.com/agent
```

## Several instances, and serverless

When a process keeps sessions on its own disk, a session belongs to the process that started it. Behind a load balancer, or in serverless functions, the next request reaches another process, which has no record of the session. A function instance is also frozen between requests and recycled without warning, and a run in flight goes with it.

Point every egin process at one [libSQL](https://docs.turso.tech/libsql) database, on [Turso](https://turso.tech) or your own libSQL server, and a session belongs to the store instead:

| Variable | Value |
|---|---|
| `EGIN_STORE_URL` | The database, such as `libsql://my-agent-acme.turso.io`, or the `https://` address of a libSQL server |
| `EGIN_STORE_TOKEN` | Its auth token |

egin creates its tables on first start. With the store set:

- Any instance can show any session, take messages for it and cancel it.
- Only one instance runs a session at a time. Starting a run and taking one over are each one statement in the database, so two instances that receive the same message can't both start.
- The next instance that gets a request about a session continues a run whose instance died, once that instance has gone 30 seconds without renewing its claim. An open event stream counts as a request, so a chat page picks its own run back up.
- An event stream works from any instance. The one running the session sends token deltas as they arrive. Another one reads the store every second and sends whole messages.

Runs nobody is watching, started by a channel or a schedule, get no request to continue them. Call `POST /resume` from a cron job every minute: it continues every run whose instance is gone.

```bash
curl -X POST -H "Authorization: Bearer $EGIN_TOKEN" https://agent.example.com/resume
```

The store holds sessions: messages, events, approvals, queued messages. Files stay on each instance's disk: uploads, memory, and whatever a `local` sandbox writes. Use a cloud sandbox (`e2b`, `vercel`, `daytona`), which follows the session to whichever instance runs it.

Without `EGIN_STORE_URL`, run one process: several of them on separate disks each see only their own sessions.

## Vercel, with nothing else to run

A session can run as one run of a [Workflow SDK](https://workflow-sdk.dev) workflow. On Vercel, Vercel Workflow stores the run, so the agent deploys with your app and there is nothing else to run: no `egin serve`, no container, no database. The Ask AI drawer on this site runs this way.

A turn is one step of the run: one `egin step`, which makes the turn's model calls and runs its tools. A turn longer than a minute continues in another step. Each event is saved and sent as it happens, so the event stream is the one `egin serve` sends. Between turns, and while a tool call waits for an approval or an answer, the run is paused and uses no compute.

The run's state is the conversation, saved after every step, and any function instance can run the next step. A step whose instance died runs again from where it began, so it repeats any tool call it had already made.

Add the Workflow SDK to your app as its [getting started guide](https://workflow-sdk.dev/docs/getting-started) shows for your framework, then:

```bash
npm i egin-ts workflow
```

```ts
// workflows/egin.ts: your app's compiler builds the session workflow from here
export * from "egin-ts/workflow/session";
```

```ts
// app/api/egin/[...path]/route.ts
import { createWorkflowApi } from "egin-ts/workflow";
import { eginSession } from "@/workflows/egin";

const handler = createWorkflowApi({
  session: eginSession,
  agent: "agent",
  prefix: "/api/egin",
  user: async () => (await auth())?.user?.id,
});
export { handler as GET, handler as POST };
```

The handler answers the same routes as `egin serve` with the same events, so `useAgent` and `createClient` work against it unchanged. `user` works as it does for [`createProxy`](/docs/frontends#one-app-many-users): each user reaches only their own sessions.

The function that runs the steps needs more than its code: the agent folder and an egin binary for its architecture. Some frameworks run steps inside the app's own functions. Others build a separate one that holds a single bundled file, so the build has to copy the folder and the binary in. `agent` can say where each is:

```ts
agent: {
  dir: path.join(process.cwd(), "agent"),
  // {arch} becomes x64 or arm64, whichever the function running the step is
  bin: path.join(process.cwd(), "bin/egin-linux-{arch}"),
}
```

A step may run on a different instance from the one before it, and each instance has its own disk. An agent that only reads, or one with a cloud sandbox (`e2b`, `vercel`, `daytona`), is not affected. With a `local` sandbox, files a step wrote can be gone by the next one.

These parts of `egin serve` don't work this way yet: uploads, the session list, deleting a session, a model or output schema chosen per session, [channels and schedules](/docs/channels).

In development the same code runs on the Workflow SDK's local world, which keeps runs in a folder, so `npm run dev` is enough.

## Vercel, with the agent in a container

A Next.js app made with `--template web`, or set up as in [Frontends](/docs/frontends), deploys as is. Set two environment variables in the Vercel project:

| Variable | Value |
|---|---|
| `EGIN_URL` | The container's URL, such as `https://agent.example.com` |
| `EGIN_TOKEN` | The token the container was started with |

With `EGIN_URL` set, `withEgin` starts no local server, and the `/egin/*` route handler forwards each call to the container with the token. The browser only ever talks to your app. Vercel doesn't use the app's `agent/` folder: deploy the same folder to the container.

## Cloudflare Workers

`createProxy` takes a web `Request` and returns a `Response`, so it runs in a Worker as it is:

```ts
// src/index.ts
import { createProxy } from "egin-ts/proxy";

export default {
  fetch(request: Request, env: { EGIN_URL: string; EGIN_TOKEN: string }) {
    if (!new URL(request.url).pathname.startsWith("/egin/")) return new Response("Not found", { status: 404 });
    return createProxy({ target: env.EGIN_URL, token: env.EGIN_TOKEN, prefix: "/egin" })(request);
  },
};
```

```jsonc
// wrangler.jsonc
{
  "name": "my-agent-proxy",
  "main": "src/index.ts",
  "compatibility_date": "2026-11-01",
  "vars": { "EGIN_URL": "https://agent.example.com" }
}
```

```bash
npx wrangler secret put EGIN_TOKEN
npx wrangler deploy
```

Event streams pass through the Worker unbuffered. Serve your frontend from the same Worker, or from Pages on the same domain, and call `useAgent({ baseUrl: "/egin" })`.

## Many users

When people sign in to your app, pass `user` to `createProxy` so each one reaches only their own sessions. See [One app, many users](/docs/frontends#one-app-many-users).

## What to read next

- [Sandboxing](/docs/sandboxing#backends): pick a cloud sandbox for a container.
- [Durability](/docs/durability): what happens to running sessions during a redeploy.
- [Observability](/docs/observability): see what the deployed agent is doing.
- [Security](/docs/security): what to check before production.
