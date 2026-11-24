// Svelte binding: a store (`$agent` in a component) with the agent's actions on it. It follows the
// store contract rather than importing Svelte, so it works with Svelte 4 and 5 alike.

import { Agent, type AgentOptions, type Snapshot } from "./agent.js";

export type { Message, Part, Snapshot, Status, ToolState } from "./agent.js";
export type { AgentInfo, Attachment, TurnOptions } from "./client.js";
import type { TurnOptions } from "./client.js";

export interface AgentStore {
  subscribe: (run: (value: Snapshot) => void) => () => void;
  send: (text: string, files?: File[], opts?: TurnOptions) => Promise<void>;
  approve: (callId: string, allow: boolean) => Promise<void>;
  answer: (callId: string, text: string) => Promise<void>;
  loadEarlier: () => Promise<void>;
  cancel: () => Promise<void>;
  newSession: () => void;
}

/** The stream is followed while the store has subscribers, and stopped when the last one leaves. */
export function createAgent(options: AgentOptions = {}): AgentStore {
  const agent = new Agent(options);
  let subscribers = 0;
  return {
    subscribe(run) {
      run(agent.getSnapshot());
      const off = agent.subscribe(() => run(agent.getSnapshot()));
      if (subscribers++ === 0) agent.start();
      return () => {
        off();
        if (--subscribers === 0) agent.stop();
      };
    },
    send: (text, files, opts) => agent.send(text, files, opts),
    approve: (callId, allow) => agent.approve(callId, allow),
    answer: (callId, text) => agent.answer(callId, text),
    loadEarlier: () => agent.loadEarlier(),
    cancel: () => agent.cancel(),
    newSession: () => agent.newSession(),
  };
}
