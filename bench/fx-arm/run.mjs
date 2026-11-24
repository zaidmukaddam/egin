// Run one task with fx 0.0.17 (Vercel Labs' coding agent, the one eve-bench runs beside eve, opencode and pi) inside
// the task's Daytona sandbox: `fx ask --json --full-access` with the instruction on stdin and one custom Chat
// Completions connection to the router, holding the model's real context window. Everything else is fx's own
// default. Prints {"status", "reply", "usage"} as the last stdout line. Called by `egin bench` for the [fx] arm.
//   node run.mjs <instruction file>
// The binary is the release's static Linux x86 build, fetched once by fetch.sh into $EGIN_HARNESS_DIR (default
// ~/harness), and uploaded into each sandbox.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { download, fail, put, quote, report, sh, settings, STATE, upload } from "../box-arm/lib.mjs";

const VERSION = "0.0.17";
const s = settings();
const instruction = readFileSync(process.argv[2], "utf8");
const tarball = join(process.env.EGIN_HARNESS_DIR || join(process.env.HOME, "harness"), `fx-${VERSION}`, "fx-linux-x86_64.tar.gz");

// The key is read from the variable at request time; in the sandbox it holds Daytona's placeholder, not the key.
// first_response_ms is raised from fx's 120s: reasoning models on the router can take longer to start a reply.
const config = JSON.stringify({
	provider: "egin",
	providers: {
		egin: {
			protocol: "openai-chat-completions",
			base_url: s.router,
			auth: { type: "bearer", env: s.keyEnv },
			timeouts: { first_response_ms: 600000 },
			model_metadata: { [s.model]: { context_window: s.context, supports_tool_use: true } },
		},
	},
	models: { egin: s.model },
	auto_upgrade: false,
});

// Daytona's egress proxy re-signs TLS with its own CA (/etc/rlp/ca.pem) and points SSL_CERT_FILE at a bundle that
// holds it. fx is a Zig build whose TLS client reads only the system bundle's fixed paths, so without the CA there it
// retries "Connection lost" until the task times out. Add the CA to the bundle fx reads (or create it from Daytona's).
const trust =
	"[ -f /etc/rlp/ca.pem ] && { b=/etc/ssl/certs/ca-certificates.crt; if [ -f $b ]; then " +
	'grep -qF "$(sed -n 2p /etc/rlp/ca.pem)" $b || cat /etc/rlp/ca.pem >> $b; ' +
	"else mkdir -p /etc/ssl/certs && cp /etc/rlp/ca-bundle.pem $b; fi; }";

try {
	await upload(tarball, `${STATE}/fx.tar.gz`);
	const setup = await sh(`set -e; ${trust}; mkdir -p ${STATE}/home/.fx; cd ${STATE}; tar -xzf fx.tar.gz fx; rm fx.tar.gz; ./fx --version`);
	if (setup.code !== 0) fail(`installing fx: ${setup.out.trim().slice(-400)}`);
	await put(config, `${STATE}/home/.fx/settings.json`);
	await put(instruction, `${STATE}/instruction.md`);
} catch (e) {
	fail(`installing fx: ${e.message}`);
}

const run = await sh(
	`cd ${quote(s.workdir)} && HOME=${STATE}/home FX_AUTO_UPGRADE=0 NO_COLOR=1 ${STATE}/fx ask --json --full-access ` +
		`< ${STATE}/instruction.md > ${STATE}/out.json 2> ${STATE}/err.log; echo "exit $?"`,
	{ cwd: s.workdir, timeoutMs: 12 * 3600_000 },
);
const code = Number(/exit (\d+)/.exec(run.out)?.[1] ?? 1);
const stderr = (await download(`${STATE}/err.log`)).slice(-600);
let result = null;
try {
	result = JSON.parse(await download(`${STATE}/out.json`));
} catch {}

const u = result?.usage ?? {};
const usage = { prompt: u.input_tokens ?? 0, cached: u.cache_read_tokens ?? 0, completion: u.output_tokens ?? 0, reasoning: 0 };
const error = !result
	? `fx printed no result (exit ${code}): ${stderr.trim().split("\n").slice(-3).join(" | ")}`
	: result.exit_code !== 0
		? `fx ask failed: ${result.error ?? result.recovery?.cause ?? "exit " + result.exit_code}${result.recovery?.message ? ` (${result.recovery.message})` : ""}`
		: null;
report({
	status: code === 0 && !error ? "completed" : "failed",
	reply: result?.final_output || result?.output || "",
	error,
	turns: result?.steps ?? 0,
	usage,
	tool_calls: result?.tool_calls?.length ?? 0,
	stderr,
});
