"use client"

import * as React from "react"
import Link from "next/link"
import { createClient, type SessionSummary } from "egin-ts/client"
import { RiArrowLeftLine, RiArrowRightSLine } from "@remixicon/react"

import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { useMountEffect } from "@/hooks/use-mount-effect"
import { cn } from "@/lib/utils"

const when = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" })

/** Every session, newest first, each opening its trace. */
export function TraceList() {
  const [sessions, setSessions] = React.useState<SessionSummary[] | null>(null)
  const [error, setError] = React.useState<string | null>(null)

  useMountEffect(() => {
    let live = true
    createClient({ baseUrl: "/egin" })
      .listSessions(100)
      .then(
        // In a transition, so the placeholders fade out and the list fades in (see the ViewTransitions below).
        (list) => live && React.startTransition(() => setSessions(list)),
        (e) => live && setError(String(e.message ?? e))
      )
    return () => {
      live = false
    }
  })

  return (
    <main className="mx-auto w-full max-w-3xl px-4 py-6">
      <Button variant="ghost" size="sm" className="-ml-2 mb-4" nativeButton={false} render={<Link href="/" transitionTypes={["nav-back"]} />}>
        <RiArrowLeftLine />
        Chat
      </Button>
      <h1 className="mb-4 text-lg font-medium">Traces</h1>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          Could not load sessions: {error}
        </p>
      ) : !sessions ? (
        // Each state is its own ViewTransition, so one leaving and the other arriving is an exit and an enter.
        // `default="none"` keeps them still while the page itself slides in or out.
        <React.ViewTransition key="loading" exit="fade-out" default="none">
          <div className="flex flex-col gap-2" aria-busy="true" aria-label="Loading sessions">
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} className="h-12 w-full" />
            ))}
          </div>
        </React.ViewTransition>
      ) : (
        <React.ViewTransition key="loaded" enter="fade-in" default="none">
          {sessions.length === 0 ? (
            <p className="text-sm text-muted-foreground">No sessions yet. Each conversation in the chat shows up here.</p>
          ) : (
            <ul className="divide-y rounded-xl border">
              {sessions.map((s) => (
                <li key={s.id}>
                  <Link
                    href={`/traces/${s.id}`}
                    transitionTypes={["nav-forward"]}
                    className="flex items-center gap-3 px-4 py-3 text-sm hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
                  >
                    <span className={cn("size-2 shrink-0 rounded-full", s.status === "error" ? "bg-destructive" : s.status === "running" ? "animate-pulse bg-foreground" : "bg-muted-foreground/40")} aria-label={s.status} />
                    <span className="min-w-0 flex-1 truncate">{s.title ?? "Untitled session"}</span>
                    <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{when.format(s.created * 1000)}</span>
                    <RiArrowRightSLine className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </React.ViewTransition>
      )}
    </main>
  )
}
