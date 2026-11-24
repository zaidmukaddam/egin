import * as React from "react"
import { createClient } from "egin-ts/client"

import { describeArgs } from "@/components/chat/tool-step"
import { useMountEffect } from "@/hooks/use-mount-effect"
import { modelName } from "@/lib/model-name"

export type SpanKind = "agent" | "model" | "tool"

export interface Span {
  id: string
  kind: SpanKind
  label: string
  detail?: string
  start: number
  /** null while it runs. */
  end: number | null
  /** Time spent waiting for an approval, inside a tool call. */
  waiting?: [number, number | null]
  cost?: number
  tokens?: number
  failed?: boolean
  children: Span[]
}

export interface Trace {
  root: Span | null
  /** Sessions recorded before egin stamped its events carry no times. */
  untimed: boolean
}

type Body = Record<string, unknown> & { type: string; at?: number }

const END = new Set(["done", "error", "cancelled"])

/** One session's events as a span, with its subagents' sessions nested under the call that started them. */
function build(id: string, label: string, sessions: Map<string, Body[]>): Span | null {
  const events = sessions.get(id)?.filter((e) => typeof e.at === "number") ?? []
  if (!events.length) return null
  const agent: Span = { id, kind: "agent", label, start: events[0].at!, end: null, cost: 0, tokens: 0, children: [] }
  const open = new Map<string, Span>()
  let lastTool: Span | null = null
  for (const e of events) {
    const at = e.at!
    if (e.type === "assistant") {
      const usage = (e.usage ?? {}) as Record<string, number>
      const tokens = (usage.prompt ?? 0) + (usage.completion ?? 0)
      const ms = typeof e.ms === "number" ? e.ms : 0
      const cost = typeof e.cost === "number" ? e.cost : undefined
      agent.cost! += cost ?? 0
      agent.tokens! += tokens
      agent.children.push({ id: `${id}:m${at}`, kind: "model", label: typeof e.model === "string" ? modelName(e.model) : "Model", detail: `${tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : tokens} tokens`, start: at - ms, end: at, cost, tokens, children: [] })
    } else if (e.type === "tool_call") {
      // The same call again: egin restarted while it ran, and runs it anew.
      const before = open.get(String(e.call_id))
      if (before) before.end = at
      const span: Span = { id: `${id}:${e.call_id}:${at}`, kind: "tool", label: String(e.tool), detail: describeArgs(e.args), start: at, end: null, children: [] }
      open.set(String(e.call_id), span)
      agent.children.push(span)
      lastTool = span
    } else if (e.type === "approval_request") {
      const span = open.get(String(e.call_id))
      if (span) span.waiting = [at, null]
    } else if (e.type === "approval") {
      const span = open.get(String(e.call_id))
      if (span?.waiting) span.waiting[1] = at
    } else if (e.type === "tool_result") {
      const span = open.get(String(e.call_id))
      if (span) {
        span.end = at
        span.failed = String(e.output ?? "").startsWith("error:")
        open.delete(String(e.call_id))
      }
    } else if (e.type === "subagent" && typeof e.session === "string") {
      const child = build(e.session, String(e.agent ?? "Subagent"), sessions)
      if (child) {
        // The subagent's own row stands in for the task call that started it, with its prompt and any wait.
        const at = lastTool?.label === "task" ? agent.children.indexOf(lastTool) : -1
        if (lastTool && at >= 0) {
          child.start = lastTool.start
          child.detail = lastTool.detail
          child.waiting = lastTool.waiting
          agent.children[at] = child
          lastTool = null
        } else {
          agent.children.push(child)
        }
        agent.cost! += child.cost ?? 0
        agent.tokens! += child.tokens ?? 0
      }
    } else if (END.has(e.type)) {
      agent.end = at
      agent.failed = e.type === "error"
    }
  }
  // A finished turn followed by a new message runs again.
  if (agent.end !== null && !END.has(events.at(-1)!.type)) agent.end = null
  // Calls cut off by a stop or a crash end with the run.
  if (agent.end !== null) for (const span of open.values()) span.end = agent.end
  return agent
}

/** A session's trace, live: the stored events, then new ones as they happen, for it and its subagents. It
 * follows the session it mounted with; render the component with `key={sessionId}` to follow another. */
export function useTrace(sessionId: string, baseUrl = "/egin"): Trace {
  const [sessions, setSessions] = React.useState(() => new Map<string, Body[]>())

  useMountEffect(() => {
    const client = createClient({ baseUrl })
    const abort = new AbortController()
    const following = new Set<string>()
    const follow = async (id: string) => {
      if (following.has(id)) return
      following.add(id)
      try {
        for await (const event of client.events(id, { signal: abort.signal })) {
          const body = event as Body
          if (body.type === "token" || body.type === "reasoning") continue
          setSessions((prev) => new Map(prev).set(id, [...(prev.get(id) ?? []), body]))
          if (body.type === "subagent" && typeof body.session === "string") void follow(body.session)
        }
      } catch {
        // Aborted on unmount; a dropped stream is reconnected by the client.
      }
    }
    void follow(sessionId)
    return () => abort.abort()
  })

  return React.useMemo(() => {
    const events = sessions.get(sessionId) ?? []
    return { root: build(sessionId, "Agent", sessions), untimed: events.length > 0 && events.every((e) => typeof e.at !== "number") }
  }, [sessions, sessionId])
}
