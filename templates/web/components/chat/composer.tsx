"use client"

import * as React from "react"
import { RiAddLine, RiArrowUpLine, RiCloseLine, RiFileTextLine, RiStopFill } from "@remixicon/react"

import {
  Attachment,
  AttachmentAction,
  AttachmentActions,
  AttachmentContent,
  AttachmentDescription,
  AttachmentGroup,
  AttachmentMedia,
  AttachmentTitle,
} from "@/components/ui/attachment"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group"

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

interface ComposerProps {
  busy: boolean
  /** The saved conversation is still loading. */
  loading: boolean
  onSend: (text: string, files: File[]) => void
  onStop: () => void
  /** Controls shown beside the send button, such as the model picker. */
  children?: React.ReactNode
}

export function Composer({ busy, loading, onSend, onStop, children }: ComposerProps) {
  const [text, setText] = React.useState("")
  const [files, setFiles] = React.useState<File[]>([])
  const picker = React.useRef<HTMLInputElement>(null)
  // A message sent while the agent works joins its current run.
  const typed = text.trim().length > 0 || files.length > 0
  const canSend = typed && !loading

  // Copy the files out now: a FileList is live, and the picker is cleared right after.
  const add = (list: FileList | null) => {
    const picked = Array.from(list ?? [])
    if (picked.length > 0) setFiles((current) => [...current, ...picked])
  }

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!canSend) return
    onSend(text.trim(), files)
    setText("")
    setFiles([])
  }

  return (
    <form
      onSubmit={submit}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        event.preventDefault()
        add(event.dataTransfer.files)
      }}
    >
      <InputGroup>
        {files.length > 0 && (
          <InputGroupAddon align="block-start">
            <AttachmentGroup>
              {files.map((file, i) => (
                <Attachment key={`${file.name}-${i}`} size="sm">
                  <AttachmentMedia>
                    <RiFileTextLine />
                  </AttachmentMedia>
                  <AttachmentContent>
                    <AttachmentTitle>{file.name}</AttachmentTitle>
                    <AttachmentDescription>{formatSize(file.size)}</AttachmentDescription>
                  </AttachmentContent>
                  <AttachmentActions>
                    <AttachmentAction aria-label={`Remove ${file.name}`} onClick={() => setFiles((current) => current.filter((_, j) => j !== i))}>
                      <RiCloseLine />
                    </AttachmentAction>
                  </AttachmentActions>
                </Attachment>
              ))}
            </AttachmentGroup>
          </InputGroupAddon>
        )}
        <InputGroupTextarea
          value={text}
          autoFocus
          aria-label="Message"
          placeholder={loading ? "Loading the conversation…" : busy ? "Add to what the agent is doing…" : "Ask the agent…"}
          className="max-h-60 min-h-14 p-3"
          onChange={(event) => setText(event.target.value)}
          onPaste={(event) => {
            if (event.clipboardData.files.length > 0) {
              event.preventDefault()
              add(event.clipboardData.files)
            }
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault()
              event.currentTarget.form?.requestSubmit()
            }
          }}
        />
        <InputGroupAddon align="block-end" className="pt-1">
          <input
            ref={picker}
            type="file"
            multiple
            hidden
            onChange={(event) => {
              add(event.target.files)
              event.target.value = ""
            }}
          />
          <InputGroupButton size="icon-sm" variant="outline" className="rounded-full" aria-label="Attach files" onClick={() => picker.current?.click()}>
            <RiAddLine />
          </InputGroupButton>
          <div className="ml-auto flex items-center gap-1">
            {children}
            {busy && !typed ? (
              <InputGroupButton size="icon-sm" variant="default" className="rounded-full" aria-label="Stop" onClick={onStop}>
                <RiStopFill />
              </InputGroupButton>
            ) : (
              <InputGroupButton type="submit" size="icon-sm" variant="default" className="rounded-full" aria-label="Send" disabled={!canSend}>
                <RiArrowUpLine />
              </InputGroupButton>
            )}
          </div>
        </InputGroupAddon>
      </InputGroup>
    </form>
  )
}
