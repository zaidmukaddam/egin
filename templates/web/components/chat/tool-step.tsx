"use client"

import * as React from "react"
import type { Part } from "egin-ts/react"
import { RiFileTextLine, RiGlobalLine, RiQuestionLine, RiSearchLine, RiRobot2Line, RiTerminalBoxLine, RiToolsLine, type RemixiconComponentType } from "@remixicon/react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { CodeBlock, CodeBlockContent } from "@/components/ui/code-block"
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "@/components/ui/item"
import {
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceCollapsibleTrigger,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace"

export type ToolPart = Extract<Part, { type: "tool" }>

const MAIN = ["command", "query", "url", "path", "pattern", "agent", "prompt"]

/** What a call does, in one line: its command, query, URL or path, otherwise its first string argument. */
export function describeArgs(args: unknown): string | undefined {
  if (!args || typeof args !== "object") return undefined
  const a = args as Record<string, unknown>
  const v = MAIN.map((k) => a[k]).find((x) => typeof x === "string") ?? Object.values(a).find((x) => typeof x === "string")
  return typeof v === "string" ? v : undefined
}

function describe(part: ToolPart) {
  return describeArgs(part.args) ?? part.tool
}

interface Kind {
  icon: RemixiconComponentType
  running: string
  done: string
  /** The argument is code or a path, so it is set in mono. */
  mono?: boolean
  /** The verb an approval asks with, when it isn't "Run". */
  ask?: string
}

const BASH: Kind = { icon: RiTerminalBoxLine, running: "Running", done: "Ran", mono: true }
const KINDS: Record<string, Kind> = {
  bash: BASH,
  web_search: { icon: RiSearchLine, running: "Searching", done: "Searched" },
  web_fetch: { icon: RiGlobalLine, running: "Reading", done: "Read", mono: true },
  web_extract: { icon: RiGlobalLine, running: "Reading", done: "Read", mono: true },
  read_file: { icon: RiFileTextLine, running: "Reading", done: "Read", mono: true },
  write_file: { icon: RiFileTextLine, running: "Writing", done: "Wrote", mono: true, ask: "Write" },
  edit_file: { icon: RiFileTextLine, running: "Editing", done: "Edited", mono: true, ask: "Edit" },
  search_files: { icon: RiSearchLine, running: "Searching files for", done: "Searched files for", mono: true },
  ask_user: { icon: RiQuestionLine, running: "Asking", done: "Asked", mono: false },
  task: { icon: RiRobot2Line, running: "Handing off to", done: "Handed off to", mono: false },
}

/** How a call reads in the trace; provider-prefixed names such as you_web_search match their plain tool. */
export function kind(tool: string): Kind {
  const name = Object.keys(KINDS).find((k) => tool === k || tool.endsWith(`_${k}`))
  return name ? KINDS[name] : { icon: RiToolsLine, running: `Using ${tool}:`, done: `Used ${tool}:`, mono: true }
}

interface ToolStepProps {
  part: ToolPart
  onAnswer: (allow: boolean) => void
}

/** A call waiting for approval: a question, not activity, so it sits outside the trace (shadcn's Confirmation). */
export function Confirmation({ part, onAnswer }: ToolStepProps) {
  const { icon: Icon, ask = "Run" } = kind(part.tool)
  const who = part.agent ? `The ${part.agent} subagent` : "The agent"
  const handoff = part.tool === "task"
  const prompt = handoff && part.args && typeof part.args === "object" ? (part.args as Record<string, unknown>).prompt : undefined
  return (
    <Item variant="outline" size="sm">
      <ItemMedia variant="icon">
        <Icon />
      </ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="w-full truncate">{handoff ? `Hand off to ${describe(part)}?` : `${ask} ${describe(part)}?`}</ItemTitle>
        <ItemDescription className="line-clamp-2">
          {handoff && typeof prompt === "string" ? prompt : `${who} wants to use ${part.tool} in its sandbox.`}
        </ItemDescription>
      </ItemContent>
      <ItemActions>
        <Button size="sm" variant="outline" onClick={() => onAnswer(false)}>
          Deny
        </Button>
        <Button size="sm" onClick={() => onAnswer(true)}>
          Approve
        </Button>
      </ItemActions>
    </Item>
  )
}

/** An `ask_user` question: the agent waits until it is answered, with one of its choices or in words. */
export function Question({ part, onAnswer }: { part: ToolPart; onAnswer: (text: string) => void }) {
  const [text, setText] = React.useState("")
  const q = part.question
  if (!q) return null
  return (
    <Item variant="outline" size="sm" className="flex-col items-stretch gap-3">
      <div className="flex items-start gap-3">
        <ItemMedia variant="icon">
          <RiQuestionLine />
        </ItemMedia>
        <ItemContent className="min-w-0">
          <ItemTitle className="text-pretty">{q.text}</ItemTitle>
          <ItemDescription>The agent is waiting for your answer.</ItemDescription>
        </ItemContent>
      </div>
      {q.options.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {q.options.map((option) => (
            <Button key={option} size="sm" variant="outline" onClick={() => onAnswer(option)}>
              {option}
            </Button>
          ))}
        </div>
      )}
      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault()
          if (text.trim()) onAnswer(text.trim())
        }}
      >
        <Input value={text} onChange={(event) => setText(event.target.value)} aria-label="Your answer" placeholder={q.options.length > 0 ? "Or answer in your own words" : "Your answer"} />
        <Button type="submit" size="sm" disabled={!text.trim()}>
          Answer
        </Button>
      </form>
    </Item>
  )
}

/** One tool call as a line in the agent's trace (shadcn's Tool): running, denied, or done with its output. */
export function ToolStep({ part }: { part: ToolPart }) {
  const what = describe(part)
  const k = kind(part.tool)
  const line = (title: string, status?: "running" | "pending") => (
    <TraceLine status={status}>
      <TraceIcon>
        <k.icon />
      </TraceIcon>
      <TraceTitle className="truncate">
        {title} <span className={k.mono ? "font-mono" : undefined}>{what}</span>
      </TraceTitle>
    </TraceLine>
  )

  if (part.state === "denied") return line("Denied")
  if (part.state !== "done") return line(k.running, part.state === "pending" ? "pending" : "running")

  return (
    <TraceCollapsible>
      <TraceCollapsibleTrigger>{line(k.done)}</TraceCollapsibleTrigger>
      <TraceCollapsibleContent>
        <CodeBlock size="sm">
          <CodeBlockContent wrap className="max-h-64">
            {part.output || "(no output)"}
          </CodeBlockContent>
        </CodeBlock>
      </TraceCollapsibleContent>
    </TraceCollapsible>
  )
}
