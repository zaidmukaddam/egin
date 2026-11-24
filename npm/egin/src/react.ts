// React binding: `useAgent` keeps one Agent per component and renders its snapshot.

import { useState, useSyncExternalStore } from "react";
import { Agent, type AgentOptions, type Snapshot } from "./agent.js";

export type { Message, Part, Snapshot, Status, ToolState } from "./agent.js";
export type { AgentInfo, Attachment, TurnOptions } from "./client.js";
import type { TurnOptions } from "./client.js";

export interface UseAgent extends Snapshot {
  send: (text: string, files?: File[], opts?: TurnOptions) => Promise<void>;
  approve: (callId: string, allow: boolean) => Promise<void>;
  answer: (callId: string, text: string) => Promise<void>;
  loadEarlier: () => Promise<void>;
  cancel: () => Promise<void>;
  newSession: () => void;
}

/**
 * Connect a component to an egin agent. Options are read once, when the component mounts;
 * give the component a `key` to start over with different ones.
 */
export function useAgent(options: AgentOptions = {}): UseAgent {
  // The agent follows its session for exactly as long as React is subscribed to it: subscribing on mount
  // starts it, unsubscribing on unmount stops it. No effect needed.
  const [{ agent, subscribe }] = useState(() => {
    const agent = new Agent(options);
    const subscribe = (fn: () => void) => {
      const off = agent.subscribe(fn);
      agent.start();
      return () => {
        off();
        agent.stop();
      };
    };
    return { agent, subscribe };
  });
  const snap = useSyncExternalStore(subscribe, agent.getSnapshot, agent.getSnapshot);
  return {
    ...snap,
    send: (text, files, opts) => agent.send(text, files, opts),
    approve: (callId, allow) => agent.approve(callId, allow),
    answer: (callId, text) => agent.answer(callId, text),
    loadEarlier: () => agent.loadEarlier(),
    cancel: () => agent.cancel(),
    newSession: () => agent.newSession(),
  };
}
