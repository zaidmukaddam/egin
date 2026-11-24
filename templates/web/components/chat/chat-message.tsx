import type { Message as AgentMessage } from "egin-ts/react"
import { RiFileTextLine } from "@remixicon/react"

import { Markdown } from "@/components/markdown"
import { Attachment, AttachmentContent, AttachmentMedia, AttachmentTitle } from "@/components/ui/attachment"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { Message, MessageContent } from "@/components/ui/message"
import { Trace, TraceLine, TraceTitle } from "@/components/ui/trace"
import { Confirmation, Question, ToolStep, type ToolPart } from "./tool-step"

interface ChatMessageProps {
  message: AgentMessage
  /** This is the latest turn and the agent is still working on it. */
  working: boolean
  onApprove: (callId: string, allow: boolean) => void
  onAnswer: (callId: string, text: string) => void
}

export function ChatMessage({ message, working, onApprove, onAnswer }: ChatMessageProps) {
  if (message.role === "user") {
    const first = message.parts[0]
    return (
      <Message align="end" className={message.pending ? "opacity-60" : undefined} aria-label={message.queued ? "Queued for the running turn" : undefined}>
        <MessageContent>
          {message.files?.map((file) => (
            <Attachment key={file.path || file.name} size="sm">
              <AttachmentMedia>
                <RiFileTextLine />
              </AttachmentMedia>
              <AttachmentContent>
                <AttachmentTitle>{file.name}</AttachmentTitle>
              </AttachmentContent>
            </Attachment>
          ))}
          {first?.type === "text" && first.text && (
            <Bubble align="end" variant="muted">
              <BubbleContent className="whitespace-pre-wrap">{first.text}</BubbleContent>
            </Bubble>
          )}
        </MessageContent>
      </Message>
    )
  }

  // Consecutive tool calls share one trace. Text, and a call waiting on an approval or an answer, each stand on their own.
  const groups: (string | ToolPart | ToolPart[])[] = []
  for (const part of message.parts) {
    const last = groups.at(-1)
    if (part.type === "text") groups.push(part.text)
    // Tool data is for UIs built on egin's client (charts, cards); the template shows the tool step instead.
    else if (part.type === "data") continue
    else if (part.state === "approval" || part.state === "question") groups.push(part)
    else if (Array.isArray(last)) last.push(part)
    else groups.push([part])
  }
  const tail = message.parts.at(-1)
  const between = working && (!tail || (tail.type === "tool" && (tail.state === "done" || tail.state === "denied")))

  return (
    <Message>
      <MessageContent className="gap-3">
        {groups.map((group, i) =>
          typeof group === "string" ? (
            <Markdown key={i} streaming={working && i === groups.length - 1}>
              {group}
            </Markdown>
          ) : Array.isArray(group) ? (
            <Trace key={i}>
              {group.map((part) => (
                <ToolStep key={part.callId} part={part} />
              ))}
            </Trace>
          ) : group.state === "question" ? (
            <Question key={group.callId} part={group} onAnswer={(text) => onAnswer(group.callId, text)} />
          ) : (
            <Confirmation key={group.callId} part={group} onAnswer={(allow) => onApprove(group.callId, allow)} />
          ),
        )}
        {between && <Thinking />}
      </MessageContent>
    </Message>
  )
}

export function Thinking() {
  return (
    <TraceLine status="running">
      <TraceTitle>Thinking</TraceTitle>
    </TraceLine>
  )
}
