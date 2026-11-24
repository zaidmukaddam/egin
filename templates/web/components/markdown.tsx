"use client"

import * as React from "react"
import { Streamdown, defaultRehypePlugins, useIsCodeFenceIncomplete, type ExtraProps, type StreamdownProps } from "streamdown"

import { useHighlightedCode } from "@/hooks/use-highlighted-code"
import {
  CodeBlock,
  CodeBlockActions,
  CodeBlockContent,
  CodeBlockCopyButton,
  CodeBlockHeader,
  CodeBlockLine,
  CodeBlockTitle,
} from "@/components/ui/code-block"

function MarkdownCodeBlock({ code, language }: { code: string; language: string }) {
  const incomplete = useIsCodeFenceIncomplete()
  const lines = useHighlightedCode(code, language)

  return (
    <CodeBlock>
      <CodeBlockHeader>
        <CodeBlockTitle>{language || "text"}</CodeBlockTitle>
        <CodeBlockActions>
          <CodeBlockCopyButton value={code} disabled={incomplete} />
        </CodeBlockActions>
      </CodeBlockHeader>
      <CodeBlockContent>
        {lines.map((line, index) => (
          <CodeBlockLine key={index}>{line}</CodeBlockLine>
        ))}
      </CodeBlockContent>
    </CodeBlock>
  )
}

function MarkdownCode({ node, className, children, ...props }: React.ComponentProps<"code"> & ExtraProps) {
  // Streamdown marks fenced code with data-block. Everything else is inline.
  if (!("data-block" in props)) {
    return (
      <code className="rounded-sm bg-muted px-1 py-0.5 font-mono text-[0.9em]" {...props}>
        {children}
      </code>
    )
  }

  return <MarkdownCodeBlock code={typeof children === "string" ? children : ""} language={/language-(\S+)/.exec(className ?? "")?.[1] ?? ""} />
}

const components = { code: MarkdownCode }

// Streamdown's link sanitizer, with one change. A link to a file the agent wrote ("notes.md") has no scheme and
// no leading "/", so the sanitizer can't place it and prints "[blocked]" in its place. It shows the link's text.
const { harden, ...otherRehypePlugins } = defaultRehypePlugins
const [hardenPlugin, hardenOptions] = harden as [unknown, Record<string, unknown>]
const rehypePlugins = [
  ...Object.values(otherRehypePlugins),
  [hardenPlugin, { ...hardenOptions, linkBlockPolicy: "text-only" }],
] as NonNullable<StreamdownProps["rehypePlugins"]>

/** An agent reply: Markdown through Streamdown, fenced code in a Code Block (shadcn's recipe). */
export function Markdown({ children, streaming = false }: { children: string; streaming?: boolean }) {
  return (
    <Streamdown controls={false} components={components} rehypePlugins={rehypePlugins} isAnimating={streaming}>
      {children}
    </Streamdown>
  )
}
