"use client"

import * as React from "react"
import { RiArrowRightSLine, RiBarChartHorizontalLine, RiBrainLine, RiRobot2Line, RiSearchLine } from "@remixicon/react"

import { kind } from "@/components/chat/tool-step"
import { useMountEffect } from "@/hooks/use-mount-effect"
import { cn } from "@/lib/utils"
import type { Span } from "@/lib/trace"

const HATCH = "bg-[repeating-linear-gradient(135deg,var(--muted-foreground)_0_1.5px,transparent_1.5px_5px)]"
/** Subagents each get a hue; the top agent stays grey. */
const HUES = [250, 300, 55, 160, 20]
const INDENT = 20
const BASE = 28

function duration(ms: number) {
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`
  return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`
}

function cost(usd: number) {
  return usd >= 0.01 ? `$${usd.toFixed(2)}` : `$${usd.toFixed(4).replace(/0+$/, "")}`
}

function compact(n: number) {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n)
}

/** About five gridlines at round steps: 1, 2 or 5 times a power of ten. */
function ticks(total: number) {
  const raw = total / 5
  const pow = 10 ** Math.floor(Math.log10(raw || 1))
  const step = [1, 2, 5, 10].map((m) => m * pow).find((s) => s >= raw) ?? raw
  const out: number[] = []
  for (let t = 0; t <= total; t += step) out.push(t)
  return out
}

function count(span: Span, k: Span["kind"]): number {
  return (span.kind === k ? 1 : 0) + span.children.reduce((n, c) => n + count(c, k), 0)
}

function failures(span: Span): number {
  return (span.failed ? 1 : 0) + span.children.reduce((n, c) => n + failures(c), 0)
}

function matches(span: Span, query: string): boolean {
  return !query || `${span.label} ${span.detail ?? ""}`.toLowerCase().includes(query) || span.children.some((c) => matches(c, query))
}

/** A bar's colour: its agent's hue, brighter for model calls, dimmer for tools, a dark tint for the agent itself. */
function fill(span: Span, hue: number | null): React.CSSProperties {
  if (span.failed) return { background: "var(--destructive)" }
  const c = hue === null ? 0 : 1
  const shade = span.kind === "model" ? "model" : span.kind === "tool" ? "tool" : "group"
  return { background: `oklch(var(--trace-l-${shade}) calc(var(--trace-c-${shade}) * ${c}) ${hue ?? 0})` }
}

interface Ctx {
  root: Span
  total: number
  now: number
  query: string
  collapsed: Set<string>
  toggle: (id: string) => void
}

function pct(ctx: Ctx, t: number) {
  return ((t - ctx.root.start) / ctx.total) * 100
}

function Tile({ span, hue }: { span: Span; hue: number | null }) {
  const Glyph = span.kind === "agent" ? RiRobot2Line : span.kind === "model" ? RiBrainLine : kind(span.label).icon
  return (
    <span
      className="flex size-5.5 shrink-0 items-center justify-center rounded-[5px] border bg-background text-muted-foreground [&_svg]:size-3"
      style={span.kind === "model" && hue !== null ? { color: `oklch(var(--trace-l-model) var(--trace-c-model) ${hue})` } : undefined}
      aria-hidden
    >
      <Glyph />
    </span>
  )
}

function Bar({ ctx, span, hue }: { ctx: Ctx; span: Span; hue: number | null }) {
  const stop = span.end ?? ctx.now
  const left = pct(ctx, span.start)
  const width = Math.max(pct(ctx, stop) - left, 0)
  const group = span.kind === "agent"
  const label = [duration(stop - span.start), span.cost ? cost(span.cost) : null].filter(Boolean).join(" · ")
  const late = left + width > 88 && left > 30
  return (
    <div className="relative h-full flex-1" role="img" aria-label={`${span.label}: ${label}${span.failed ? ", failed" : ""}${span.end === null ? ", running" : ""}`}>
      <span
        className={cn("absolute top-1/2 min-w-1 -translate-y-1/2 overflow-hidden", group ? "h-5 rounded-[6px]" : "h-4 rounded-[5px]", span.end === null && !group && "animate-pulse")}
        style={{ left: `${left}%`, width: `${width}%`, ...fill(span, hue) }}
      >
        {/* An agent's row shows its own steps inside its bar, so a collapsed agent still reads. */}
        {group && span.waiting && (
          <span
            className={cn("absolute inset-y-1 rounded-[3px]", HATCH)}
            style={{ left: 0, width: `${(((span.waiting[1] ?? stop) - span.waiting[0]) / Math.max(stop - span.start, 1)) * 100}%` }}
          />
        )}
        {group &&
          span.children.map((c) => {
            const cs = pct(ctx, c.start)
            const ce = pct(ctx, c.end ?? ctx.now)
            return (
              <span
                key={c.id}
                className={cn("absolute inset-y-1 rounded-[3px]", c.waiting && !c.failed && HATCH)}
                style={{ left: `${((cs - left) / (width || 1)) * 100}%`, width: `max(${((ce - cs) / (width || 1)) * 100}%, 2px)`, ...(c.waiting && !c.failed ? {} : fill(c, c.kind === "agent" ? null : hue)) }}
              />
            )
          })}
        {!group && span.waiting && (
          <span
            className={cn("absolute inset-y-0 bg-card", HATCH)}
            style={{
              left: `${((span.waiting[0] - span.start) / Math.max(stop - span.start, 1)) * 100}%`,
              width: `${(((span.waiting[1] ?? stop) - span.waiting[0]) / Math.max(stop - span.start, 1)) * 100}%`,
            }}
          />
        )}
      </span>
      <span
        className={cn("absolute top-1/2 z-10 flex -translate-y-1/2 items-center text-xs whitespace-nowrap text-muted-foreground tabular-nums", late ? "pe-2" : "ps-2")}
        style={late ? { right: `${100 - left}%` } : { left: `${left + width}%` }}
      >
        {label}
      </span>
    </div>
  )
}

function Row({ ctx, span, depth, hue }: { ctx: Ctx; span: Span; depth: number; hue: number | null }) {
  const group = span.kind === "agent"
  const open = !ctx.collapsed.has(span.id) || !!ctx.query
  const pad = BASE + depth * INDENT
  let next = 0
  return (
    <div className="relative">
      <div
        className={cn(
          "group/row relative flex h-9 items-center before:pointer-events-none before:absolute before:inset-0 before:bg-foreground/5 before:opacity-0 hover:before:opacity-100",
          group && "sticky z-20 bg-card",
        )}
        style={group ? { top: depth * 36 } : undefined}
      >
        <div className="relative flex h-full w-(--trace-label) shrink-0 items-center gap-2 border-e pe-3" style={{ paddingInlineStart: pad }}>
          {group && span.children.length > 0 && (
            <button
              type="button"
              onClick={() => ctx.toggle(span.id)}
              aria-expanded={open}
              aria-label={`${open ? "Collapse" : "Expand"} ${span.label}`}
              className="absolute flex size-4 items-center justify-center rounded text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              style={{ left: pad - 20 }}
            >
              <RiArrowRightSLine className={cn("size-3.5 transition-transform duration-200 ease-out", open && "rotate-90")} />
            </button>
          )}
          <Tile span={span} hue={hue} />
          <span className="flex min-w-0 flex-1 items-center gap-1.5">
            <span className={cn("shrink-0", group ? "font-medium" : "", span.failed && "text-destructive")}>{span.label}</span>
            {span.detail && <span className="truncate text-muted-foreground">{span.detail}</span>}
          </span>
        </div>
        <div className="relative h-full flex-1 px-4">
          <Bar ctx={ctx} span={span} hue={hue} />
        </div>
      </div>

      {group && open && (
        <div className="relative">
          {/* The tree guide, down from this agent's icon. */}
          <span className="absolute inset-y-0 w-px bg-border" style={{ left: pad + 11 }} aria-hidden />
          {span.children
            .filter((c) => matches(c, ctx.query))
            .map((c) => (
              <Row key={c.id} ctx={ctx} span={c} depth={depth + 1} hue={c.kind === "agent" ? HUES[(depth + next++) % HUES.length] : hue} />
            ))}
        </div>
      )}
    </div>
  )
}

/** Moves the clock four times a second. Rendered only while the run is live, so the timer starts and stops
 * with it. */
function Ticker({ onTick }: { onTick: (now: number) => void }) {
  useMountEffect(() => {
    const timer = setInterval(() => onTick(Date.now()), 250)
    return () => clearInterval(timer)
  })
  return null
}

/** A run on one timeline: every model call, tool call and subagent, live while the run goes on. */
export function Waterfall({ root }: { root: Span }) {
  const running = root.end === null
  const [now, setNow] = React.useState(() => Date.now())
  const [collapsed, setCollapsed] = React.useState(() => new Set<string>())
  const [query, setQuery] = React.useState("")
  const search = React.useRef<HTMLInputElement>(null)

  // ⌘F / Ctrl+F jumps to the trace's own search instead of the browser's.
  useMountEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault()
        search.current?.focus()
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  })

  // The axis runs a little past the end, so the last bars keep room for their labels.
  const total = Math.max((root.end ?? now) - root.start, 1) * 1.18
  const ctx: Ctx = {
    root,
    total,
    now,
    query: query.trim().toLowerCase(),
    collapsed,
    toggle: (id) =>
      setCollapsed((prev) => {
        const next = new Set(prev)
        if (!next.delete(id)) next.add(id)
        return next
      }),
  }
  const grid = ticks(total)

  const stats: [string, string][] = [
    ["Duration", duration((root.end ?? now) - root.start)],
    ["Cost", cost(root.cost ?? 0)],
    ["Tokens", compact(root.tokens ?? 0)],
    ["Model calls", String(count(root, "model"))],
    ["Tool calls", String(count(root, "tool"))],
    ["Errors", String(failures(root))],
  ]

  return (
    <section aria-label="Trace" className="@container/trace w-full overflow-hidden rounded-xl border bg-card text-sm text-card-foreground [--trace-label:15rem] md:[--trace-label:20rem]">
      {running && <Ticker onTick={setNow} />}
      <div className="flex items-center gap-4 border-b px-4 py-3">
        <span className="flex items-center gap-2 font-semibold">
          Trace
          {running && (
            <span className="flex items-center gap-1.5 text-xs font-normal text-muted-foreground">
              <span className="size-1.5 animate-pulse rounded-full bg-foreground" aria-hidden />
              Live
            </span>
          )}
        </span>
        <label className="ms-auto flex h-8 w-1/3 min-w-44 cursor-text items-center gap-2 rounded-lg border border-foreground/10 bg-foreground/[0.03] ps-2.5 pe-1.5 text-muted-foreground transition-[border-color,background-color] duration-150 ease-out focus-within:border-foreground/25 hover:border-foreground/20">
          <RiSearchLine className="size-3.5 shrink-0" aria-hidden />
          <input
            ref={search}
            type="search"
            placeholder="Search"
            aria-label="Search steps"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground [&::-webkit-search-cancel-button]:appearance-none"
          />
          <kbd className="rounded border px-1 font-sans text-[11px] leading-4 text-muted-foreground">⌘F</kbd>
        </label>
      </div>

      <dl className="grid grid-cols-2 gap-px border-b bg-border bg-clip-padding @xl/trace:grid-cols-6">
        {stats.map(([label, value]) => (
          <div key={label} className="flex min-w-0 flex-col gap-1 bg-card px-4 py-3">
            <dt className="truncate text-xs text-muted-foreground">{label}</dt>
            <dd className={cn("text-lg leading-6 tabular-nums", label === "Errors" && value !== "0" && "text-destructive")}>{value}</dd>
          </div>
        ))}
      </dl>

      <div className="relative flex h-9 items-center border-b text-xs text-muted-foreground">
        <div className="flex h-full w-(--trace-label) shrink-0 items-center gap-2 border-e ps-4">
          <RiBarChartHorizontalLine className="size-3.5" aria-hidden />
          Steps
        </div>
        <div className="relative h-full flex-1 px-4" aria-hidden>
          <div className="relative h-full">
            {grid.map((t) => (
              <span key={t} className={cn("absolute top-1/2 -translate-y-1/2 ps-1.5 tabular-nums", t / total > 0.9 && t > 0 && "hidden")} style={{ left: `${(t / total) * 100}%` }}>
                {t === 0 ? "0s" : duration(t)}
              </span>
            ))}
          </div>
        </div>
      </div>

      <div className="relative max-h-112 overflow-y-auto overscroll-contain">
        {/* Gridlines and, while it runs, the line for now, under every row. */}
        <div className="pointer-events-none absolute inset-y-0 start-[calc(var(--trace-label)+1rem)] end-4" aria-hidden>
          {grid.map((t) => (
            <span key={t} className="absolute inset-y-0 w-px bg-border/60" style={{ left: `${(t / total) * 100}%` }} />
          ))}
          {running && <span className="absolute inset-y-0 w-px bg-foreground/60" style={{ left: `${((now - root.start) / total) * 100}%` }} />}
        </div>
        {matches(root, ctx.query) ? (
          <Row ctx={ctx} span={root} depth={0} hue={null} />
        ) : (
          <p className="px-4 py-8 text-center text-muted-foreground">Nothing in this trace matches “{query}”.</p>
        )}
      </div>

      <div className="flex flex-wrap gap-x-4 gap-y-1.5 border-t px-4 py-2.5 text-xs text-muted-foreground">
        <Legend style={{ background: "oklch(var(--trace-l-model) 0 0)" }}>Model call</Legend>
        <Legend style={{ background: "oklch(var(--trace-l-tool) 0 0)" }}>Tool call</Legend>
        <Legend className={cn("ring-1 ring-border ring-inset", HATCH)}>Waiting</Legend>
        <Legend className="bg-destructive">Failed</Legend>
      </div>
    </section>
  )
}

function Legend({ className, style, children }: { className?: string; style?: React.CSSProperties; children: React.ReactNode }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className={cn("h-2.5 w-2.5 rounded-[3px]", className)} style={style} aria-hidden />
      {children}
    </span>
  )
}
