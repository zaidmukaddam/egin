// Vue binding: `useAgent` in a component's setup. Each field is a computed ref, so destructure freely.

import { computed, onScopeDispose, shallowRef } from "vue";
import { Agent, type AgentOptions } from "./agent.js";

export type { Message, Part, Snapshot, Status, ToolState } from "./agent.js";
export type { AgentInfo, Attachment, TurnOptions } from "./client.js";
import type { TurnOptions } from "./client.js";

export function useAgent(options: AgentOptions = {}) {
  const agent = new Agent(options);
  const snap = shallowRef(agent.getSnapshot());
  const off = agent.subscribe(() => (snap.value = agent.getSnapshot()));
  agent.start();
  onScopeDispose(() => {
    off();
    agent.stop();
  });
  return {
    messages: computed(() => snap.value.messages),
    status: computed(() => snap.value.status),
    error: computed(() => snap.value.error),
    approvals: computed(() => snap.value.approvals),
    questions: computed(() => snap.value.questions),
    hasEarlier: computed(() => snap.value.hasEarlier),
    sessionId: computed(() => snap.value.sessionId),
    info: computed(() => snap.value.info),
    send: (text: string, files?: File[], opts?: TurnOptions) => agent.send(text, files, opts),
    approve: (callId: string, allow: boolean) => agent.approve(callId, allow),
    answer: (callId: string, text: string) => agent.answer(callId, text),
    loadEarlier: () => agent.loadEarlier(),
    cancel: () => agent.cancel(),
    newSession: () => agent.newSession(),
  };
}
