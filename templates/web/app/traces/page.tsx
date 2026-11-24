import { DirectionalTransition } from "@/components/directional-transition"
import { TraceList } from "@/components/trace/trace-list"

export const metadata = { title: "Traces" }

export default function Page() {
  return (
    <DirectionalTransition>
      <TraceList />
    </DirectionalTransition>
  )
}
