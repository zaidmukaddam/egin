// Next.js integration, in two pieces:
//
//   // next.config.ts: `next dev` and `next start` run `egin serve` for the agent in this app
//   import { withEgin } from "egin-ts/next";
//   export default withEgin({});
//
//   // app/egin/[...path]/route.ts: the browser's /egin/* calls go to that server
//   export { GET, POST, DELETE } from "egin-ts/next";
//
// To use an agent that runs elsewhere, set EGIN_URL (and EGIN_TOKEN): withEgin then starts nothing,
// and the route handler forwards there. For per-user sessions, use createProxy from "egin-ts/proxy".
import { createProxy } from "./proxy.js";

const LOCAL = "http://127.0.0.1:8787";

const proxy = createProxy({ target: process.env.EGIN_URL ?? LOCAL, token: process.env.EGIN_TOKEN });

/** The proxy, and for a local agent, a server that is not there (the one withEgin found was a restarted
 * `next dev`'s, gone with it) is started and the request tried once more. */
async function handler(request: Request): Promise<Response> {
  if (process.env.EGIN_URL) return proxy(request);
  try {
    return await proxy(request.clone());
  } catch (e) {
    if (!String((e as { cause?: { code?: string } }).cause?.code).includes("ECONNREFUSED")) throw e;
    await ensureServer(process.env.EGIN_AGENT_DIR ?? ".");
    return proxy(request);
  }
}

export { handler as GET, handler as POST, handler as DELETE };

export interface WithEginOptions {
  /** The agent folder, or the app that holds it in agent/. Defaults to the app's own folder. */
  agent?: string;
}

type ConfigOrFn<C> = C | ((phase: string, context: unknown) => C | Promise<C>);

/** Wraps a Next.js config so `next dev` and `next start` also run `egin serve` for this app's agent. */
export function withEgin<C>(config: ConfigOrFn<C>, options: WithEginOptions = {}) {
  return async (phase: string, context: unknown): Promise<C> => {
    const resolved = typeof config === "function" ? await (config as (p: string, c: unknown) => C | Promise<C>)(phase, context) : config;
    if ((phase === "phase-development-server" || phase === "phase-production-server") && !process.env.EGIN_URL) {
      // Kept for the route handler, which starts the server itself if it ever finds it gone.
      process.env.EGIN_AGENT_DIR = options.agent ?? ".";
      await ensureServer(process.env.EGIN_AGENT_DIR);
    }
    return resolved;
  };
}

async function healthy() {
  try {
    return (await fetch(`${LOCAL}/health`, { signal: AbortSignal.timeout(1000) })).ok;
  } catch {
    return false;
  }
}

let starting: Promise<void> | null = null;

/** Starts `egin serve` on 127.0.0.1:8787 unless one is already answering there, and waits until it answers.
 * Concurrent callers share one start. */
function ensureServer(agent: string): Promise<void> {
  starting ??= start(agent).finally(() => (starting = null));
  return starting;
}

async function start(agent: string) {
  if (await healthy()) return;
  const [{ spawn }, { resolve }, { fileURLToPath }] = await Promise.all([import("node:child_process"), import("node:path"), import("node:url")]);
  // EGIN_BIN points at a binary built or installed some other way; otherwise this package's launcher.
  const [command, ...prefix] = process.env.EGIN_BIN ? [process.env.EGIN_BIN] : [process.execPath, fileURLToPath(new URL("../bin/egin.js", import.meta.url))];
  const child = spawn(command, [...prefix, "serve", resolve(process.cwd(), agent), "--port", "8787"], { stdio: "inherit" });
  // Killed outright, so a restarted `next dev` never finds the old server still answering. egin resumes
  // interrupted sessions from its store, so nothing is lost.
  process.once("exit", () => child.kill("SIGKILL"));
  for (let i = 0; i < 60 && child.exitCode === null; i++) {
    if (await healthy()) return;
    await new Promise((r) => setTimeout(r, 500));
  }
  // Another Next.js process may have started it first, in which case this one exited on the busy port.
  if (!(await healthy())) throw new Error("egin serve did not start; see its output above");
}
