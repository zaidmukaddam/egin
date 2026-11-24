// A client for `egin serve`: sessions, messages, approvals, and the event stream.
// Runs in browsers, Node 18+, Bun and Deno: it only needs `fetch`.

export type Headers = Record<string, string>;

export interface ClientOptions {
  /** Where the egin routes live: a same-origin prefix such as "/egin", or another origin. */
  baseUrl?: string;
  /** Extra headers, or a function that returns them before every request (for a fresh token). */
  headers?: Headers | (() => Headers | Promise<Headers>);
  fetch?: typeof fetch;
}

/** One stored event. `id` is missing on token deltas, which are not stored. */
export interface EginEvent {
  id?: number;
  type: string;
  [key: string]: unknown;
}

/** An uploaded file, as the agent sees it. */
export interface Attachment {
  name: string;
  path: string;
  size?: number;
}

export interface SessionSummary {
  id: string;
  status: string;
  /** Unix seconds. */
  created: number;
  /** The first user message, or null before one is sent. */
  title: string | null;
}

export interface SessionInfo {
  id: string;
  status: "running" | "done" | "error" | "cancelled" | string;
  stats: Record<string, number>;
  messages: unknown[];
}

/** What the agent lets a client pick, from `GET /agent`. */
export interface AgentInfo {
  /** The agent's own model, used unless a session picks another. */
  model: string;
  /** Every model a session may pick, the agent's own first. */
  models: string[];
  reasoning_effort: string | null;
  /** The reasoning efforts each model accepts, by model id. */
  reasoning_efforts: Record<string, string[]>;
}

/** Choices for this turn and the ones after it. The server refuses a model the agent does not offer. */
export interface TurnOptions {
  model?: string;
  reasoningEffort?: string;
  /** A JSON Schema: the run ends with a value that fits it, as `output` on its `done` event. Stays
   * with the session for later turns; `null` clears it. */
  outputSchema?: Record<string, unknown> | null;
}

function choice(opts?: TurnOptions) {
  return {
    ...(opts?.model ? { model: opts.model } : {}),
    ...(opts?.reasoningEffort ? { reasoning_effort: opts.reasoningEffort } : {}),
    ...(opts?.outputSchema !== undefined ? { output_schema: opts.outputSchema } : {}),
  };
}

export class EginError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

export type Client = ReturnType<typeof createClient>;

export function createClient(options: ClientOptions = {}) {
  const base = (options.baseUrl ?? "").replace(/\/$/, "");
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);

  async function headers(): Promise<Headers> {
    const h = options.headers;
    return typeof h === "function" ? await h() : (h ?? {});
  }

  async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await doFetch(base + path, {
      method,
      headers: { ...(body === undefined ? {} : { "content-type": "application/json" }), ...(await headers()) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) throw new EginError(res.status, (await res.text()) || res.statusText);
    return (await res.json()) as T;
  }

  return {
    /** The models and reasoning efforts this agent lets a client pick. */
    agentInfo: () => call<AgentInfo>("GET", "/agent"),
    /** Create a session, and start a run when `message` is given. */
    createSession: (message?: string, opts?: TurnOptions) =>
      call<{ id: string }>("POST", "/sessions", { ...(message === undefined ? {} : { message }), ...choice(opts) }),
    /** Send a message. `files` are attachments returned by `upload` for this session. Sent while the
     * session runs, it joins that run (`queued: true`) as the agent's `mid_run` says. */
    send: (id: string, message: string, files?: Attachment[], opts?: TurnOptions) =>
      call<{ ok: true; queued?: boolean }>("POST", `/sessions/${id}/messages`, { message, ...(files?.length ? { files } : {}), ...choice(opts) }),
    /** Put a file in the session's workspace, under uploads/<session>/. The server caps a file at 25 MB. */
    async upload(id: string, file: Blob, name = (file as File).name ?? "file"): Promise<Attachment> {
      const res = await doFetch(`${base}/sessions/${id}/files?name=${encodeURIComponent(name)}`, {
        method: "POST",
        headers: { "content-type": "application/octet-stream", ...(await headers()) },
        body: file,
      });
      if (!res.ok) throw new EginError(res.status, (await res.text()) || res.statusText);
      return (await res.json()) as Attachment;
    },
    getSession: (id: string) => call<SessionInfo>("GET", `/sessions/${id}`),
    /** A page of the stored log, oldest first, starting at a turn: the latest one, or the one before
     * event `before`. `earlier` is the `before` of the page ahead of it, or null at the start. */
    history: (id: string, opts: { before?: number; limit?: number } = {}) => {
      const q = new URLSearchParams();
      if (opts.before !== undefined) q.set("before", String(opts.before));
      if (opts.limit !== undefined) q.set("limit", String(opts.limit));
      return call<{ events: EginEvent[]; earlier: number | null }>("GET", `/sessions/${id}/history?${q}`);
    },
    /** Sessions newest first, titled by their first message. Behind a proxy that sets `X-Egin-User`, only that user's. */
    listSessions: (limit = 50) => call<{ sessions: SessionSummary[] }>("GET", `/sessions?limit=${limit}`).then((r) => r.sessions),
    approve: (id: string, callId: string, allow: boolean) =>
      call<{ ok: true }>("POST", `/sessions/${id}/approvals`, { call_id: callId, allow }),
    /** Answer an `ask_user` question (a `question` event). */
    answer: (id: string, callId: string, answer: string) =>
      call<{ ok: true }>("POST", `/sessions/${id}/answers`, { call_id: callId, answer }),
    cancel: (id: string) => call<{ ok: true }>("POST", `/sessions/${id}/cancel`),
    deleteSession: (id: string) => call<{ ok: true }>("DELETE", `/sessions/${id}`),

    /**
     * The session's events: the stored log after `after`, then live events, for as long as you read.
     * A dropped connection is reopened from the last event seen, so nothing is missed or repeated.
     */
    async *events(id: string, opts: { after?: number; signal?: AbortSignal } = {}): AsyncGenerator<EginEvent> {
      let last = opts.after ?? 0;
      let delay = 500;
      while (!opts.signal?.aborted) {
        try {
          const res = await doFetch(`${base}/sessions/${id}/events?after=${last}`, {
            headers: { accept: "text/event-stream", ...(await headers()) },
            signal: opts.signal,
          });
          if (res.status === 401 || res.status === 404) throw new EginError(res.status, await res.text());
          if (!res.ok || !res.body) throw new Error(`events: HTTP ${res.status}`);
          delay = 500;
          for await (const event of parse(res.body)) {
            if (event.id !== undefined) {
              if (event.id <= last) continue;
              last = event.id;
            }
            yield event;
          }
        } catch (e) {
          if (opts.signal?.aborted || e instanceof EginError) throw e;
        }
        await sleep(delay, opts.signal);
        delay = Math.min(delay * 2, 10_000);
      }
    },
  };
}

/** Server-sent events to objects. Each frame carries `id:` (stored events only) and one `data:` line. */
async function* parse(body: ReadableStream<Uint8Array>): AsyncGenerator<EginEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      buf += decoder.decode(value, { stream: true });
      let end: number;
      while ((end = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, end);
        buf = buf.slice(end + 2);
        let id: number | undefined;
        let data = "";
        for (const line of frame.split("\n")) {
          if (line.startsWith("id:")) id = Number(line.slice(3).trim());
          else if (line.startsWith("data:")) data += line.slice(5).trimStart();
        }
        if (data) yield { ...JSON.parse(data), ...(id === undefined ? {} : { id }) };
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function sleep(ms: number, signal?: AbortSignal) {
  return new Promise<void>((resolve) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => (clearTimeout(t), resolve()), { once: true });
  });
}
