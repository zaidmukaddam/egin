// The Agent store against a fake `egin serve`: streamed tokens, an approval, a dropped
// connection mid-turn, and a reload that replays the stored log.
import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";
import { Agent } from "../dist/index.js";

const call = { id: "c1", function: { name: "bash", arguments: '{"command":"ls"}' } };
// The stored log, by id. Token deltas are live only (no id), as in egin.
const log = [
  { id: 1, type: "user", text: "list files" },
  { id: 2, type: "assistant", content: "Let me look.", tool_calls: [call] },
  { id: 3, type: "tool_call", call_id: "c1", tool: "bash", args: { command: "ls" } },
  { id: 4, type: "approval_request", call_id: "c1", tool: "bash", args: { command: "ls" } },
  { id: 5, type: "approval", call_id: "c1", allow: true },
  { id: 6, type: "tool_started", call_id: "c1" },
  { id: 7, type: "tool_result", call_id: "c1", tool: "bash", output: "a.txt" },
  { id: 8, type: "assistant", content: "One file: a.txt", tool_calls: null },
  { id: 9, type: "done", text: "One file: a.txt" },
];

function server({ finished = false } = {}) {
  let approved = finished ? { call_id: "c1", allow: true } : null;
  let connections = 0;
  const auth = [];
  const srv = http.createServer(async (req, res) => {
    auth.push(req.headers.authorization);
    const url = new URL(req.url, "http://x");
    const body = await new Promise((r) => { let b = ""; req.on("data", (c) => (b += c)); req.on("end", () => r(b)); });
    const json = (v) => res.end(JSON.stringify(v));
    if (req.method === "POST" && url.pathname === "/sessions") return json({ id: "s1" });
    if (req.method === "POST" && url.pathname === "/sessions/s1/messages") return json({ ok: true });
    if (req.method === "GET" && url.pathname === "/sessions/s1") return json({ id: "s1", status: "done", stats: {}, messages: [] });
    if (req.method === "POST" && url.pathname === "/sessions/s1/approvals") { approved = JSON.parse(body); return json({ ok: true }); }
    if (url.pathname === "/sessions/s1/events") {
      connections++;
      const after = Number(url.searchParams.get("after"));
      res.writeHead(200, { "content-type": "text/event-stream" });
      const send = (e) => res.write(`${e.id ? `id: ${e.id}\n` : ""}data: ${JSON.stringify(e)}\n\n`);
      for (const e of log.filter((e) => e.id > after)) {
        // Like egin, the turn does not go past the approval request until someone answers.
        while (e.id > 4 && approved === null) await new Promise((r) => setTimeout(r, 10));
        if (e.id === 2 && connections === 1) for (const t of ["Let ", "me ", "look."]) send({ type: "token", text: t });
        send(e);
        // The first connection drops right after the approval request.
        if (e.id === 4 && connections === 1) return res.destroy();
      }
      return; // stay open, like egin serve
    }
    res.statusCode = 404;
    res.end();
  });
  return new Promise((r) => srv.listen(0, () => r({ srv, url: `http://127.0.0.1:${srv.address().port}`, get approved() { return approved; }, get connections() { return connections; }, auth })));
}

const until = async (agent, pred) => {
  for (let i = 0; i < 200; i++) {
    if (pred(agent.getSnapshot())) return agent.getSnapshot();
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("timed out: " + JSON.stringify(agent.getSnapshot()));
};

test("a turn with an approval survives a dropped connection", async () => {
  const s = await server();
  let saved = null;
  const agent = new Agent({ baseUrl: s.url, headers: () => ({ authorization: "Bearer t" }), onSessionChange: (id) => (saved = id) });
  await agent.send("list files");
  assert.equal(saved, "s1");

  const waiting = await until(agent, (x) => x.approvals.length === 1);
  assert.equal(waiting.approvals[0].callId, "c1");
  await agent.approve("c1", true);
  assert.deepEqual(s.approved, { call_id: "c1", allow: true });

  const done = await until(agent, (x) => x.status === "idle" && x.messages.length === 2);
  assert.ok(s.connections >= 2, "reconnected after the drop");
  const [user, reply] = done.messages;
  assert.deepEqual(user.parts, [{ type: "text", text: "list files" }]);
  assert.equal(user.pending, undefined, "the optimistic message was replaced, not duplicated");
  assert.match(user.id, /^local-/, "and kept its id, so a UI keyed on it does not remount the row");
  assert.deepEqual(reply.parts.map((p) => (p.type === "text" ? p.text : `${p.tool}:${p.state}:${p.output}`)), [
    "Let me look.",
    "bash:done:a.txt",
    "One file: a.txt",
  ]);
  assert.ok(s.auth.every((a) => a === "Bearer t"), "every request carried the header");
  agent.stop();
  s.srv.closeAllConnections();
  s.srv.close();
});

test("a saved session survives the server being down, and only a 404 drops it", async () => {
  // Nothing listens here: every request fails to connect, as when egin serve is restarting.
  let saved = "s1";
  const down = new Agent({ baseUrl: "http://127.0.0.1:9", sessionId: "s1", onSessionChange: (id) => (saved = id) });
  down.start();
  const failed = await until(down, (x) => x.status === "idle" && x.error !== null);
  assert.deepEqual([failed.sessionId, saved], ["s1", "s1"], "the conversation is kept to resume later");
  down.stop();

  const s = await server();
  const gone = new Agent({ baseUrl: s.url, sessionId: "nope", onSessionChange: (id) => (saved = id) });
  gone.start();
  await until(gone, (x) => x.sessionId === null);
  assert.equal(saved, null, "a session the server does not know is forgotten");
  gone.stop();
  s.srv.closeAllConnections();
  s.srv.close();
});

test("a subagent's approval request is asked in the parent's chat and answered on its own session", async () => {
  const task = { id: "t1", function: { name: "task", arguments: '{"agent":"researcher","prompt":"look"}' } };
  const parent = [
    { id: 1, type: "user", text: "research it" },
    { id: 2, type: "assistant", content: "", tool_calls: [task] },
    { id: 3, type: "tool_call", call_id: "t1", tool: "task", args: {} },
    { id: 4, type: "subagent", agent: "researcher", session: "child" },
  ];
  const child = [
    { id: 1, type: "user", text: "look" },
    { id: 2, type: "tool_call", call_id: "b1", tool: "bash", args: { command: "date" } },
    { id: 3, type: "approval_request", call_id: "b1", tool: "bash", args: { command: "date" } },
  ];
  let answered = null;
  const srv = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    const body = await new Promise((r) => { let b = ""; req.on("data", (c) => (b += c)); req.on("end", () => r(b)); });
    if (url.pathname === "/sessions/child/approvals") {
      answered = JSON.parse(body);
      child.push({ id: 4, type: "approval", call_id: "b1", allow: true }, { id: 5, type: "tool_result", call_id: "b1", tool: "bash", output: "Tue" });
      return res.end('{"ok":true}');
    }
    const log = { "/sessions/s1/events": parent, "/sessions/child/events": child }[url.pathname];
    if (!log) return res.end(url.pathname === "/sessions/s1" ? '{"id":"s1","status":"running","messages":[]}' : "{}");
    res.writeHead(200, { "content-type": "text/event-stream" });
    let sent = Number(url.searchParams.get("after"));
    // Like egin, the stream stays open and sends new events as they are stored.
    const pump = () => {
      for (const e of log.filter((e) => e.id > sent)) res.write(`id: ${e.id}\ndata: ${JSON.stringify(e)}\n\n`), (sent = e.id);
    };
    pump();
    const timer = setInterval(pump, 10);
    res.on("close", () => clearInterval(timer));
  });
  await new Promise((r) => srv.listen(0, r));
  const agent = new Agent({ baseUrl: `http://127.0.0.1:${srv.address().port}`, sessionId: "s1" });
  agent.start();

  const waiting = await until(agent, (x) => x.approvals.length === 1);
  assert.deepEqual(
    { callId: waiting.approvals[0].callId, tool: waiting.approvals[0].tool, agent: waiting.approvals[0].agent, session: waiting.approvals[0].session },
    { callId: "b1", tool: "bash", agent: "researcher", session: "child" },
  );
  assert.deepEqual(waiting.messages[1].parts.map((p) => p.callId), ["t1", "b1"], "asked right under the hand-off");

  // A quick stop and start, as React's development mode does: the subagent is still followed.
  agent.stop();
  agent.start();
  const again = await until(agent, (x) => x.approvals.length === 1);
  assert.equal(again.status, "busy");
  assert.equal(again.messages.flatMap((m) => m.parts).filter((p) => p.callId === "b1").length, 1, "asked once");

  await agent.approve("b1", true);
  assert.deepEqual(answered, { call_id: "b1", allow: true }, "the answer went to the subagent's session");

  const done = await until(agent, (x) => x.approvals.length === 0 && x.messages.at(-1).parts.some((p) => p.callId === "b1" && p.state === "done"));
  assert.equal(done.messages.at(-1).parts.find((p) => p.callId === "b1").output, "Tue");
  agent.stop();
  srv.closeAllConnections();
  srv.close();
});

test("a saved session id replays the stored log", async () => {
  const s = await server({ finished: true });
  const agent = new Agent({ baseUrl: s.url, sessionId: "s1" });
  agent.start();
  const snap = await until(agent, (x) => x.messages.length === 2 && x.status === "idle");
  assert.equal(snap.messages[1].parts.length, 3);
  agent.stop();
  s.srv.closeAllConnections();
  s.srv.close();
});
