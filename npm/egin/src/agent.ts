// Conversation state for one egin session, built from its event stream. Framework-free:
// `egin-ts/react` wraps it with useSyncExternalStore, and any other UI can subscribe the same way.

import { createClient, EginError, type AgentInfo, type Attachment, type Client, type ClientOptions, type EginEvent, type TurnOptions } from "./client.js";

export type ToolState = "pending" | "approval" | "question" | "running" | "done" | "denied";

export type Part =
  | { type: "text"; text: string }
  /** Structured output a tool sent for the UI (a `::data` line): a chart, a card, a table. */
  | { type: "data"; tool: string; callId?: string; data: unknown }
  | {
      type: "tool";
      callId: string;
      tool: string;
      args: unknown;
      state: ToolState;
      output?: string;
      /** For an `ask_user` call waiting on `answer`: the question, and the choices if it gave any. */
      question?: { text: string; options: string[] };
      /** For a call made by a subagent: the subagent's name and the session it runs as. */
      agent?: string;
      session?: string;
    };

type ToolPart = Extract<Part, { type: "tool" }>;

export interface Message {
  id: string;
  role: "user" | "assistant";
  parts: Part[];
  /** Files sent with a user message. While pending, `path` is empty. */
  files?: Attachment[];
  /** A user message shown before the server confirmed it. */
  pending?: boolean;
  /** Sent while a turn was running: it waits until that run takes it up. */
  queued?: boolean;
}

export type Status = "idle" | "busy" | "resuming";

export interface Snapshot {
  sessionId: string | null;
  messages: Message[];
  status: Status;
  error: string | null;
  /** Tool calls waiting for `approve`, the subagents' included. */
  approvals: ToolPart[];
  /** `ask_user` calls waiting for `answer`. */
  questions: ToolPart[];
  /** With `historyLimit`, whether older turns are left to `loadEarlier`. */
  hasEarlier: boolean;
  /** The models and reasoning efforts the agent offers, once loaded; for a picker. */
  info: AgentInfo | null;
}

export interface AgentOptions extends ClientOptions {
  /** Continue this session: its history is replayed, and a running turn is followed. */
  sessionId?: string | null;
  /** Called when a session is created or cleared, so you can save the id. */
  onSessionChange?: (id: string | null) => void;
  /** Called with every event, including token deltas. */
  onEvent?: (event: EginEvent) => void;
  /** Load a saved session's last turns only, about this many events, instead of its whole log;
   * `loadEarlier` loads the turns before them. */
  historyLimit?: number;
}

export class Agent {
  readonly client: Client;
  private snap: Snapshot;
  private listeners = new Set<() => void>();
  private stream: AbortController | null = null;
  /** Subagent sessions, followed for their approval requests: each one's name, and the turn and `task` call
   * that started it. */
  private subagents = new Map<string, { agent: string; turn: string; task?: string }>();
  private children = new Map<string, AbortController>();
  private seq = 0;
  private open = false; // the last text part is still receiving tokens
  private lastId = 0;
  private resumed = false;
  private infoRequested = false;
  /** Stored events applied so far, kept to rebuild the conversation when earlier turns load. */
  private stored: EginEvent[] = [];
  private earlier: number | null = null;

  constructor(private options: AgentOptions = {}) {
    this.client = createClient(options);
    this.snap = { sessionId: options.sessionId ?? null, messages: [], status: "idle", error: null, approvals: [], questions: [], hasEarlier: false, info: null };
  }

  /** Start (or restart) following the session's stream. Safe to call more than once. */
  start() {
    if (!this.infoRequested) {
      this.infoRequested = true;
      // An older server without `GET /agent` just leaves `info` empty.
      this.client.agentInfo().then((info) => this.set({ info }), () => {});
    }
    const id = this.snap.sessionId;
    if (!id || this.stream) return;
    if (this.resumed) this.follow(id, this.lastId);
    else void this.resume(id);
    // A restart replays only newer events, so the subagents already named are followed again here.
    for (const session of this.subagents.keys()) this.followChild(session);
  }

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  getSnapshot = () => this.snap;

  /** Send a message, with optional files, creating the session on first use. `opts` picks the model
   * and reasoning effort for this turn and the ones after it. */
  async send(text: string, files: File[] = [], opts?: TurnOptions) {
    const message = text.trim();
    if (!message && files.length === 0) return;
    const local: Message = {
      id: `local-${++this.seq}`,
      role: "user",
      parts: [{ type: "text", text: message }],
      pending: true,
      ...(this.snap.status === "busy" ? { queued: true } : {}),
      ...(files.length ? { files: files.map((f) => ({ name: f.name, path: "", size: f.size })) } : {}),
    };
    this.set({ messages: [...this.snap.messages, local], status: "busy", error: null });
    try {
      let id = this.snap.sessionId;
      if (!id) {
        // A first message with no files to upload goes with the session's creation: one request
        // instead of two, and the turn starts with the session.
        const first = files.length || !message ? undefined : message;
        ({ id } = await this.client.createSession(first, first === undefined ? undefined : opts));
        this.resumed = true;
        this.set({ sessionId: id });
        this.options.onSessionChange?.(id);
        this.follow(id, 0);
        if (first !== undefined) return;
      }
      const uploaded = await Promise.all(files.map((f) => this.client.upload(id!, f)));
      await this.client.send(id, message || "See the attached files.", uploaded, opts);
    } catch (e) {
      this.set({ messages: this.snap.messages.filter((m) => m !== local), status: "idle", error: String(e) });
    }
  }

  /** Answer a call waiting for approval, whether this session's or one of its subagents'. */
  async approve(callId: string, allow: boolean) {
    const session = this.snap.approvals.find((p) => p.callId === callId)?.session ?? this.snap.sessionId;
    if (!session) return;
    try {
      await this.client.approve(session, callId, allow);
    } catch (e) {
      this.set({ error: String(e) });
    }
  }

  /** With `historyLimit`: load the turns before the earliest one shown. */
  async loadEarlier() {
    const id = this.snap.sessionId;
    if (!id || this.earlier === null) return;
    try {
      const page = await this.client.history(id, { before: this.earlier, limit: this.options.historyLimit ?? 500 });
      this.earlier = page.earlier;
      // Rebuild from the whole log seen so far; messages still on their way are kept at the end.
      const pending = this.snap.messages.filter((m) => m.pending);
      const all = [...page.events, ...this.stored];
      this.stored = [];
      this.lastId = 0;
      this.open = false;
      this.snap = { ...this.snap, messages: [] };
      const status = this.snap.status;
      for (const e of all) this.apply(e);
      this.set({ messages: [...this.snap.messages, ...pending], hasEarlier: page.earlier !== null, status });
    } catch (e) {
      this.set({ error: String(e) });
    }
  }

  /** Answer an `ask_user` question of this session. */
  async answer(callId: string, text: string) {
    if (!this.snap.sessionId) return;
    try {
      await this.client.answer(this.snap.sessionId, callId, text);
    } catch (e) {
      this.set({ error: String(e) });
    }
  }

  async cancel() {
    if (!this.snap.sessionId || this.snap.status !== "busy") return;
    try {
      await this.client.cancel(this.snap.sessionId);
    } catch (e) {
      this.set({ error: String(e) });
    }
  }

  /** Forget the current session locally and start fresh on the next `send`. */
  newSession() {
    this.stop();
    this.open = false;
    this.lastId = 0;
    this.resumed = false;
    this.subagents.clear();
    this.stored = [];
    this.earlier = null;
    this.snap = { sessionId: null, messages: [], status: "idle", error: null, approvals: [], questions: [], hasEarlier: false, info: this.snap.info };
    this.emit();
    this.options.onSessionChange?.(null);
  }

  /** Stop following the stream. Call when the UI goes away. */
  stop() {
    this.stream?.abort();
    this.stream = null;
    for (const ctl of this.children.values()) ctl.abort();
    this.children.clear();
  }

  /** Note a subagent's session the first time an event names it. */
  private addSubagent(session: string, agent: string, turn: string, task?: string) {
    if (!this.subagents.has(session)) this.subagents.set(session, { agent, turn, task });
    this.followChild(session);
  }

  /** Follow a subagent's session, and its own subagents', for the calls that wait on an approval. */
  private followChild(session: string) {
    if (this.children.has(session)) return;
    const ctl = new AbortController();
    this.children.set(session, ctl);
    void (async () => {
      try {
        for await (const e of this.client.events(session, { signal: ctl.signal })) this.applyChild(session, e);
      } catch {
        // Aborted, or the subagent's session is gone: its calls can no longer be answered anyway.
      }
    })();
  }

  private applyChild(session: string, e: EginEvent) {
    const { agent, turn, task } = this.subagents.get(session)!;
    if (e.type === "subagent" && typeof e.session === "string") return this.addSubagent(e.session, String(e.agent ?? agent), turn, task);
    if (e.type !== "approval_request" && e.type !== "approval" && e.type !== "tool_result") return;
    const ours = (p: Part) => p.type === "tool" && p.session === session && p.callId === e.call_id;
    const at = this.snap.messages.findIndex((m) => m.parts.some(ours));
    const messages = this.snap.messages.slice();
    if (e.type === "approval_request") {
      if (at >= 0) return; // replayed after a reconnect
      // The question joins the turn that handed off to the subagent.
      const i = messages.findIndex((m) => m.id === turn);
      if (i < 0) return;
      const part: ToolPart = { type: "tool", callId: String(e.call_id), tool: String(e.tool ?? ""), args: e.args, state: "approval", agent, session };
      // Right under the `task` step, after this subagent's earlier questions; at the end if that step is not found.
      const parts = messages[i].parts;
      const below = lastIndex(parts, (p) => p.type === "tool" && (p.callId === task || p.session === session));
      const slot = below < 0 ? parts.length : below + 1;
      messages[i] = { ...messages[i], parts: [...parts.slice(0, slot), part, ...parts.slice(slot)] };
    } else {
      // Only calls that asked for an approval are shown; the subagent's other calls stay in its own trace.
      if (at < 0) return;
      const update = (p: Part): Part => {
        if (!ours(p)) return p;
        return e.type === "tool_result" ? { ...(p as ToolPart), state: "done", output: String(e.output ?? "") } : { ...(p as ToolPart), state: e.allow ? "running" : "denied" };
      };
      messages[at] = { ...messages[at], parts: messages[at].parts.map(update) };
    }
    this.set({ messages, approvals: pending(messages) });
  }

  private async resume(id: string) {
    this.resumed = true;
    this.set({ status: "resuming" });
    try {
      await this.client.getSession(id);
      // A long chat opens at its last turns; the stream then follows from there.
      if (this.options.historyLimit && this.lastId === 0) {
        const page = await this.client.history(id, { limit: this.options.historyLimit });
        for (const e of page.events) this.apply(e);
        this.earlier = page.earlier;
        this.set({ hasEarlier: page.earlier !== null });
      }
      // Unless a stream started meanwhile (a quick stop and start) has already replayed the turn.
      if (this.snap.status === "resuming") this.set({ status: "idle" });
      this.follow(id, this.lastId);
    } catch (e) {
      if (e instanceof EginError && e.status === 404) {
        // The session no longer exists: start over rather than show an error forever.
        this.newSession();
        this.set({ error: String(e) });
        return;
      }
      // The server is down or failing: keep the session and reconnect, which retries until it answers.
      this.set({ status: "idle", error: String(e) });
      this.follow(id, this.lastId);
    }
  }

  private follow(id: string, after: number) {
    // Only the session's own stream restarts; subagents' streams carry on, since a resumed stream
    // after `after` does not replay the events that named them.
    this.stream?.abort();
    const ctl = new AbortController();
    this.stream = ctl;
    void (async () => {
      try {
        for await (const e of this.client.events(id, { after, signal: ctl.signal })) {
          this.options.onEvent?.(e);
          this.apply(e);
        }
      } catch (e) {
        if (!ctl.signal.aborted) this.set({ status: "idle", error: String(e) });
      }
    })();
  }

  private apply(e: EginEvent) {
    if (e.id !== undefined) {
      if (e.id <= this.lastId) return;
      this.lastId = e.id;
      this.stored.push(e);
    }
    const messages = this.snap.messages.slice();
    // Events arriving again means the server is back: a connection error no longer applies.
    const patch: Partial<Snapshot> = this.snap.error ? { error: null } : {};
    // The assistant message for the current turn, created on first use. Messages sent mid-run and not
    // yet taken up by the run stay below it.
    const turn = () => {
      let end = messages.length;
      while (end > 0 && messages[end - 1].pending) end--;
      let last = messages[end - 1];
      if (!last || last.role !== "assistant") {
        last = { id: `a-${e.id ?? ++this.seq}`, role: "assistant", parts: [] };
        messages.splice(end, 0, last);
      } else {
        last = { ...last, parts: last.parts.slice() };
        messages[end - 1] = last;
      }
      return last;
    };
    const tool = (callId: unknown, update: Partial<Extract<Part, { type: "tool" }>>) => {
      const m = turn();
      const i = m.parts.findIndex((p) => p.type === "tool" && p.callId === callId);
      if (i >= 0) m.parts[i] = { ...(m.parts[i] as Extract<Part, { type: "tool" }>), ...update };
    };

    switch (e.type) {
      case "user": {
        const text = String(e.text ?? "");
        const files = Array.isArray(e.files) ? (e.files as Attachment[]) : undefined;
        // The confirmed message takes the place of the oldest pending one (its text may have been filled in)
        // and keeps its id, so a UI keyed on ids, such as a scroller anchoring the turn, sees the same row.
        const confirmed: Message = { id: `u-${e.id}`, role: "user", parts: [{ type: "text", text }], ...(files ? { files } : {}) };
        const local = messages.findIndex((m) => m.pending);
        if (local >= 0) messages[local] = { ...confirmed, id: messages[local].id };
        else messages.push(confirmed);
        this.open = false;
        patch.status = "busy";
        break;
      }
      case "queued": {
        // A message sent mid-run that the run has not taken up yet. The sender already shows it; a
        // reload or another tab learns of it here.
        const text = String(e.text ?? "");
        const mine = messages.some((m) => m.queued && m.parts[0]?.type === "text" && m.parts[0].text === text);
        if (!mine) messages.push({ id: `q-${e.id}`, role: "user", parts: [{ type: "text", text }], pending: true, queued: true });
        break;
      }
      case "token": {
        const m = turn();
        const lastPart = m.parts[m.parts.length - 1];
        if (this.open && lastPart?.type === "text") m.parts[m.parts.length - 1] = { type: "text", text: lastPart.text + String(e.text ?? "") };
        else m.parts.push({ type: "text", text: String(e.text ?? "") });
        this.open = true;
        break;
      }
      case "assistant": {
        const m = turn();
        const content = typeof e.content === "string" ? e.content : "";
        const lastPart = m.parts[m.parts.length - 1];
        // The stored reply is authoritative: it replaces the streamed text, or stands in for it on replay.
        if (this.open && lastPart?.type === "text") m.parts[m.parts.length - 1] = { type: "text", text: content };
        else if (content) m.parts.push({ type: "text", text: content });
        this.open = false;
        for (const c of (e.tool_calls as { id: string; function: { name: string; arguments: string } }[] | null) ?? []) {
          let args: unknown = c.function.arguments;
          try {
            args = JSON.parse(c.function.arguments);
          } catch {}
          m.parts.push({ type: "tool", callId: c.id, tool: c.function.name, args, state: "pending" });
        }
        break;
      }
      case "tool_call": // runs now unless an approval_request follows
        tool(e.call_id, { state: "running" });
        break;
      case "approval_request":
        tool(e.call_id, { state: "approval" });
        break;
      case "approval":
        tool(e.call_id, { state: e.allow ? "running" : "denied" });
        break;
      case "data":
        turn().parts.push({ type: "data", tool: String(e.tool ?? ""), ...(typeof e.call_id === "string" ? { callId: e.call_id } : {}), data: e.data });
        break;
      case "question":
        tool(e.call_id, { state: "question", question: { text: String(e.question ?? ""), options: Array.isArray(e.options) ? e.options.map(String) : [] } });
        break;
      case "answer":
        tool(e.call_id, { state: "running", question: undefined });
        break;
      case "tool_started":
        tool(e.call_id, { state: "running" });
        break;
      case "tool_result":
        tool(e.call_id, { state: "done", output: String(e.output ?? "") });
        break;
      case "subagent":
        // The `task` call that starts a subagent is already in this turn: the newest one not yet done.
        if (typeof e.session === "string") {
          const m = turn();
          const task = m.parts[lastIndex(m.parts, (p) => p.type === "tool" && p.tool === "task" && p.state !== "done")];
          this.addSubagent(e.session, String(e.agent ?? "subagent"), m.id, task?.type === "tool" ? task.callId : undefined);
        }
        break;
      case "done":
      case "cancelled":
        patch.status = "idle";
        break;
      case "error":
        patch.status = "idle";
        patch.error = String(e.message ?? "the run failed");
        break;
      default:
        return; // events the conversation view does not show
    }
    patch.messages = messages;
    patch.approvals = pending(messages);
    patch.questions = pending(messages, "question");
    this.set(patch);
  }

  private set(patch: Partial<Snapshot>) {
    this.snap = { ...this.snap, ...patch };
    this.emit();
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }
}

/** `findLastIndex`, which the Node 18 this package supports lacks. */
function lastIndex<T>(items: T[], pred: (item: T) => boolean): number {
  for (let i = items.length - 1; i >= 0; i--) if (pred(items[i])) return i;
  return -1;
}

function pending(messages: Message[], state: ToolState = "approval"): ToolPart[] {
  return messages.flatMap((m) => m.parts.filter((p): p is ToolPart => p.type === "tool" && p.state === state));
}
