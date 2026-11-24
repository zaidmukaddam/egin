// egin's HTTP API answered from workflow runs instead of an `egin serve` process: the same routes and
// the same events, so egin's client (`createClient`, `Agent`, `useAgent`) does not know the difference.
// Each session is a run of `eginSession` (./session.ts), stored by the workflow's world; on Vercel
// that is Vercel Workflow, and nothing else has to exist: no database, no server.
//
//   // workflows/egin.ts: your app's compiler builds the session workflow from here
//   export * from "egin-ts/workflow/session";
//
//   // app/api/egin/[...path]/route.ts
//   import { createWorkflowApi } from "egin-ts/workflow";
//   import { eginSession } from "@/workflows/egin";
//   const handler = createWorkflowApi({ session: eginSession, agent: "agent", prefix: "/api/egin" });
//   export { handler as GET, handler as POST };
//
// What it does not do yet, and `egin serve` does: uploads, a session list, deleting a session, a model
// or output schema picked per session, channels and schedules.
import path from "node:path";
import { getRun, resumeHook, start } from "workflow/api";
import { HookNotFoundError } from "workflow/errors";
import { step, type AgentRef, type Answer } from "./run.js";
import { answersTo, cancelsTo, messagesTo, V, type Cancel, type eginSession, type Entry, type Live, type State, type Stored } from "./session.js";

export { locate, step, type AgentRef, type Answer, type Located, type StepInput, type StepResult } from "./run.js";

export interface WorkflowApiOptions {
  /** The session workflow as your app built it: `eginSession`, imported from the file in your
   * workflows folder that re-exports "egin-ts/workflow/session". */
  session: typeof eginSession;
  /** The agent folder, or where it is and how to run it. A relative path is from the working directory. */
  agent: string | AgentRef;
  /** The path this handler is mounted at, such as "/api/egin". Found from the URL when left out. */
  prefix?: string;
  /**
   * The signed-in user's id, from your own session check. Each user then reaches only their own
   * sessions. Return nothing to refuse the request with 401. Leave `user` out for a single-user app.
   */
  user?: (request: Request) => string | null | undefined | Promise<string | null | undefined>;
}

const json = (value: unknown, status = 200) => Response.json(value, { status, headers: { "cache-control": "no-store" } });
const text = (message: string, status: number) => new Response(message, { status });
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** What the session is now: the last entry of its "state" stream. A session that only just started
 * has none yet, so this waits for the first. Null for no such session, one that is not `user`'s, or
 * one an older version of the workflow runs. */
async function stateOf(id: string, user: string | undefined): Promise<State | null> {
  const run = getRun(id);
  // Asked at once: whether the run exists, and for its state. No run ends the wait for a state.
  const exists = run.exists.catch(() => false);
  const reader = run.getReadable<State>({ namespace: "state", startIndex: -1 }).getReader();
  const first = await Promise.race([reader.read().catch(() => null), exists.then((yes) => (yes ? wait(15_000) : undefined)).then(() => null)]);
  void reader.cancel().catch(() => {});
  const state = first?.value;
  if (!state || state.v !== V || (user && state.owner !== user)) return null;
  return state;
}

/** The session's log as it stands: every stored event, with what a step that ran again replaced left out. */
async function stored(id: string): Promise<Stored[]> {
  const events: Stored[] = [];
  const log = getRun(id).getReadable<Entry>();
  const reader = log.getReader();
  for (let left = (await log.getTailIndex()) + 1; left > 0; left--) {
    const { value, done } = await reader.read();
    if (done) break;
    if ("back" in value) events.length = Math.min(events.length, value.back);
    else if (value.id > events.length) events.push(value);
  }
  void reader.cancel().catch(() => {});
  return events;
}

/** `GET /sessions/{id}/events?after=<id>`: the stored events after `after`, then what follows. */
function follow(id: string, after: number, now: State, signal: AbortSignal): Response {
  const run = getRun(id);
  const encoder = new TextEncoder();
  const stop = new AbortController();
  signal.addEventListener("abort", () => stop.abort());
  let last = after;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const send = (chunk: string) => {
        try {
          controller.enqueue(encoder.encode(chunk));
        } catch {}
      };
      const pump = async <T>(source: ReadableStream<T>, each: (value: T) => void) => {
        const reader = source.getReader();
        stop.signal.addEventListener("abort", () => void reader.cancel().catch(() => {}));
        try {
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            each(value);
          }
        } catch {}
      };
      // Token deltas, waiting their turn: one follows the `n` events stored before it, so it goes
      // out once those have, and not at all once the reply it was part of has.
      const held: Live[] = [];
      const release = () => {
        while (held.length && held[0].n <= last) {
          const { n, e } = held.shift()!;
          if (n === last) send(`data: ${JSON.stringify(e)}\n\n`);
        }
      };
      // The stream holds other entries beside events, so a position in it can only be at or past
      // the id it holds: starting at `after` never skips an event, and repeats are dropped by id.
      void pump(run.getReadable<Entry>({ startIndex: after }), (entry) => {
        // A step is running again: what its first attempt showed is replaced by what follows.
        if ("back" in entry) return void (last = Math.max(after, Math.min(last, entry.back)));
        if (entry.id <= last) return;
        last = entry.id;
        send(`id: ${entry.id}\ndata: ${JSON.stringify(entry.e)}\n\n`);
        release();
      });
      // Nothing from before this stream opened: that is in the log, which is being replayed.
      void pump(run.getReadable<Live>({ namespace: "live", startIndex: -1 }), (chunk) => {
        if (chunk.n < now.events) return;
        held.push(chunk);
        release();
      });
      const beat = setInterval(() => send(":\n\n"), 15_000);
      stop.signal.addEventListener("abort", () => {
        clearInterval(beat);
        try {
          controller.close();
        } catch {}
      });
    },
    cancel: () => stop.abort(),
  });
  return new Response(body, { headers: { "content-type": "text/event-stream", "cache-control": "no-store", "x-accel-buffering": "no" } });
}

/** `GET /sessions/{id}/history?before=<id>&limit=500`, as egin serve answers it: a page of the log,
 * oldest first, widened back to the `user` event that opened its first turn. */
function page(all: Stored[], before: number, limit: number) {
  const older = all.filter((s) => s.id < before);
  let from = Math.max(0, older.length - limit);
  while (from > 0 && older[from].e.type !== "user") from--;
  const events = older.slice(from).map(({ id, e }) => ({ ...e, id }));
  return { events, earlier: from > 0 ? older[from].id : null };
}

export function createWorkflowApi(options: WorkflowApiOptions): (request: Request) => Promise<Response> {
  const agent: AgentRef = typeof options.agent === "string" ? { dir: path.resolve(options.agent) } : options.agent;
  const prefix = options.prefix?.replace(/\/$/, "");
  let info: Promise<unknown> | undefined;

  return async (request) => {
    const url = new URL(request.url);
    // Without a prefix, everything before the first /sessions or /agent is the mount point.
    const at = prefix !== undefined ? (url.pathname.startsWith(prefix) ? prefix.length : 0) : Math.max(0, url.pathname.search(/\/(sessions|agent)(\/|$)/));
    const route = url.pathname.slice(at);
    const method = request.method;
    let user: string | undefined;
    if (options.user) {
      user = (await options.user(request)) ?? undefined;
      if (!user) return text("sign in first", 401);
    }
    const body = method === "POST" ? ((await request.json().catch(() => null)) as { message?: unknown; call_id?: unknown; allow?: unknown; answer?: unknown } | null) : null;
    const message = typeof body?.message === "string" ? body.message : undefined;

    if (method === "GET" && route === "/agent") {
      info ??= step(agent, { info: true }).catch((e) => {
        info = undefined;
        throw e;
      });
      return json(await info);
    }
    if (method === "POST" && route === "/sessions") {
      const run = await start(options.session, [{ agent, owner: user ?? null, ...(message ? { message } : {}) }]);
      return json({ id: run.runId });
    }
    const [, id, action] = route.match(/^\/sessions\/([\w-]+)(?:\/(\w+))?$/) ?? [];
    if (!id) return text("not found", 404);
    const asked = stateOf(id, user);

    if (method === "POST" && action === "messages") {
      if (!message) return text("`message` is required", 400);
      // The message leaves at once, not behind the look at the session: its hook's token carries
      // the owner, so one from anyone else finds no hook. Neither does one sent right behind the
      // session's creation, until the run has registered it: that one waits.
      for (let tries = 0; ; tries++) {
        try {
          await resumeHook(messagesTo(id, user ?? null), { text: message });
          break;
        } catch (e) {
          if (!HookNotFoundError.is(e)) throw e;
          if (tries >= 30 || !(await asked)) return text("no such session", 404);
          await wait(100);
        }
      }
      const status = (await asked)?.status;
      return json({ ok: true, ...(status === "running" || status === "waiting" ? { queued: true } : {}) });
    }
    const state = await asked;
    if (!state) return text("no such session", 404);

    if (method === "GET" && !action) return json({ id, status: state.status, stats: state.stats, messages: [] });
    if (method === "GET" && action === "events") return follow(id, Number(url.searchParams.get("after")) || 0, state, request.signal);
    if (method === "GET" && action === "history") {
      const limit = Math.min(Math.max(Number(url.searchParams.get("limit")) || 500, 1), 5000);
      return json(page(await stored(id), Number(url.searchParams.get("before")) || Infinity, limit));
    }
    if (method === "POST" && action === "cancel") {
      if (state.status !== "running" && state.status !== "waiting") return text("session is not running", 404);
      // Two ways in, for a turn is in one of two places: a step in flight reads the stream, and a
      // turn resting on an approval or an answer wakes on the hook.
      const ask: Cancel = { n: state.events };
      const landed: Promise<unknown>[] = [];
      const to = getRun(id).getWritable<Cancel>({ namespace: "cancel", ops: landed }).getWriter();
      await to.write(ask);
      to.releaseLock();
      await Promise.all([...landed, resumeHook(cancelsTo(id), ask)]);
      return json({ ok: true });
    }
    // `/approvals` takes `{call_id, allow}`, `/answers` takes `{call_id, answer}`: the reply to the
    // request the turn stopped at. egin drops one that answers nothing it asked.
    if (method === "POST" && (action === "approvals" || action === "answers")) {
      if (typeof body?.call_id !== "string") return text("`call_id` is required", 400);
      if (state.status !== "waiting") return text("nothing is waiting on an answer", 404);
      const reply: Answer = action === "approvals" ? { call_id: body.call_id, allow: body.allow === true } : { call_id: body.call_id, answer: typeof body.answer === "string" ? body.answer : "" };
      await resumeHook(answersTo(id), reply);
      return json({ ok: true });
    }
    return text("not found", 404);
  };
}
