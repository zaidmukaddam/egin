import * as React from "react"

import { loadHighlighter } from "@/lib/highlighter"

type Highlighter = Awaited<ReturnType<typeof loadHighlighter>>

// Highlighters by language, loaded once for the whole page and shared by every code block. A block reads its
// language's highlighter from this store, and is told when it arrives; until then it shows plain lines.
const loaded = new Map<string, Highlighter>()
const loading = new Set<string>()
const listeners = new Set<() => void>()

function load(language: string) {
  if (loaded.has(language) || loading.has(language)) return
  loading.add(language)
  loadHighlighter(language)
    .then((highlighter) => {
      loaded.set(language, highlighter)
      listeners.forEach((notify) => notify())
    })
    .catch(() => {})
    .finally(() => loading.delete(language))
}

function useHighlighter(language: string): Highlighter {
  const subscribe = React.useCallback(
    (notify: () => void) => {
      listeners.add(notify)
      load(language)
      return () => void listeners.delete(notify)
    },
    [language]
  )
  return React.useSyncExternalStore(subscribe, () => loaded.get(language) ?? null, () => null)
}

export function useHighlightedCode(value: string, language: string) {
  // A trailing newline would render as an extra empty line.
  const code = value.endsWith("\n") ? value.slice(0, -1) : value
  const highlighter = useHighlighter(language)

  // The complete lines, highlighted. While the code streams, only the new
  // lines are tokenized, starting from the grammar state of the last one.
  const [done, setDone] = React.useState<{
    highlighter: typeof highlighter
    text: string
    lines: React.ReactNode[]
    state?: Parameters<NonNullable<typeof highlighter>["highlight"]>[1]
  }>({ highlighter: null, text: "", lines: [] })

  let next = done

  if (next.highlighter !== highlighter || !code.startsWith(next.text)) {
    next = { highlighter, text: "", lines: [] }
  }

  const end = code.lastIndexOf("\n") + 1

  if (highlighter && end > next.text.length) {
    const result = highlighter.highlight(
      code.slice(next.text.length, end - 1),
      next.state
    )

    next = {
      highlighter,
      text: code.slice(0, end),
      lines: [...next.lines, ...result.lines],
      state: result.state,
    }
  }

  if (next !== done) {
    setDone(next)
  }

  // The last line is still being written, so it is tokenized on every render.
  const tail = code.slice(end)

  return React.useMemo(() => {
    if (!highlighter) {
      return code.split("\n")
    }

    return [...next.lines, highlighter.highlight(tail, next.state).lines[0]]
  }, [code, highlighter, next, tail])
}
