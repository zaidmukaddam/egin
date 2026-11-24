// A change within one page, run as a view transition where the browser has them. The types say what
// kind of change it is, and the styles give each its motion: "panel" for something that opens or
// closes over the page, "panel resize" when it changes size, "theme" for the switch between light
// and dark.
type Start = (options: { update: () => void; types: string[] }) => { ready?: Promise<unknown> };

export function transition(types: "panel" | "panel resize" | "theme", change: () => void) {
  const start = (document as unknown as { startViewTransition?: Start }).startViewTransition;
  // No motion for a reader who asked for less: the change just happens.
  if (!start || matchMedia("(prefers-reduced-motion: reduce)").matches) return change();
  try {
    // A transition the browser skips (the tab is hidden, or another one starts) still makes the change;
    // only its motion is dropped, and that is not an error worth reporting.
    start.call(document, { update: change, types: types.split(" ") }).ready?.catch(() => {});
  } catch {
    // A browser from before typed transitions takes only a callback, and would run the page-navigation
    // fade for this: there the change happens without motion.
    change();
  }
}
