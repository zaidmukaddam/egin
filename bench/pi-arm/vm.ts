// pi's default tools (read, bash, edit, write), run inside the environment egin made for this run (a local
// microVM, or a Daytona sandbox when EGIN_EVE_TOOLBOX is set), so pi and egin work in identical task
// environments. pi itself runs on the host, as in pi's Gondolin example; only where the tools act changes, not
// the tools, their prompts or their output.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
	type BashOperations,
	createBashTool,
	createEditTool,
	createReadTool,
	createWriteTool,
	type ReadOperations,
	type WriteOperations,
} from "@earendil-works/pi-coding-agent";

const MSB = process.env.EGIN_MSB ?? `${process.env.HOME}/.microsandbox/bin/msb`;
const VM = process.env.EGIN_EVE_VM ?? "";
const ROOT = process.env.EGIN_EVE_WORKDIR ?? "/app";
const TOOLBOX = process.env.EGIN_EVE_TOOLBOX;
const DAYTONA_KEY = process.env.DAYTONA_API_KEY ?? "";

const quote = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;

/** Run a shell script in the Daytona sandbox to completion (as bench/eve-arm does): the toolbox returns
 * stdout and then stderr, at the end. */
async function daytona(script: string, cwd: string, input?: Buffer): Promise<{ code: number; out: Buffer }> {
	const auth = { Authorization: `Bearer ${DAYTONA_KEY}` };
	if (input) {
		const f = `/tmp/.pi-stdin-${randomUUID()}`;
		const form = new FormData();
		form.append("file", new Blob([input]), "f");
		const up = await fetch(`${TOOLBOX}/files/upload?path=${encodeURIComponent(f)}`, { method: "POST", headers: auth, body: form });
		if (!up.ok) return { code: 1, out: Buffer.from(`daytona upload: ${up.status} ${await up.text()}`) };
		script = `( ${script} ) < ${f}; c=$?; rm -f ${f}; exit $c`;
	}
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
	if (!r.ok) return { code: 1, out: Buffer.from(`daytona exec: ${r.status} ${JSON.stringify(p)}`) };
	while (p.state !== "terminal") {
		await fetch(`${TOOLBOX}/processes/${p.id}/wait?timeoutMs=60000`, { headers: auth });
		p = (await (await fetch(`${TOOLBOX}/processes/${p.id}`, { headers: auth })).json()) as Proc;
	}
	const logs = p.logs?.eof ? p.logs : ((await (await fetch(`${TOOLBOX}/processes/${p.id}/logs?limit=5000`, { headers: auth })).json()) as Proc["logs"]);
	const frames = logs?.frames ?? [];
	const text = (stderr: boolean) => frames.filter((f) => (f.channel === "stderr") === stderr).map((f) => f.data ?? "").join("");
	return { code: p.exitCode ?? 1, out: Buffer.from(text(false) + text(true)) };
}

/** Run argv in the VM to completion, `input` on stdin. */
function run(argv: string[], input?: Buffer): Promise<{ code: number; out: Buffer }> {
	if (TOOLBOX) return daytona(argv.map(quote).join(" "), "/", input);
	return new Promise((resolve) => {
		const child = spawn(MSB, ["exec", "-w", "/", VM, "--", ...argv], { stdio: [input ? "pipe" : "ignore", "pipe", "pipe"] });
		const chunks: Buffer[] = [];
		child.stdout.on("data", (c: Buffer) => chunks.push(c));
		child.on("close", (code) => resolve({ code: code ?? 1, out: Buffer.concat(chunks) }));
		child.on("error", () => resolve({ code: 1, out: Buffer.alloc(0) }));
		if (input) child.stdin!.end(input);
	});
}

const missing = (path: string) => Object.assign(new Error(`ENOENT: no such file or directory, '${path}'`), { code: "ENOENT" });

const readOps: ReadOperations = {
	// base64, so binary files come through the CLI intact.
	readFile: async (path) => {
		const r = await run(["sh", "-c", 'test -f "$1" || exit 3; base64 < "$1" | tr -d "\\n"', "sh", path]);
		if (r.code === 3) throw missing(path);
		if (r.code !== 0) throw new Error(`cannot read ${path}`);
		return Buffer.from(r.out.toString().trim(), "base64");
	},
	access: async (path) => {
		if ((await run(["test", "-e", path])).code !== 0) throw missing(path);
	},
	detectImageMimeType: async (path) =>
		({ ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp" })[
			path.slice(path.lastIndexOf(".")).toLowerCase()
		] ?? null,
};

const writeOps: WriteOperations = {
	writeFile: async (path, content) => {
		const r = await run(["sh", "-c", 'mkdir -p "$(dirname "$1")" && cat > "$1"', "sh", path], Buffer.from(content));
		if (r.code !== 0) throw new Error(`cannot write ${path}`);
	},
	mkdir: async (dir) => {
		if ((await run(["mkdir", "-p", dir])).code !== 0) throw new Error(`cannot create ${dir}`);
	},
};

const bashOps: BashOperations = {
	exec: (command, cwd, { onData, signal, timeout, env }) =>
		new Promise((resolve, reject) => {
			// Only pi's own session variables go in: the host's environment (PATH, HOME) is not the VM's.
			const piEnv = Object.entries(env ?? {}).filter(([k, v]) => k.startsWith("PI_") && typeof v === "string") as [string, string][];
			if (TOOLBOX) {
				// The toolbox gives output only at the end and cannot stop a command once sent; a timeout or abort
				// returns to pi and leaves the command to egin, which deletes the sandbox when the run ends.
				let settled = false;
				const settle = (f: () => void) => !settled && ((settled = true), clearTimeout(timer), f());
				const timer = setTimeout(() => settle(() => reject(new Error(`timeout:${timeout}`))), timeout && timeout > 0 ? timeout * 1000 : 2 ** 31 - 1);
				signal?.addEventListener("abort", () => settle(() => reject(new Error("aborted"))), { once: true });
				const script = piEnv.map(([k, v]) => `export ${k}=${quote(v)}; `).join("") + `bash -lc ${quote(command)}`;
				daytona(script, cwd).then(
					(r) => settle(() => (onData(r.out), resolve({ exitCode: r.code }))),
					(e) => settle(() => reject(e)),
				);
				return;
			}
			const envFlags = piEnv.flatMap(([k, v]) => ["-e", `${k}=${v}`]);
			const child = spawn(MSB, ["exec", "-w", cwd, ...envFlags, VM, "--", "bash", "-lc", command], { stdio: ["ignore", "pipe", "pipe"] });
			let timedOut = false;
			const timer = timeout && timeout > 0 ? setTimeout(() => ((timedOut = true), child.kill("SIGKILL")), timeout * 1000) : undefined;
			const abort = () => child.kill("SIGKILL");
			signal?.addEventListener("abort", abort, { once: true });
			child.stdout.on("data", onData);
			child.stderr.on("data", onData);
			child.on("close", (code) => {
				if (timer) clearTimeout(timer);
				signal?.removeEventListener("abort", abort);
				if (signal?.aborted) reject(new Error("aborted"));
				else if (timedOut) reject(new Error(`timeout:${timeout}`));
				else resolve({ exitCode: code ?? 1 });
			});
			child.on("error", reject);
		}),
};

export default function (pi: ExtensionAPI) {
	pi.registerTool(createReadTool(ROOT, { operations: readOps }));
	pi.registerTool(createWriteTool(ROOT, { operations: writeOps }));
	pi.registerTool(createEditTool(ROOT, { operations: { readFile: readOps.readFile, writeFile: writeOps.writeFile, access: readOps.access } }));
	pi.registerTool(createBashTool(ROOT, { operations: bashOps }));
	// pi's prompt names the host folder it was started in; the task's folder is the VM's.
	pi.on("before_agent_start", async (event) => ({
		systemPrompt: event.systemPrompt.replace(/Current working directory: .*/, `Current working directory: ${ROOT}`),
	}));
}
