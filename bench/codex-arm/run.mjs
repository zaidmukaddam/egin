// Run one task with Codex CLI 0.164.0 (the version on the FrontierHarness v1.1 leaderboard) inside the task's
// Daytona sandbox, launched as Harbor launches it: `codex exec --json` with approvals and its sandbox off, web search
// off, tool output capped at 10,000 tokens, and a custom Responses-API provider for the router (the suite's
// settings). Prints {"status", "reply", "usage"} as the last stdout line. Called by `egin bench` for the [codex] arm.
//   node run.mjs <instruction file>
// The binary is the release's static musl build, fetched once by fetch.sh into $EGIN_HARNESS_DIR (default
// ~/harness), and uploaded into each sandbox.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { download, events, fail, put, quote, report, sh, settings, STATE, upload } from "../box-arm/lib.mjs";

const VERSION = "0.164.0";
const s = settings();
const instruction = readFileSync(process.argv[2], "utf8");
const tarball = join(process.env.EGIN_HARNESS_DIR || join(process.env.HOME, "harness"), `codex-${VERSION}`, "codex.tar.gz");

// The suite gives every model a 1M window, so Codex never compacts before the provider refuses; here Codex gets the
// model's real window, as egin, eve and pi do. Retries are raised from the suite's 1: the router's rate limit
// answers 429 under load, which Runta's per-trial provider did not.
const config = `model = ${JSON.stringify(s.model)}
model_provider = "egin"
approval_policy = "never"
sandbox_mode = "danger-full-access"
web_search = "disabled"
model_context_window = ${s.context}
tool_output_token_limit = 10000

[model_providers.egin]
name = "egin router"
base_url = ${JSON.stringify(s.router)}
env_key = ${JSON.stringify(s.keyEnv)}
wire_api = "responses"
supports_websockets = false
stream_idle_timeout_ms = 900000
stream_max_retries = 5
request_max_retries = 8
`;

try {
	await upload(tarball, `${STATE}/codex.tar.gz`);
	const setup = await sh(
		`set -e; mkdir -p ${STATE}/home; cd ${STATE}; tar -xzf codex.tar.gz; mv codex-x86_64-unknown-linux-musl codex; rm codex.tar.gz; ./codex --version`,
	);
	if (setup.code !== 0) fail(`installing codex: ${setup.out.trim().slice(-400)}`);
	await put(config, `${STATE}/home/config.toml`);
	await put(instruction, `${STATE}/instruction.md`);
} catch (e) {
	fail(`installing codex: ${e.message}`);
}

const run = await sh(
	`cd ${quote(s.workdir)} && CODEX_HOME=${STATE}/home ${STATE}/codex exec --dangerously-bypass-approvals-and-sandbox ` +
		`--skip-git-repo-check --json -C ${quote(s.workdir)} - < ${STATE}/instruction.md > ${STATE}/out.jsonl 2> ${STATE}/err.log; echo "exit $?"`,
	{ cwd: s.workdir, timeoutMs: 12 * 3600_000 },
);
const code = Number(/exit (\d+)/.exec(run.out)?.[1] ?? 1);
const evs = events(await download(`${STATE}/out.jsonl`));
const stderr = (await download(`${STATE}/err.log`)).slice(-600);

let reply = "";
let error = null;
let turns = 0;
const usage = { prompt: 0, cached: 0, completion: 0, reasoning: 0 };
for (const e of evs) {
	if (e.type === "item.completed") {
		turns++;
		if (e.item?.type === "agent_message" && e.item.text) reply = e.item.text;
	} else if (e.type === "turn.completed" && e.usage) {
		usage.prompt += e.usage.input_tokens ?? 0;
		usage.cached += e.usage.cached_input_tokens ?? 0;
		usage.completion += e.usage.output_tokens ?? 0;
		usage.reasoning += e.usage.reasoning_output_tokens ?? 0;
	} else if (e.type === "turn.failed") {
		error = e.error?.message ?? "turn failed";
	} else if (e.type === "error" && !error) {
		error = e.message ?? "codex error";
	}
}
if (!error && evs.length === 0) error = `codex printed no events (exit ${code}): ${stderr.trim().split("\n").slice(-3).join(" | ")}`;
report({ status: code === 0 && !error ? "completed" : "failed", reply, error, turns, usage, stderr });
