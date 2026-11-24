// One egin session as one run of a Workflow SDK workflow (https://workflow-sdk.dev). The run is the
// session's store: its state is the conversation, each step is one `egin step` (a turn, or a minute
// of a long one), and the workflow's world saves the state after every step. On Vercel that world is
// Vercel Workflow, so there is no database to set up and no process to keep alive: any function
// instance takes the next step, and a step whose instance died is run again.
//
// Your app's compiler has to build this file. Re-export it from a file in your workflows folder:
//
//   // workflows/egin.ts
//   export * from "egin-ts/workflow/session";
//
// The run's id is the session id. Between turns it waits on a hook for messages, and when a tool call
// needs a yes or an answer it rests until one arrives. A step writes to three streams as it goes: the
// session's stored events (the default one), token deltas ("live"), and what the session is now
// ("state"). It reads a fourth, "cancel", to stop when asked. `createWorkflowApi` is the other end of
// all four.
import { createHook, getStepMetadata, getWorkflowMetadata, getWritable } from "workflow";
import { getRun } from "workflow/api";
import { step, type AgentRef, type Answer } from "./run.js";

/** The conversation, deeply frozen: the world then stores each step's change, not the whole of it again. */
export type Session = Readonly<{ messages: readonly unknown[]; events: readonly Readonly<Record<string, unknown>>[]; stats: unknown; status: string }>;
/** A stored event with its id: its position in the session's log, counted from 1. */
export type Stored = { id: number; e: Record<string, unknown> };
/** An entry of the default stream: a stored event, or `back`, written by a step that is running
 * again: the events after that id, which its earlier attempt wrote, are replaced by what follows. */
export type Entry = Stored | { back: number };
/** A token delta. `n` is how many events were stored when it was written: it follows them. */
export type Live = { n: number; e: Record<string, unknown> };
/** What the session is now. `v` is the version of this file's streams and hooks that the run speaks. */
export type State = { v: typeof V; status: string; owner: string | null; events: number; stats: unknown };
/** A run started by an older version keeps speaking that one, so `createWorkflowApi` answers for
 * its session as for one that does not exist, and a client starts a new one. */
export const V = 2;
/** A request to cancel the turn in progress, made when the session had `n` stored events. */
export type Cancel = { n: number };

/** Hook tokens. A message's carries the owner, so one sent by anyone else finds no hook. */
export const messagesTo = (id: string, owner: string | null) => `egin:${id}:${owner ?? ""}`;
export const cancelsTo = (id: string) => `egin:${id}:cancel`;
export const answersTo = (id: string) => `egin:${id}:answer`;

/** How long one step goes on starting model calls. A turn that takes longer continues in the next
 * step, so a function's time limit never cuts one, and an instance that dies costs at most this
 * much of the turn again. */
const SECONDS = 60;

const freeze = <T>(value: T): T => {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const v of Object.values(value)) freeze(v);
  }
  return value;
};

const EMPTY: Session = freeze({ messages: [], events: [], stats: {}, status: "idle" });

type Input = { message?: string; answers?: Answer[]; cancel?: boolean };

/** A session nobody has written to yet shows as idle. */
async function open(owner: string | null) {
  "use step";
  const state = getWritable<State>({ namespace: "state" }).getWriter();
  await state.write({ v: V, status: "idle", owner, events: 0, stats: {} });
  state.releaseLock();
}

/** One `egin step` on the session, each event saved and shown as it happens. `turn` is how many
 * events the session had when the turn began: a cancel made since then is for this turn. */
async function advance(agent: AgentRef, id: string, owner: string | null, session: Session, input: Input, turn: number): Promise<Session> {
  "use step";
  const log = getWritable<Entry>().getWriter();
  const live = getWritable<Live>({ namespace: "live" }).getWriter();
  const state = getWritable<State>({ namespace: "state" }).getWriter();
  const stop = new AbortController();
  const cancels = getRun(id).getReadable<Cancel>({ namespace: "cancel", startIndex: -1 }).getReader();
  void (async () => {
    for (;;) {
      const { value, done } = await cancels.read();
      if (done) return;
      if (value.n >= turn) return stop.abort();
    }
  })().catch(() => {});
  let count = session.events.length;
  // A write is accepted at once and sent in order; the step does not end before all of them landed.
  const put = <T>(to: WritableStreamDefaultWriter<T>, chunk: T) => void to.write(chunk).catch(() => {});
  try {
    if (getStepMetadata().attempt > 1) put(log, { back: count });
    if (!input.cancel) put(state, { v: V, status: "running", owner, events: count, stats: session.stats });
    const out = await step(
      agent,
      { id, messages: session.messages, events: session.events, stats: session.stats, ...input, seconds: SECONDS },
      (e, stored) => (stored ? put(log, { id: ++count, e }) : put(live, { n: count, e })),
      stop.signal,
    );
    for (let i = count - session.events.length; i < out.events.length; i++) put(log, { id: ++count, e: out.events[i] });
    // A message this step left alone stays the same object, which is how the world knows not to store it again.
    const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
    const messages = out.messages.map((m, i) => (i < session.messages.length && same(session.messages[i], m) ? session.messages[i] : freeze(m)));
    const next = freeze({ messages, events: [...session.events, ...out.events.map(freeze)], stats: out.stats, status: out.status });
    await state.write({ v: V, status: next.status, owner, events: next.events.length, stats: next.stats });
    return next;
  } finally {
    void cancels.cancel().catch(() => {});
    log.releaseLock();
    live.releaseLock();
    state.releaseLock();
  }
}

export async function eginSession(init: { agent: AgentRef; owner: string | null; message?: string }) {
  "use workflow";
  const { workflowRunId: id } = getWorkflowMetadata();
  const { agent, owner } = init;
  const messages = createHook<{ text: string }>({ token: messagesTo(id, owner) })[Symbol.asyncIterator]();
  const cancels = createHook<Cancel>({ token: cancelsTo(id) })[Symbol.asyncIterator]();
  const answers = createHook<Answer>({ token: answersTo(id) })[Symbol.asyncIterator]();
  let cancel = cancels.next();
  let answer = answers.next();

  let session = EMPTY;
  let text = init.message;
  if (text === undefined) await open(owner);
  for (;;) {
    if (text === undefined) {
      const next = await messages.next();
      if (next.done) return;
      text = next.value.text;
    }
    const turn = session.events.length;
    session = await advance(agent, id, owner, session, { message: text }, turn);
    text = undefined;
    // A turn is as many steps as it takes. A step in flight hears a cancel itself, on the "cancel"
    // stream; the hook is for a turn that is resting.
    while (session.status === "running" || session.status === "waiting") {
      let input: Input = {};
      // A tool call needs a yes or an answer before it runs. The turn rests here, holding no
      // compute, until one arrives or the turn is cancelled.
      while (session.status === "waiting" && !input.answers && !input.cancel) {
        const first = await Promise.race([answer.then((got) => ({ got })), cancel.then((asked) => ({ asked }))]);
        if ("got" in first) {
          answer = answers.next();
          if (first.got.done) return;
          input = { answers: [first.got.value] };
        } else {
          cancel = cancels.next();
          if (first.asked.done) return;
          // A cancel made before this turn began was for an earlier one.
          if (first.asked.value.n >= turn) input = { cancel: true };
        }
      }
      session = await advance(agent, id, owner, session, input, turn);
    }
  }
}
