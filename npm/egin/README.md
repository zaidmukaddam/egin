# egin

Write an agent as a folder of files and run it with one binary. This package contains the `egin` CLI, a client for `egin serve`, Next.js integration (`egin-ts/next`), a proxy for any web server (`egin-ts/proxy`), and React, Vue and Svelte bindings.

```bash
npx egin-ts@latest init my-agent
```

## Put an agent in a web app

```bash
npx egin-ts@latest init my-agent --template web
cd my-agent/web && npm install && npm run dev
```

```tsx
import { useAgent } from "egin-ts/react";

const agent = useAgent({ baseUrl: "/egin" });
agent.send("Summarise this", files);
agent.approve(callId, true);
```

`useAgent` returns the conversation as `messages`, plus `status`, `approvals`, `error`, `send`, `approve`, `cancel` and `newSession`. A saved `sessionId` replays the conversation and follows a turn that is still running.

## From any JavaScript runtime

```ts
import { createClient } from "egin-ts/client";

const egin = createClient({ baseUrl: "http://127.0.0.1:8787", headers: { authorization: `Bearer ${token}` } });
const { id } = await egin.createSession("Check every target");
for await (const event of egin.events(id)) {
  // A run ends with one of these three.
  if (event.type === "done" || event.type === "error" || event.type === "cancelled") break;
}
```

Read the [egin docs](https://egin.dev/docs).
