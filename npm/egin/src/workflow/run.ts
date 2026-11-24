// Where an agent is, and one `egin step` on it: the session goes in on stdin, token deltas come out
// as they arrive, then what the session is now. The process keeps nothing, so on a host with no
// process or disk of its own, any instance can run the next step.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { accessSync, constants, cpSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** Where an agent is and how to run it. Plain data, because it travels in a workflow run's input. */
export interface AgentRef {
  /** The agent folder, as an absolute path that is the same in every instance of a deployment. */
  dir: string;
  /** The egin binary. Default: `EGIN_BIN`, else the one for this machine in the egin package's `bin/`.
   * `{platform}` and `{arch}` in it become those of the instance that runs it ("linux", "x64" or
   * "arm64"): on Vercel the function that runs a workflow's steps is arm64 by default, whatever the
   * app's own functions are, so one path can name a binary for each. */
  bin?: string;
  /** Folders copied into the agent folder before it runs, by their path inside it, such as
   * `{"workspace/docs": "/var/task/docs"}`. */
  mounts?: Record<string, string>;
  /** Environment for egin, on top of the process's own. An empty value removes a variable. */
  env?: Record<string, string>;
}

export type Answer = { call_id: string; allow?: boolean; answer?: string };

export type StepInput = {
  id?: string;
  messages?: readonly unknown[];
  events?: readonly unknown[];
  stats?: unknown;
  message?: string;
  answers?: Answer[];
  calls?: number;
  /** How long the step may go on starting model calls. */
  seconds?: number;
  cancel?: boolean;
  info?: boolean;
};

export type StepResult = { messages: unknown[]; events: Record<string, unknown>[]; stats: unknown; status: string };

export interface Located {
  bin: string;
  dir: string;
  env: NodeJS.ProcessEnv;
}

const located = new Map<string, Located>();

function writable(dir: string) {
  try {
    accessSync(dir, constants.W_OK);
    return true;
  } catch {
    return false;
  }
}

/** The agent as this instance runs it. egin writes into the agent folder (its file index), so where
 * it can't, as in a serverless function whose only writable place is the temp folder, or when
 * folders are to be copied in, it runs on a copy made once per instance. */
export function locate(agent: AgentRef): Located {
  const key = JSON.stringify(agent);
  const known = located.get(key);
  if (known) return known;
  let dir = agent.dir;
  if (agent.mounts || !writable(dir)) {
    dir = path.join(tmpdir(), `egin-agent-${createHash("sha256").update(key).digest("hex").slice(0, 12)}`);
    rmSync(dir, { recursive: true, force: true });
    cpSync(agent.dir, dir, { recursive: true, dereference: true, filter: (src) => !src.split(path.sep).includes(".egin") });
    for (const [inside, from] of Object.entries(agent.mounts ?? {})) {
      rmSync(path.join(dir, inside), { recursive: true, force: true });
      cpSync(from, path.join(dir, inside), { recursive: true, dereference: true });
    }
  }
  const named = agent.bin?.replaceAll("{platform}", process.platform).replaceAll("{arch}", process.arch);
  // Built with join, not a literal path, so a deployment's file tracing does not pull every platform's binary in;
  // deployments name theirs in `bin`.
  const bin = named ?? process.env.EGIN_BIN ?? [path.dirname(fileURLToPath(import.meta.url)), "..", "..", "bin", `${process.platform}-${process.arch}`, "egin"].join(path.sep);
  // Middleware and tool scripts are often Node scripts, so this Node goes on PATH.
  const env: NodeJS.ProcessEnv = { ...process.env, PATH: `${path.dirname(process.execPath)}:${process.env.PATH ?? ""}` };
  for (const [name, value] of Object.entries(agent.env ?? {})) {
    if (value === "") delete env[name];
    else env[name] = value;
  }
  const here = { bin, dir, env };
  located.set(key, here);
  return here;
}

/** One `egin step`. `seen` gets each event as it happens: token deltas, which are not stored, and
 * stored events (`stored` true) ahead of the result that carries them again. Aborting `cancel` ends
 * the turn where it stands: the step still resolves, with the session as cancelled. */
export function step<T = StepResult>(agent: AgentRef, input: StepInput, seen?: (event: Record<string, unknown>, stored: boolean) => void, cancel?: AbortSignal): Promise<T> {
  const { bin, dir, env } = locate(agent);
  return new Promise((resolve, reject) => {
    const child = spawn(bin, ["step", dir], { env, stdio: ["pipe", "pipe", "pipe"] });
    let buffer = "";
    let errors = "";
    let result: T | undefined;
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
      buffer += chunk;
      for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        if (!line) continue;
        const value = JSON.parse(line) as { live?: Record<string, unknown>; event?: Record<string, unknown>; result?: T };
        if (value.live) seen?.(value.live, false);
        else if (value.event) seen?.(value.event, true);
        else if (value.result) result = value.result;
      }
    });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => (errors = (errors + chunk).slice(-2000)));
    child.on("error", reject);
    // The session is one line on stdin. A second line is a cancel, so stdin stays open for one.
    const stop = () => child.stdin.end("cancel\n");
    child.on("close", (code) => {
      cancel?.removeEventListener("abort", stop);
      if (code === 0 && result) resolve(result);
      else reject(new Error(`egin step exited with ${code}: ${errors.trim()}`));
    });
    child.stdin.on("error", () => {});
    child.stdin.write(`${JSON.stringify(input)}\n`);
    if (!cancel) child.stdin.end();
    else if (cancel.aborted) stop();
    else cancel.addEventListener("abort", stop, { once: true });
  });
}
