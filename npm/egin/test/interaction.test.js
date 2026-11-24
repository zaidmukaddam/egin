// A question from the agent, answered in the chat, and a message sent while the turn still runs.
import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";
import { Agent } from "../dist/index.js";

const ask = { id: "q1", function: { name: "ask_user", arguments: '{"question":"Which environment?","options":["staging","production"]}' } };
const log = [
  { id: 1, type: "user", text: "deploy" },
  { id: 2, type: "assistant", content: "", tool_calls: [ask] },
  { id: 3, type: "tool_call", call_id: "q1", tool: "ask_user", args: {} },
  { id: 4, type: "question", call_id: "q1", question: "Which environment?", options: ["staging", "production"] },
  { id: 5, type: "answer", call_id: "q1", answer: "staging" },
  { id: 6, type: "tool_result", call_id: "q1", tool: "ask_user", output: "staging" },
  { id: 7, type: "queued", text: "and tell the team" },
  { id: 8, type: "assistant", content: "Deploying to staging.", tool_calls: null },
  { id: 9, type: "user", text: "and tell the team" },
  { id: 10, type: "assistant", content: "Deployed, and the team knows.", tool_calls: null },
  { id: 11, type: "done", text: "Deployed, and the team knows." },
];

function server() {
  const state = { answered: null, sent: [] };
  const srv = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    const body = await new Promise((r) => { let b = ""; req.on("data", (c) => (b += c)); req.on("end", () => r(b)); });
    const json = (v) => res.end(JSON.stringify(v));
    if (req.method === "POST" && url.pathname === "/sessions") {
      // A session's first message arrives with its creation, in one request.
      state.sent.push(JSON.parse(body).message);
      return json({ id: "s1" });
    }
    if (req.method === "POST" && url.pathname === "/sessions/s1/messages") {
      state.sent.push(JSON.parse(body).message);
      return json(state.sent.length > 1 ? { ok: true, queued: true } : { ok: true });
    }
    if (req.method === "POST" && url.pathname === "/sessions/s1/answers") { state.answered = JSON.parse(body); return json({ ok: true }); }
    if (url.pathname === "/sessions/s1/events") {
      res.writeHead(200, { "content-type": "text/event-stream" });
      for (const e of log.filter((e) => e.id > Number(url.searchParams.get("after")))) {
        // The run waits for the answer, and the queued message only exists once it is sent.
        while ((e.id >= 5 && !state.answered) || (e.id >= 7 && state.sent.length < 2)) await new Promise((r) => setTimeout(r, 10));
        res.write(`id: ${e.id}\ndata: ${JSON.stringify(e)}\n\n`);
      }
      return;
    }
    res.statusCode = 404;
    res.end();
  });
  return new Promise((r) => srv.listen(0, () => r({ srv, url: `http://127.0.0.1:${srv.address().port}`, state })));
}

const until = async (agent, pred) => {
  for (let i = 0; i < 300; i++) {
    if (pred(agent.getSnapshot())) return agent.getSnapshot();
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("timed out: " + JSON.stringify(agent.getSnapshot()));
};

test("a long chat opens at its last turn, loads earlier ones on demand, and shows tool data", async () => {
  const turns = [
    [{ id: 1, type: "user", text: "first" }, { id: 2, type: "assistant", content: "one", tool_calls: null }, { id: 3, type: "done", text: "one" }],
    [
      { id: 4, type: "user", text: "second" },
      { id: 5, type: "assistant", content: "", tool_calls: [{ id: "c1", function: { name: "chart", arguments: "{}" } }] },
      { id: 6, type: "data", call_id: "c1", tool: "chart", data: { kind: "bar", values: [1, 2] } },
      { id: 7, type: "tool_result", call_id: "c1", tool: "chart", output: "drawn" },
      { id: 8, type: "assistant", content: "two", tool_calls: null },
      { id: 9, type: "done", text: "two" },
    ],
  ];
  const asked = [];
  const srv = http.createServer((req, res) => {
    const url = new URL(req.url, "http://x");
    const json = (v) => res.end(JSON.stringify(v));
    if (url.pathname === "/sessions/s1") return json({ id: "s1", status: "done", stats: {}, messages: [] });
    if (url.pathname === "/sessions/s1/history") {
      asked.push(url.searchParams.toString());
      return url.searchParams.get("before") ? json({ events: turns[0], earlier: null }) : json({ events: turns[1], earlier: 4 });
    }
    if (url.pathname === "/sessions/s1/events") {
      assert.equal(url.searchParams.get("after"), "9", "the stream follows on from the loaded page");
      res.writeHead(200, { "content-type": "text/event-stream" });
      return;
    }
    res.statusCode = 404;
    res.end();
  });
  await new Promise((r) => srv.listen(0, r));
  const agent = new Agent({ baseUrl: `http://127.0.0.1:${srv.address().port}`, sessionId: "s1", historyLimit: 6 });
  agent.start();

  const opened = await until(agent, (x) => x.messages.length === 2);
  assert.equal(opened.hasEarlier, true);
  assert.deepEqual(opened.messages[0].parts, [{ type: "text", text: "second" }]);
  assert.deepEqual(opened.messages[1].parts.find((p) => p.type === "data"), { type: "data", tool: "chart", callId: "c1", data: { kind: "bar", values: [1, 2] } });

  await agent.loadEarlier();
  const all = agent.getSnapshot();
  assert.deepEqual(all.messages.map((m) => m.parts.find((p) => p.type === "text")?.text), ["first", "one", "second", "two"]);
  assert.equal(all.hasEarlier, false);
  assert.deepEqual(asked, ["limit=6", "before=4&limit=6"]);
  agent.stop();
  srv.closeAllConnections();
  srv.close();
});

test("a question is answered in the chat, and a message sent mid-turn waits below it", async () => {
  const s = await server();
  const agent = new Agent({ baseUrl: s.url });
  await agent.send("deploy");

  const asked = await until(agent, (x) => x.questions.length === 1);
  assert.deepEqual(asked.questions[0].question, { text: "Which environment?", options: ["staging", "production"] });
  assert.equal(asked.status, "busy");

  // Sent while the turn runs: shown as pending, under the turn that is still being written.
  await agent.send("and tell the team");
  const mid = agent.getSnapshot();
  assert.deepEqual(mid.messages.map((m) => [m.role, !!m.pending]), [["user", false], ["assistant", false], ["user", true]]);

  await agent.answer("q1", "staging");
  assert.deepEqual(s.state.answered, { call_id: "q1", answer: "staging" });
  assert.deepEqual(s.state.sent, ["deploy", "and tell the team"], "the first with the session, the second on its own");

  const done = await until(agent, (x) => x.status === "idle");
  assert.deepEqual(done.messages.map((m) => m.role), ["user", "assistant", "user", "assistant"]);
  const first = done.messages[1].parts;
  assert.equal(first.find((p) => p.type === "tool").state, "done", "the answered question finished in its own turn");
  assert.equal(first.at(-1).text, "Deploying to staging.", "the turn's text stayed above the queued message");
  assert.equal(done.messages[2].pending, undefined);
  assert.equal(done.questions.length, 0);
  agent.stop();
  s.srv.closeAllConnections();
  s.srv.close();
});
