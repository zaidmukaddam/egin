// Run one task with eve: start the eve server, send the instruction, wait for the turn to settle,
// print {"reply", "usage"} as the last stdout line. Called by `egin bench` and `egin eval` for the [eve] arm.
//   node run.mjs <instruction file>
// Environment: EGIN_EVE_VM, EGIN_EVE_WORKDIR, EGIN_EVE_MODEL, EGIN_EVE_PRICE, EGIN_EVE_MAX_COST, YDC_API_KEY
import { spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client } from "eve/client";
import { emptySessionView, foldEvents, openInteractions, usage } from "eve/events";

const here = new URL(".", import.meta.url).pathname;
const instruction = readFileSync(process.argv[2], "utf8");

const port = await new Promise((resolve) => {
  const s = createServer().listen(0, "127.0.0.1", () => {
    const p = s.address().port;
    s.close(() => resolve(p));
  });
});

// eve allows one dev server per project folder, so each run gets its own copy (node_modules shared by symlink),
// at EGIN_EVE_PROJECT when egin names it (egin deletes it even if this process is killed).
const project = process.env.EGIN_EVE_PROJECT || mkdtempSync(join(tmpdir(), "egin-eve-"));
mkdirSync(project, { recursive: true });
for (const f of ["agent", "package.json", "package-lock.json"]) cpSync(join(here, f), join(project, f), { recursive: true });
// A task that brings its own system instructions (an eval's answer format) replaces eve's default ones.
if (process.env.EGIN_EVE_INSTRUCTIONS) cpSync(process.env.EGIN_EVE_INSTRUCTIONS, join(project, "agent", "instructions.md"));
symlinkSync(join(here, "node_modules"), join(project, "node_modules"));

const server = spawn(`${here}node_modules/.bin/eve`, ["dev", "--no-ui", "--no-default-extensions", "--logs", "none", "--host", "127.0.0.1", "--port", String(port)], {
  cwd: project,
  env: { ...process.env, EVE_TELEMETRY_DISABLED: "1", EGIN_EVE_WRAP_UP_FILE: join(project, ".egin-wrap-up") },
  stdio: ["ignore", "pipe", "pipe"],
});
let serverLog = "";
server.stdout.on("data", (c) => (serverLog += c));
server.stderr.on("data", (c) => (serverLog += c));
const stop = () => {
  server.kill("SIGTERM");
  rmSync(project, { recursive: true, force: true });
};
process.on("exit", stop);

const client = new Client({ baseUrl: `http://127.0.0.1:${port}` });
for (let i = 0; ; i++) {
  try {
    if ((await client.health()).status === "ready") break;
  } catch {}
  if (i > 240) {
    console.error(serverLog.slice(-4000));
    throw new Error("eve server did not become ready");
  }
  await new Promise((r) => setTimeout(r, 500));
}

const { session, delivery } = await client.sessions.create({ message: instruction });
let result = await delivery.result();
// The only pause in these runs is eve's budget prompt at the spend cap (`kind: "budget"`, options
// `approve` and `deny`, possibly passed up from a subagent). egin answers once more there, tools off,
// from what it found; give eve the same turn: switch its tools off (the marker its approval policy
// reads), queue egin's wrap-up note, approve the prompt, and read the reply of the resumed turn.
// EGIN_EVE_WRAP_UP=0 keeps eve's own behaviour (no answer).
let wrappedUp = false;
const paused = result.status === "awaiting-input" ? result.interactions ?? [] : [];
const budget = paused.filter((i) => i.request?.kind === "budget");
if (budget.length > 0 && process.env.EGIN_EVE_WRAP_UP !== "0") {
  const note =
    "You have reached this task's limit (spend limit reached). Do not call any more tools. From what you have found so far, give your best final answer now, in the exact format the task asks for.";
  writeFileSync(join(project, ".egin-wrap-up"), "");
  const queued = await session.send(note);
  await session.respond(budget.map((i) => ({ interactionId: i.interactionId, optionId: "approve" })));
  const resumed = await Promise.race([queued.result(), new Promise((r) => setTimeout(() => r(null), 600_000))]);
  if (resumed) {
    result = resumed;
    wrappedUp = true;
  }
}
let total = null;
// A run that ends "awaiting-input" stopped for a person: which kind of request (a budget prompt at the
// spend cap, an approval, a question) says why it gave no answer.
let waiting = [];
try {
  // Replay the recorded events (no follow) into a view and read eve's own usage totals.
  let view = emptySessionView();
  for await (const event of session.stream({ follow: false })) view = foldEvents(view, [event]);
  total = usage(view).total;
  waiting = openInteractions(view).map((i) => i.request.kind);
} catch (e) {
  console.error(`usage unavailable: ${e}`);
}
console.log(JSON.stringify({ status: result.status, reply: result.reply ?? "", error: result.error ?? null, usage: total, waiting, wrapped_up: wrappedUp, paused }));
stop();
process.exit(0);
