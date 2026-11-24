// createProxy against a fake upstream: what it forwards, what it refuses, and what the browser cannot forge.
import assert from "node:assert/strict";
import { test } from "node:test";
import { createProxy } from "../dist/proxy.js";

function upstream() {
  const seen = [];
  const fetch = async (url, init) => {
    seen.push({ url, method: init.method, headers: Object.fromEntries(init.headers), body: init.body ? await new Response(init.body).text() : null });
    return new Response("data: {}\n\n", { status: 200, headers: { "content-type": "text/event-stream", "set-cookie": "leak=1" } });
  };
  return { seen, fetch };
}

test("forwards session routes with the token and the signed-in user", async () => {
  const up = upstream();
  const proxy = createProxy({ target: "http://egin:8787/", token: "secret", prefix: "/api/egin", user: () => "u1", fetch: up.fetch });
  const res = await proxy(
    new Request("https://app.example/api/egin/sessions/s1/messages?x=1", {
      method: "POST",
      headers: { "content-type": "application/json", cookie: "session=abc", authorization: "Bearer browser", "x-egin-user": "someone-else" },
      body: '{"message":"hi"}',
    }),
  );
  assert.equal(res.status, 200);
  assert.deepEqual(up.seen[0], {
    url: "http://egin:8787/sessions/s1/messages?x=1",
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer secret", "x-egin-user": "u1" },
    body: '{"message":"hi"}',
  });
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  assert.equal(res.headers.get("x-accel-buffering"), "no");
  assert.equal(res.headers.get("set-cookie"), null, "upstream headers other than the content type stay behind");
});

test("finds its mount point when no prefix is given", async () => {
  const up = upstream();
  const proxy = createProxy({ target: "http://egin:8787", fetch: up.fetch });
  await proxy(new Request("https://app.example/any/where/sessions/s1/events?after=3"));
  assert.equal(up.seen[0].url, "http://egin:8787/sessions/s1/events?after=3");
  await proxy(new Request("https://app.example/api/egin/agent"));
  assert.equal(up.seen[1].url, "http://egin:8787/agent");
});

test("refuses a request with no user, and anything that is not a session route", async () => {
  const up = upstream();
  const proxy = createProxy({ target: "http://egin:8787", prefix: "/api/egin", user: () => null, fetch: up.fetch });
  assert.equal((await proxy(new Request("https://app.example/api/egin/sessions"))).status, 401);
  assert.equal((await proxy(new Request("https://app.example/api/egin/channels/slack", { method: "POST", body: "{}" }))).status, 404);
  assert.equal(up.seen.length, 0);
});
