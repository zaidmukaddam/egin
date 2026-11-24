import * as React from "react"

/**
 * Runs once when the component mounts, and its cleanup once when it unmounts: for syncing with something
 * outside React (a timer, a window listener, a stream). The only place this app calls useEffect; everything
 * else derives values in render or acts in event handlers. To start over for a new id, remount with `key`.
 */
export function useMountEffect(effect: () => void | (() => void)) {
  // eslint-disable-next-line react-hooks/exhaustive-deps, no-restricted-syntax
  React.useEffect(effect, [])
}
