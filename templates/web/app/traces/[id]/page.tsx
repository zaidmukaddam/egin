import { DirectionalTransition } from "@/components/directional-transition"
import { TraceView } from "@/components/trace/trace-view"

export const metadata = { title: "Trace" }

export default async function Page({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params
  return (
    <DirectionalTransition>
      {/* Keyed by the session, so moving to another trace starts a fresh view that follows the new session. */}
      <TraceView key={id} sessionId={id} />
    </DirectionalTransition>
  )
}
