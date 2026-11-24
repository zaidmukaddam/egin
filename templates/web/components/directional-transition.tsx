import { ViewTransition } from "react"

/**
 * A page that moves the way the navigation went: in from the right when a link goes deeper (chat, then
 * traces, then one trace), from the left when it comes back. Links say which with `transitionTypes`.
 * A navigation that names no direction, such as the browser's back button, swaps without motion.
 * Goes around each page, not in a layout: a layout stays mounted, so it never enters or exits.
 */
export function DirectionalTransition({ children }: { children: React.ReactNode }) {
  return (
    <ViewTransition
      enter={{ "nav-forward": "nav-forward", "nav-back": "nav-back", default: "none" }}
      exit={{ "nav-forward": "nav-forward", "nav-back": "nav-back", default: "none" }}
      default="none"
    >
      {children}
    </ViewTransition>
  )
}
