"use client"

import * as React from "react"
import Link from "next/link"
import { useAgent } from "egin-ts/react"
import { RiBarChartHorizontalLine, RiChatSmile2Line, RiEditBoxLine } from "@remixicon/react"

import { Button } from "@/components/ui/button"
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"
import { useMountEffect } from "@/hooks/use-mount-effect"
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
} from "@/components/ui/message-scroller"
import { ChatMessage, Thinking } from "./chat-message"
import { Composer } from "./composer"
import { ModelPicker } from "./model-picker"

const SESSION_KEY = "egin-session"
const SUGGESTIONS = ["What changed in the news today?", "Summarise the file I attach", "Write a script that renames photos by date"]

function savedSession() {
  try {
    return localStorage.getItem(SESSION_KEY)
  } catch {
    return null
  }
}

function saveSession(id: string | null) {
  try {
    if (id) localStorage.setItem(SESSION_KEY, id)
    else localStorage.removeItem(SESSION_KEY)
  } catch {}
}

const noop = () => () => {}

export function Chat() {
  const agent = useAgent({ baseUrl: "/egin", sessionId: savedSession(), onSessionChange: saveSession })
  const busy = agent.status === "busy"
  // The turn being worked on: messages sent mid-run wait below it until the run takes them up.
  const last = agent.messages.findLast((m) => !m.queued)
  // The saved session is read from localStorage, which the server render does not have; until hydration the
  // header renders as the server did, without it.
  const hydrated = React.useSyncExternalStore(noop, () => true, () => false)
  const sessionId = hydrated ? agent.sessionId : null
  // The agent's own model until the person picks another; the server keeps a pick for the session's later turns.
  const [model, setModel] = React.useState<string | null>(null)
  const [effort, setEffort] = React.useState<string | null>(null)
  const send = (text: string, files: File[] = []) => void agent.send(text, files, { model: model ?? undefined, reasoningEffort: effort ?? undefined })

  // Esc stops the answer being written. The listener is set once; the event reads the latest state each time.
  const onEscape = React.useEffectEvent(() => {
    if (busy) void agent.cancel()
  })
  useMountEffect(() => {
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && onEscape()
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  })

  return (
    <MessageScrollerProvider autoScroll>
      {/* The scroller spans the window so its scrollbar sits at the edge; each row centres its own content. */}
      <div className="flex h-dvh w-full flex-col">
        <header className="mx-auto flex w-full max-w-3xl items-center justify-between px-4 py-3">
          <span className="text-sm font-medium">Agent</span>
          <div className="flex items-center gap-1">
            <Button variant="ghost" size="icon-sm" aria-label="Trace of this chat" nativeButton={false} render={<Link href={sessionId ? `/traces/${sessionId}` : "/traces"} transitionTypes={["nav-forward"]} />}>
              <RiBarChartHorizontalLine />
            </Button>
            <Button variant="ghost" size="icon-sm" aria-label="New chat" onClick={agent.newSession} disabled={!sessionId}>
              <RiEditBoxLine />
            </Button>
          </div>
        </header>

        <div className="min-h-0 flex-1">
          {agent.messages.length === 0 && agent.status !== "resuming" ? (
            <Empty className="h-full">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <RiChatSmile2Line />
                </EmptyMedia>
                <EmptyTitle>What should the agent do?</EmptyTitle>
                <EmptyDescription>
                  It can search the web, read files you attach and run code in its own sandbox. Anything that changes files waits for your approval.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          ) : (
            <MessageScroller>
              <MessageScrollerViewport>
                <MessageScrollerContent aria-busy={busy} className="mx-auto w-full max-w-3xl px-4 py-2">
                  {agent.messages.map((message) => (
                    <MessageScrollerItem key={message.id} messageId={message.id} scrollAnchor={message.role === "user"}>
                      <ChatMessage message={message} working={busy && message === last} onApprove={(callId, allow) => void agent.approve(callId, allow)} onAnswer={(callId, text) => void agent.answer(callId, text)} />
                    </MessageScrollerItem>
                  ))}
                  {busy && last?.role === "user" && <Thinking />}
                  {agent.error && (
                    <p role="alert" className="text-sm text-destructive">
                      {agent.error}
                    </p>
                  )}
                </MessageScrollerContent>
              </MessageScrollerViewport>
              <MessageScrollerButton />
            </MessageScroller>
          )}
        </div>

        <div className="mx-auto flex w-full max-w-3xl flex-col gap-2 px-4 pt-2 pb-4">
          {agent.messages.length === 0 && (
            // shadcn's Suggestion: prompts to start from, in one row above the field.
            <div className="flex scroll-fade-x scrollbar-none gap-2 overflow-x-auto overscroll-x-contain">
              {SUGGESTIONS.map((s) => (
                <Button key={s} variant="outline" size="sm" className="shrink-0" onClick={() => send(s)}>
                  {s}
                </Button>
              ))}
            </div>
          )}
          <Composer busy={busy} loading={agent.status === "resuming"} onSend={send} onStop={() => void agent.cancel()}>
            {agent.info && (
              <ModelPicker
                info={agent.info}
                model={model ?? agent.info.model}
                onModel={(m) => {
                  setModel(m)
                  // Efforts differ by model: keep the pick only if the new model takes it.
                  if (effort && !agent.info?.reasoning_efforts[m]?.includes(effort)) setEffort(null)
                }}
                effort={effort}
                onEffort={setEffort}
              />
            )}
          </Composer>
        </div>
      </div>
    </MessageScrollerProvider>
  )
}
