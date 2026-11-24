"use client"

import Link from "next/link"
import { RiArrowLeftLine } from "@remixicon/react"

import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { useTrace } from "@/lib/trace"
import { Waterfall } from "./waterfall"

export function TraceView({ sessionId }: { sessionId: string }) {
  const { root, untimed } = useTrace(sessionId)
  return (
    <main className="mx-auto w-full max-w-5xl px-4 py-6">
      <Button variant="ghost" size="sm" className="-ml-2 mb-4" nativeButton={false} render={<Link href="/traces" transitionTypes={["nav-back"]} />}>
        <RiArrowLeftLine />
        Traces
      </Button>
      {root ? (
        <Waterfall root={root} />
      ) : untimed ? (
        <p className="text-sm text-muted-foreground">This session ran on an egin that did not record event times yet, so it has no timeline.</p>
      ) : (
        <Skeleton className="h-80 w-full rounded-xl" />
      )}
    </main>
  )
}
