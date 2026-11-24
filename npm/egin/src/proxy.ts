// Put `egin serve` behind your app's own origin. The handler takes a web `Request` and returns a
// `Response`, so one function serves a Next.js route handler, SvelteKit, Nuxt, Hono, Bun, Deno and a
// Cloudflare Worker. It adds the server's token, names the signed-in user, and streams events through.
//
//   // app/api/egin/[...path]/route.ts
//   const handler = createProxy({
//     target: process.env.EGIN_URL!,
//     token: process.env.EGIN_TOKEN,
//     prefix: "/api/egin",
//     user: async () => (await auth())?.user?.id,
//   });
//   export { handler as GET, handler as POST, handler as DELETE };

export interface ProxyOptions {
  /** Where `egin serve` listens, such as "https://agent.internal:8787". */
  target: string;
  /** The server's EGIN_TOKEN. It stays on your server; the browser never sees it. */
  token?: string;
  /** The path this handler is mounted at, such as "/api/egin". Found from the URL when left out. */
  prefix?: string;
  /**
   * The signed-in user's id, from your own session check. Each user then reaches only their own
   * sessions. Return nothing to refuse the request with 401. Leave `user` out for a single-user app.
   */
  user?: (request: Request) => string | null | undefined | Promise<string | null | undefined>;
  fetch?: typeof fetch;
}

export function createProxy(options: ProxyOptions): (request: Request) => Promise<Response> {
  const target = options.target.replace(/\/$/, "");
  const prefix = options.prefix?.replace(/\/$/, "");
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);

  return async (request) => {
    const url = new URL(request.url);
    // Without a prefix, everything before the first /sessions or /agent is the mount point.
    const at = prefix !== undefined ? (url.pathname.startsWith(prefix) ? prefix.length : 0) : Math.max(0, url.pathname.search(/\/(sessions|agent)(\/|$)/));
    const path = url.pathname.slice(at);
    // Only the browser's routes: the server's channel webhooks check their own signatures and are not for browsers.
    if (path !== "/agent" && path !== "/sessions" && !path.startsWith("/sessions/")) return new Response("not found", { status: 404 });

    // Built from nothing, so a browser cannot send its own `x-egin-user` or reuse the server's token.
    const headers = new Headers();
    for (const name of ["content-type", "accept", "last-event-id"]) {
      const value = request.headers.get(name);
      if (value) headers.set(name, value);
    }
    if (options.token) headers.set("authorization", `Bearer ${options.token}`);
    if (options.user) {
      const id = await options.user(request);
      if (!id) return new Response("sign in first", { status: 401 });
      headers.set("x-egin-user", id);
    }

    const hasBody = request.method !== "GET" && request.method !== "HEAD";
    const upstream = await doFetch(target + path + url.search, {
      method: request.method,
      headers,
      body: hasBody ? request.body : undefined,
      signal: request.signal,
      // Node needs this to send a streamed body (an upload).
      ...(hasBody ? { duplex: "half" } : {}),
    } as RequestInit);

    const out = new Headers({ "cache-control": "no-store" });
    const type = upstream.headers.get("content-type");
    if (type) out.set("content-type", type);
    // Tells nginx-style proxies not to hold the event stream back.
    if (type?.startsWith("text/event-stream")) out.set("x-accel-buffering", "no");
    return new Response(upstream.body, { status: upstream.status, headers: out });
  };
}
