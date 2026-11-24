// A sandbox provider that runs eve's tools inside the environment egin made for this run, so eve and egin
// work in identical task environments. Commands go through microsandbox's CLI to a local microVM, or, when
// egin runs the benchmark on Daytona (EGIN_EVE_TOOLBOX set), to that Daytona sandbox's toolbox API.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { Readable, Writable } from "node:stream";
import { defineSandbox } from "eve/sandbox";
import { defineSandboxProvider, type SandboxConnection } from "eve/sandbox/provider";

const MSB = process.env.EGIN_MSB ?? `${process.env.HOME}/.microsandbox/bin/msb`;
const VM = process.env.EGIN_EVE_VM ?? "";
const ROOT = process.env.EGIN_EVE_WORKDIR ?? "/app";
const TOOLBOX = process.env.EGIN_EVE_TOOLBOX;
const DAYTONA_KEY = process.env.DAYTONA_API_KEY ?? "";

type Run = {
  pid?: number;
  stdin?: WritableStream<Uint8Array>;
  stdout: ReadableStream<Uint8Array>;
  stderr: ReadableStream<Uint8Array>;
  done: Promise<{ exitCode: number }>;
  kill: () => void;
};

const quote = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;

// --- microsandbox ---------------------------------------------------------------------------------

function execMsb(argv: string[], opts: { cwd: string; env?: Record<string, string>; stdin?: boolean; signal?: AbortSignal }): Run {
  const envFlags = Object.entries(opts.env ?? {}).flatMap(([k, v]) => ["-e", `${k}=${v}`]);
  const child = spawn(MSB, ["exec", "-w", opts.cwd, ...envFlags, VM, "--", ...argv], {
    stdio: [opts.stdin ? "pipe" : "ignore", "pipe", "pipe"],
    signal: opts.signal,
  });
  const done = new Promise<{ exitCode: number }>((resolve) => {
    child.on("close", (code) => resolve({ exitCode: code ?? 1 }));
    child.on("error", () => resolve({ exitCode: 1 }));
  });
  return {
    pid: child.pid,
    stdin: opts.stdin ? (Writable.toWeb(child.stdin!) as WritableStream<Uint8Array>) : undefined,
    stdout: Readable.toWeb(child.stdout!) as ReadableStream<Uint8Array>,
    stderr: Readable.toWeb(child.stderr!) as ReadableStream<Uint8Array>,
    done,
    kill: () => void child.kill("SIGKILL"),
  };
}

// --- Daytona --------------------------------------------------------------------------------------

async function daytonaUpload(path: string, data: Uint8Array) {
  const form = new FormData();
  form.append("file", new Blob([data]), "f");
  const r = await fetch(`${TOOLBOX}/files/upload?path=${encodeURIComponent(path)}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${DAYTONA_KEY}` },
    body: form,
  });
  if (!r.ok) throw new Error(`daytona upload ${path}: ${r.status} ${await r.text()}`);
}

/** Run argv to completion in the sandbox; the toolbox returns stdout and stderr together, at the end. */
async function daytonaRun(argv: string[], cwd: string, env?: Record<string, string>, input?: Uint8Array): Promise<{ code: number; out: string }> {
  let script = Object.entries(env ?? {}).map(([k, v]) => `export ${k}=${quote(v)}; `).join("") + argv.map(quote).join(" ");
  if (input !== undefined) {
    const f = `/tmp/.eve-stdin-${randomUUID()}`;
    await daytonaUpload(f, input);
    script = `( ${script} ) < ${f}; c=$?; rm -f ${f}; exit $c`;
  }
  // Daytona's v2 toolbox: start the process, wait for it to end, read its log frames.
  const auth = { Authorization: `Bearer ${DAYTONA_KEY}` };
  // A 429 means the toolbox refused the command before starting it (the org is busy), so sending it again
  // can't run it twice. Wait and retry for up to 15 minutes, as egin's own sandbox does.
  let r: Response;
  for (let pause = 5000, waited = 0; ; waited += pause, pause = Math.min(pause * 2, 60_000)) {
    r = await fetch(`${TOOLBOX}/processes?waitMs=600000&logs=text`, {
      method: "POST",
      headers: { ...auth, "Content-Type": "application/json" },
      // The run's own time limit is egin's, which kills this driver; the toolbox's is a backstop.
      body: JSON.stringify({ kind: "exec", shellCommand: `sh -c ${quote(script)}`, cwd, timeoutMs: 7_200_000 }),
    });
    if (r.status !== 429 || waited >= 900_000) break;
    await new Promise((ok) => setTimeout(ok, pause));
  }
  type Proc = { id?: string; state?: string; exitCode?: number; logs?: { eof?: boolean; frames?: { channel?: string; data?: string }[] } };
  let p = (await r.json().catch(() => ({}))) as Proc;
  if (!r.ok) return { code: 1, out: `daytona exec: ${r.status} ${JSON.stringify(p)}` };
  while (p.state !== "terminal") {
    await fetch(`${TOOLBOX}/processes/${p.id}/wait?timeoutMs=60000`, { headers: auth });
    p = (await (await fetch(`${TOOLBOX}/processes/${p.id}`, { headers: auth })).json()) as Proc;
  }
  const logs = p.logs?.eof ? p.logs : ((await (await fetch(`${TOOLBOX}/processes/${p.id}/logs?limit=5000`, { headers: auth })).json()) as Proc["logs"]);
  const frames = logs?.frames ?? [];
  const out = frames.filter((f) => f.channel !== "stderr").map((f) => f.data ?? "").join("") + frames.filter((f) => f.channel === "stderr").map((f) => f.data ?? "").join("");
  return { code: p.exitCode ?? 1, out };
}

function execDaytona(argv: string[], opts: { cwd: string; env?: Record<string, string>; stdin?: boolean }): Run {
  // stdin, when asked for, is gathered until the caller closes it, then sent along with the command.
  let gathered: (input: Uint8Array) => void = () => {};
  const input = new Promise<Uint8Array>((resolve) => (gathered = resolve));
  const chunks: Uint8Array[] = [];
  const stdin = opts.stdin
    ? new WritableStream<Uint8Array>({
        write: (c) => void chunks.push(c),
        close: () => gathered(Buffer.concat(chunks)),
      })
    : undefined;
  const result = (opts.stdin ? input : Promise.resolve(undefined)).then((i) => daytonaRun(argv, opts.cwd, opts.env, i));
  const enc = new TextEncoder();
  return {
    stdin,
    stdout: new ReadableStream({
      async start(c) {
        c.enqueue(enc.encode((await result).out));
        c.close();
      },
    }),
    stderr: new ReadableStream({ start: (c) => c.close() }),
    done: result.then((r) => ({ exitCode: r.code })),
    // A command already sent cannot be called back; egin deletes the sandbox when the run ends.
    kill: () => {},
  };
}

const exec = (argv: string[], opts: { cwd: string; env?: Record<string, string>; stdin?: boolean; signal?: AbortSignal }) =>
  TOOLBOX ? execDaytona(argv, opts) : execMsb(argv, opts);

async function collect(argv: string[], input?: Uint8Array): Promise<{ code: number; out: Buffer }> {
  if (TOOLBOX) {
    const r = await daytonaRun(argv, "/", undefined, input);
    return { code: r.code, out: Buffer.from(r.out) };
  }
  const run = execMsb(argv, { cwd: "/", stdin: input !== undefined });
  const chunks: Buffer[] = [];
  const reader = run.stdout.getReader();
  const read = (async () => {
    for (let r = await reader.read(); !r.done; r = await reader.read()) chunks.push(Buffer.from(r.value));
  })();
  if (input !== undefined) {
    const w = run.stdin!.getWriter();
    await w.write(input);
    await w.close();
  }
  const { exitCode } = await run.done;
  await read;
  return { code: exitCode, out: Buffer.concat(chunks) };
}

function connect(): SandboxConnection {
  return {
    workspaceRoot: ROOT,
    async spawn({ command, env, workingDirectory, stdin, signal }) {
      const run = exec(["bash", "-lc", command], { cwd: workingDirectory, env, stdin, signal });
      return { pid: run.pid, stdin: run.stdin, stdout: run.stdout, stderr: run.stderr, wait: () => run.done, kill: async () => run.kill() };
    },
    async readFile({ path }) {
      // base64, so a binary file survives the toolbox's text output.
      const r = await collect(["sh", "-c", 'test -e "$1" || exit 3; test -d "$1" && exit 4; base64 < "$1" | tr -d "\\n"', "sh", path]);
      if (r.code === 3) return null;
      if (r.code !== 0) throw new Error(`cannot read ${path}`);
      return new Uint8Array(Buffer.from(r.out.toString().trim(), "base64"));
    },
    async writeFiles({ files }) {
      for (const f of files) {
        const r = await collect(["sh", "-c", 'mkdir -p "$(dirname "$1")" && cat > "$1"', "sh", f.path], f.content);
        if (r.code !== 0) throw new Error(`cannot write ${f.path}`);
      }
    },
    async removePath({ path, force, recursive }) {
      const r = await collect(["rm", ...(force ? ["-f"] : []), ...(recursive ? ["-r"] : []), "--", path]);
      if (r.code !== 0 && !force) throw new Error(`cannot remove ${path}`);
    },
    // The environment belongs to egin, which stops and removes it after grading.
    async stop() {},
  };
}

const EginVm = defineSandboxProvider({
  name: "egin-microvm",
  environment() {
    return {
      async prepare() {
        return {};
      },
      async start() {
        return { connection: connect(), state: { vm: VM } };
      },
      async resume() {
        return connect();
      },
      async onSessionEnd() {},
    };
  },
});

export const environment = EginVm.environment();

export default defineSandbox(() => environment.open());
