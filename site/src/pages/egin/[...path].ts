// Ask AI's backend: the docs agent in agent/, which ships with the site as one deployment. The drawer
// talks to it with egin's `Agent` (what `useAgent` wraps), over egin's HTTP API under /egin.
//
// It's public, so each visitor is an anonymous user (a cookie): they see only their own sessions.
// Only the routes the drawer uses get through, and a message is capped.
//
// A function instance is frozen between requests and recycled without notice, and the next request may
// reach another one, so a session can't live in an instance. Three ways to run the agent, by
// EGIN_RUNTIME, or picked from the environment when it isn't set:
// - "workflow", the default on Vercel: each session is a Vercel Workflow run, which stores it. There
//   is no database and no process to keep alive (`egin/workflow`, built from src/workflows/egin.ts).
// - "serve", the default elsewhere: `egin serve` starts in the instance on the first request and stays
//   warm. With a Turso database connected to the project (TURSO_DATABASE_URL and TURSO_AUTH_TOKEN), or
//   EGIN_STORE_URL and EGIN_STORE_TOKEN, every instance's egin shares its sessions there. Without one
//   a session lives in one instance's /tmp, and another instance answers 404.
// - EGIN_URL set: proxy to that egin server.
import type { APIRoute } from "astro";
import { createProxy } from "egin/proxy";
import { createWorkflowApi, locate } from "egin/workflow";
import { spawn } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { docsAgent } from "../../lib/egin-agent";
import { eginSession } from "../../workflows/egin";

export const prerender = false;

const runtime = process.env.EGIN_RUNTIME ?? (process.env.VERCEL && !process.env.EGIN_URL ? "workflow" : "serve");

// The visitor behind each request this route hands on, for the handlers' `user`.
const visitors = new WeakMap<Request, string>();

let workflow: ((request: Request) => Promise<Response>) | undefined;
const viaWorkflow = () => (workflow ??= createWorkflowApi({ session: eginSession, agent: docsAgent(), prefix: "/egin", user: (r) => visitors.get(r) }));

type Target = { url: string; token: string };

async function start(): Promise<Target> {
  const { bin, dir, env } = locate(docsAgent());
  const port = 20000 + Math.floor(Math.random() * 20000);
  const token = randomBytes(24).toString("hex");
  // The session store: the project's Turso database, unless one is named for egin outright. This
  // database is the docs agent's alone; egin makes its own tables in it.
  const store = env.EGIN_STORE_URL ? {} : { EGIN_STORE_URL: env.TURSO_DATABASE_URL ?? "", EGIN_STORE_TOKEN: env.TURSO_AUTH_TOKEN ?? "" };
  const child = spawn(bin, ["serve", dir, "--port", String(port)], { env: { ...env, ...store, EGIN_TOKEN: token }, stdio: ["ignore", "ignore", "inherit"] });
  child.on("exit", () => (local = null));
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) break;
    if (await fetch(`${url}/health`).then((r) => r.ok, () => false)) return { url, token };
    await new Promise((r) => setTimeout(r, 100));
  }
  child.kill();
  throw new Error("egin serve didn't start");
}

let local: Promise<Target> | null = null;
const target = (): Promise<Target> =>
  process.env.EGIN_URL
    ? Promise.resolve({ url: process.env.EGIN_URL, token: process.env.EGIN_TOKEN ?? "" })
    : (local ||= start().catch((e) => { local = null; throw e; }));

// What the drawer does, and nothing else: no session list, uploads or deletes. Approvals and answers
// are here for an agent that asks; the docs agent never does.
const allowed = (method: string, p: string) =>
  (method === "GET" && (p === "/agent" || /^\/sessions\/[\w-]+(\/(events|history))?$/.test(p))) ||
  (method === "POST" && (p === "/sessions" || /^\/sessions\/[\w-]+\/(messages|cancel|approvals|answers)$/.test(p)));

const MAX_MESSAGE = 2000;

// An event stream stays open for as long as the drawer does, and a function that reaches its time limit
// is killed along with whatever it holds. Production cuts a stream at 120 s, whatever maxDuration in
// astro.config.mjs asks for (measured 23 Nov 2026: "Task timed out after 120 seconds", and the instance
// was gone afterwards). So the stream ends itself at 100 s, and the drawer reopens it from the last
// event it saw.
const STREAM_MS = 100_000;

function endsAfter(res: Response, ms: number): Response {
  if (!res.body) return res;
  let timer: ReturnType<typeof setTimeout>;
  const cut = new TransformStream<Uint8Array, Uint8Array>({
    start: (c) => void (timer = setTimeout(() => c.terminate(), ms)),
    flush: () => clearTimeout(timer),
  });
  return new Response(res.body.pipeThrough(cut), { status: res.status, headers: res.headers });
}

export const ALL: APIRoute = async ({ request, cookies }) => {
  const url = new URL(request.url);
  const p = url.pathname.replace(/^\/egin/, "");
  if (!allowed(request.method, p)) return new Response("not found", { status: 404 });

  // Only the fields a route takes are read from a body, so a visitor can't pick another model or an
  // output schema.
  let message = "";
  let answer: { call_id: string; allow?: boolean; answer?: string } | undefined;
  if (request.method === "POST" && !p.endsWith("/cancel")) {
    const body = (await request.json().catch(() => null)) as { message?: unknown; call_id?: unknown; allow?: unknown; answer?: unknown } | null;
    // A session can be created with its first message, or with none.
    if (p === "/sessions" || p.endsWith("/messages")) {
      message = typeof body?.message === "string" ? body.message.trim() : "";
      if ((!message && p !== "/sessions") || message.length > MAX_MESSAGE + 200) return new Response("Ask a shorter question.", { status: 400 });
    } else {
      if (typeof body?.call_id !== "string" || body.call_id.length > 200) return new Response("`call_id` is required", { status: 400 });
      const text = typeof body.answer === "string" ? body.answer.slice(0, MAX_MESSAGE) : "";
      answer = p.endsWith("/approvals") ? { call_id: body.call_id, allow: body.allow === true } : { call_id: body.call_id, answer: text };
    }
  }
  const sent = message ? { message } : answer;

  let visitor = cookies.get("egin_visitor")?.value;
  if (!visitor || !/^[\w-]{36}$/.test(visitor)) {
    visitor = randomUUID();
    cookies.set("egin_visitor", visitor, { path: "/egin", httpOnly: true, sameSite: "strict", secure: import.meta.env.PROD, maxAge: 60 * 60 * 24 * 30 });
  }

  // A POST is rebuilt with only what was read above.
  const init = request.method === "POST" ? { method: "POST", signal: request.signal, ...(sent ? { headers: { "content-type": "application/json" }, body: JSON.stringify(sent) } : {}) } : undefined;
  const handed = init ? new Request(request.url, init) : request;
  visitors.set(handed, visitor);
  let res: Response;
  try {
    if (runtime === "workflow") {
      res = await viaWorkflow()(handed);
    } else {
      const t = await target();
      res = await createProxy({ target: t.url, token: t.token, prefix: "/egin", user: (r) => visitors.get(r) })(handed);
    }
  } catch (e) {
    console.error("Ask AI: the docs agent failed:", e);
    return new Response("The docs agent isn't running.", { status: 503 });
  }
  return p.endsWith("/events") ? endsAfter(res, STREAM_MS) : res;
};
