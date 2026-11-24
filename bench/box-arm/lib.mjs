// Shared by the arms whose harness runs inside the task's Daytona sandbox (codex-arm, zcode-arm, fx-arm), as Harbor runs
// them: the harness CLI, its tools and the task all in one machine. egin made the sandbox with the router key
// attached as a Daytona secret, so the sandbox holds only a placeholder and Daytona's egress proxy puts the real
// key on requests to the router host. The driver uploads the harness, writes its config, runs it to the end and
// reads back its event stream.
// Environment from `egin bench`: EGIN_EVE_TOOLBOX, DAYTONA_API_KEY, EGIN_EVE_WORKDIR, EGIN_EVE_MODEL,
// EGIN_EVE_CONTEXT, EGIN_ROUTER_URL, EGIN_ROUTER_KEY_ENV (the variable that holds the placeholder).
import { readFileSync } from "node:fs";

const TOOLBOX = process.env.EGIN_EVE_TOOLBOX;
const auth = { Authorization: `Bearer ${process.env.DAYTONA_API_KEY ?? ""}` };
export const STATE = "/tmp/egin-harness";
export const quote = (s) => `'${String(s).replace(/'/g, `'\\''`)}'`;

/** The run's settings, or an exit with the reason when the arm cannot run here. */
export function settings() {
	if (!TOOLBOX) fail("this arm runs its harness inside the task's sandbox: use --sandbox daytona");
	if (!process.env.EGIN_ROUTER_URL) fail("no EGIN_ROUTER_URL: this egin build does not run in-sandbox arms");
	return {
		workdir: process.env.EGIN_EVE_WORKDIR || "/app",
		model: process.env.EGIN_EVE_MODEL,
		context: Number(process.env.EGIN_EVE_CONTEXT || 128000),
		router: process.env.EGIN_ROUTER_URL.replace(/\/+$/, ""),
		keyEnv: process.env.EGIN_ROUTER_KEY_ENV || "YDC_API_KEY",
	};
}

/** Print the result line `egin bench` reads and stop. */
export function report(r) {
	console.log(JSON.stringify(r));
	process.exit(0);
}

export function fail(error) {
	report({ status: "failed", reply: "", error });
}

/** Run a shell script in the sandbox to completion (the toolbox returns stdout, then stderr, at the end). */
export async function sh(script, { cwd = "/", timeoutMs = 600_000 } = {}) {
	// A 429 means the toolbox refused the command before starting it (the org is busy), so sending it again
	// can't run it twice. Wait and retry for up to 15 minutes, as egin's own sandbox does.
	let r;
	for (let pause = 5000, waited = 0; ; waited += pause, pause = Math.min(pause * 2, 60_000)) {
		r = await fetch(`${TOOLBOX}/processes?waitMs=60000&logs=text`, {
			method: "POST",
			headers: { ...auth, "Content-Type": "application/json" },
			body: JSON.stringify({ kind: "exec", shellCommand: `sh -c ${quote(script)}`, cwd, timeoutMs }),
		});
		if (r.status !== 429 || waited >= 900_000) break;
		await new Promise((ok) => setTimeout(ok, pause));
	}
	let p = await r.json().catch(() => ({}));
	if (!r.ok) throw new Error(`daytona exec: ${r.status} ${JSON.stringify(p)}`);
	while (p.state !== "terminal") {
		await fetch(`${TOOLBOX}/processes/${p.id}/wait?timeoutMs=60000`, { headers: auth });
		p = await (await fetch(`${TOOLBOX}/processes/${p.id}`, { headers: auth })).json();
	}
	const logs = p.logs?.eof ? p.logs : await (await fetch(`${TOOLBOX}/processes/${p.id}/logs?limit=5000`, { headers: auth })).json();
	return { code: p.exitCode ?? 1, out: (logs?.frames ?? []).map((f) => f.data ?? "").join("") };
}

export const upload = (local, remote) => put(readFileSync(local), remote);

/** Write `data` (text or bytes) to `remote` in the sandbox. */
export async function put(data, remote) {
	const form = new FormData();
	form.append("file", new Blob([data]), "f");
	const r = await fetch(`${TOOLBOX}/files/upload?path=${encodeURIComponent(remote)}`, { method: "POST", headers: auth, body: form });
	if (!r.ok) throw new Error(`daytona upload ${remote}: ${r.status} ${await r.text()}`);
}

export async function download(remote) {
	const r = await fetch(`${TOOLBOX}/files/download?path=${encodeURIComponent(remote)}`, { headers: auth });
	return r.ok ? await r.text() : "";
}

/** The JSON objects in a JSON-lines stream, skipping anything else the CLI printed. */
export function events(text) {
	return text.split("\n").flatMap((l) => {
		try {
			const v = JSON.parse(l);
			return v && typeof v === "object" ? [v] : [];
		} catch {
			return [];
		}
	});
}
