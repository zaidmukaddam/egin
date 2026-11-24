// Run one task with pi, launched as Harbor launches it (`pi --print --mode json`, a custom OpenAI-compatible
// provider holding only the model id, pi's default tools and thinking level), with its tools in egin's microVM
// (vm.ts). Prints {"status", "reply"} as the last stdout line. Called by `egin bench` for the [pi] arm.
//   node run.mjs <instruction file>
// Environment: EGIN_EVE_VM or EGIN_EVE_TOOLBOX (Daytona, with DAYTONA_API_KEY), EGIN_EVE_WORKDIR,
// EGIN_EVE_MODEL, EGIN_EVE_CONTEXT, EGIN_BASE_URL, YDC_API_KEY
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const here = new URL(".", import.meta.url).pathname;
const instruction = readFileSync(process.argv[2], "utf8");

// egin deletes EGIN_EVE_PROJECT even when it kills this driver; pi's config and its cwd live there.
const project = process.env.EGIN_EVE_PROJECT || join(process.env.TMPDIR || "/tmp", `egin-pi-${process.pid}`);
const config = join(project, "agent");
mkdirSync(config, { recursive: true });
// pi caps each reply at 16,384 tokens unless told otherwise, and stops the run when a reply hits that: on
// FrontierHarness the router models spent all 16,384 reasoning before their first tool call. egin and eve send no
// cap, so pi gets the router's (131,072), at most a quarter of the window. pi's window is shrunk by the same
// amount, so a prompt plus a full reply still fits (vLLM rejects a request that does not).
const context = Number(process.env.EGIN_EVE_CONTEXT || 128000);
const maxTokens = Math.min(131072, Math.floor(context / 4));
const model = { id: process.env.EGIN_EVE_MODEL, maxTokens, contextWindow: context - maxTokens };
writeFileSync(
	join(config, "models.json"),
	JSON.stringify({ providers: { egin: { baseUrl: process.env.EGIN_BASE_URL, apiKey: "$YDC_API_KEY", api: "openai-completions", models: [model] } } }),
);

const args = ["--print", "--mode", "json", "--no-session", "--no-extensions", "--no-skills", "--no-context-files"];
args.push("--provider", "egin", "--model", model.id, "--extension", join(here, "vm.ts"));
// A task that brings its own system instructions (an eval's answer format) adds them to pi's prompt.
if (process.env.EGIN_EVE_INSTRUCTIONS) args.push("--append-system-prompt", readFileSync(process.env.EGIN_EVE_INSTRUCTIONS, "utf8"));
args.push(instruction);

const pi = spawn(join(here, "node_modules/.bin/pi"), args, {
	cwd: project,
	env: { ...process.env, PI_CODING_AGENT_DIR: config, PI_OFFLINE: "1", PI_TELEMETRY: "0" },
	stdio: ["ignore", "pipe", "pipe"],
});
let reply = "";
let error = null;
let stop = null;
let turns = 0;
let buffered = "";
let stderr = "";
// EGIN_PI_LOG: keep pi's raw event stream, for diagnosing runs.
const raw = process.env.EGIN_PI_LOG ? (await import("node:fs")).createWriteStream(process.env.EGIN_PI_LOG) : null;
pi.stderr.on("data", (c) => (stderr = (stderr + c).slice(-4000)));
pi.stdout.on("data", (c) => {
	raw?.write(c);
	buffered += c;
	let i;
	while ((i = buffered.indexOf("\n")) >= 0) {
		const line = buffered.slice(0, i);
		buffered = buffered.slice(i + 1);
		let e;
		try {
			e = JSON.parse(line);
		} catch {
			continue;
		}
		const m = e.type === "message_end" ? e.message : null;
		if (m?.role === "assistant") {
			turns++;
			stop = m.stopReason ?? null;
			const text = (m.content ?? []).filter((p) => p.type === "text").map((p) => p.text).join("");
			if (text) reply = text;
			if (m.stopReason === "error" || m.stopReason === "aborted") error = m.errorMessage ?? `model call ${m.stopReason}`;
		}
	}
});
const code = await new Promise((r) => pi.on("close", r));
raw?.end();
rmSync(project, { recursive: true, force: true });
// A run with no model turn at all did not run, whatever pi's exit code says.
if (!error && turns === 0) error = `pi made no model call (exit ${code}): ${stderr.trim().split("\n").slice(-3).join(" | ")}`;
const status = code === 0 && !error ? "completed" : "failed";
const why = error ?? (code === 0 ? null : `pi exited ${code}: ${stderr.trim().split("\n").pop()}`);
console.log(JSON.stringify({ status, reply, error: why, turns, stop, stderr: stderr.slice(-600) }));
process.exit(0);
