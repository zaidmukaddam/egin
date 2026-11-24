// Run one task with ZCode 0.17.0 (the version on the FrontierHarness v1.1 leaderboard) inside the task's Daytona
// sandbox, launched as the suite's command agent launches it: `zcode --mode yolo --output-format stream-json
// --prompt=...` with one custom OpenAI-compatible provider holding only the model id. Prints {"status", "reply",
// "usage"} as the last stdout line. Called by `egin bench` for the [zcode] arm.
//   node run.mjs <instruction file>
// The CLI bundle and its Node are built once by build.sh into $EGIN_HARNESS_DIR (default ~/harness), and
// uploaded into each sandbox.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { download, events, fail, put, quote, report, sh, settings, STATE, upload } from "../box-arm/lib.mjs";

const VERSION = "0.17.0";
const s = settings();
const instruction = readFileSync(process.argv[2], "utf8");
const pack = join(process.env.EGIN_HARNESS_DIR || join(process.env.HOME, "harness"), `zcode-${VERSION}`, "zcode-pack.tgz");
const node = `${STATE}/zpack/bin/node ${STATE}/zpack/zcode.cjs`;

try {
	await upload(pack, `${STATE}/zcode-pack.tgz`);
	await put(instruction, `${STATE}/instruction.md`);
	// The config holds the key variable's value, which in the sandbox is Daytona's placeholder, not the key.
	const config = JSON.stringify({
		provider: { egin: { kind: "openai-compatible", source: "custom", options: { apiKey: "@KEY@", baseURL: s.router }, models: { [s.model]: {} } } },
		model: { main: `egin/${s.model}` },
	});
	// For a model its downloaded rules don't know, ZCode sends every vendor's reasoning field at once (`thinking`,
	// `enable_thinking`, `reasoning_effort`, `reasoning`), and the router rejects `enable_thinking` and `reasoning`
	// for the muse models with a 400. Their provider config, written before ZCode makes its own, gets a model rule
	// that sends only `reasoning_effort`, as a rule ZCode ships for a model would. Other models keep ZCode's default.
	const personal = /^muse-/.test(s.model)
		? JSON.stringify({
				schemaVersion: 1,
				config: {
					providerConfigRules: { providerRules: [{ providerId: "egin", config: { group: "standard-personal",
						access: { type: "api-key", apiKey: "@KEY@" }, api: { type: "openai-chat-completions", baseUrl: s.router },
						personalModelIds: [s.model], modelOrder: [s.model] } }] },
					modelConfigRules: {
						providerModelRules: [{ providerId: "egin", modelId: s.model, config: { optionSpecs: { reasoningLevel: {
							map: '{"reasoning_effort": reasoningLevel == "disabled" ? "none" : reasoningLevel == "enabled" ? "high" : reasoningLevel}' } } } }],
						manualProviderModelRules: [],
					},
					defaultModelSelection: { providerId: "egin", modelId: s.model },
				},
			})
		: null;
	const pc = `${STATE}/zcode/.zcode/v2/provider_config.json`;
	const setup = await sh(
		`set -e; cd ${STATE}; tar -xzf zcode-pack.tgz; rm zcode-pack.tgz; mkdir -p home/.zcode/cli zcode/.zcode/v2; ` +
			`printf '%s' ${quote(config)} | sed "s/@KEY@/$${s.keyEnv}/" > home/.zcode/cli/config.json; chmod 600 home/.zcode/cli/config.json; ` +
			(personal ? `printf '%s' ${quote(personal)} | sed "s/@KEY@/$${s.keyEnv}/" > ${pc}; chmod 600 ${pc}; ` : "") +
			`${node} --version`,
	);
	if (setup.code !== 0) fail(`installing zcode: ${setup.out.trim().slice(-400)}`);
} catch (e) {
	fail(`installing zcode: ${e.message}`);
}

const run = await sh(
	`cd ${quote(s.workdir)} && HOME=${STATE}/home ZCODE_DATA_BASE_DIR=${STATE}/zcode ${node} --mode yolo --output-format stream-json ` +
		`--prompt="$(cat ${STATE}/instruction.md)" < /dev/null > ${STATE}/out.jsonl 2> ${STATE}/err.log; echo "exit $?"`,
	{ cwd: s.workdir, timeoutMs: 12 * 3600_000 },
);
const code = Number(/exit (\d+)/.exec(run.out)?.[1] ?? 1);
const evs = events(await download(`${STATE}/out.jsonl`));
const stderr = (await download(`${STATE}/err.log`)).slice(-600);

const result = evs.findLast((e) => e.type === "result");
// ZCode's usage summary, under whichever names this version uses.
const u = result?.usage ?? {};
const n = (...keys) => keys.map((k) => u[k]).find((v) => typeof v === "number") ?? 0;
const usage = {
	prompt: n("inputTokens", "promptTokens", "input_tokens", "prompt_tokens"),
	cached: n("cachedInputTokens", "cacheReadTokens", "cacheReadInputTokens", "cached_input_tokens", "cached_tokens"),
	completion: n("outputTokens", "completionTokens", "output_tokens", "completion_tokens"),
	reasoning: n("reasoningTokens", "reasoning_tokens", "reasoningOutputTokens"),
};
// A failed turn ends the run with no result line; its event says why.
const failed = evs.findLast((e) => e.type === "turn.failed")?.payload?.error;
const error = result
	? null
	: failed
		? `zcode turn failed: ${failed.message ?? failed.code ?? "unknown"}${failed.attribution?.reason ? ` (${failed.attribution.reason})` : ""}`
		: `zcode printed no result (exit ${code}): ${stderr.trim().split("\n").slice(-3).join(" | ")}`;
report({
	status: code === 0 && !error ? "completed" : "failed",
	reply: result?.response ?? "",
	error,
	turns: result?.projection?.turnCount ?? 0,
	usage,
	raw_usage: result?.usage ?? null,
	stderr,
});
