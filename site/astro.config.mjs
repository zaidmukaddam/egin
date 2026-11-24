import { readdirSync } from "node:fs";
import { defineConfig } from "astro/config";
import vercel from "@astrojs/vercel";
import { satteri } from "@astrojs/markdown-satteri";
import { workflow } from "workflow/astro";
import { highlightsPlugin } from "./src/lib/highlight.mjs";
import { tablesPlugin } from "./src/lib/tables.mjs";
import { chartsPlugin } from "./src/lib/charts.mjs";

export default defineConfig({
  site: "https://egin.dev",
  // Quick start merged into Getting started at /docs.
  redirects: { "/docs/quick-start": "/docs" },
  // A link is fetched when the pointer reaches it, so the click that follows does not wait on the network.
  prefetch: { prefetchAll: true },
  // The drawer's sessions run as Workflow runs (src/workflows/egin-session.ts): this builds them and
  // adds the routes Workflow calls to run their steps.
  integrations: [workflow()],
  // Every page is still prerendered; the adapter only runs /egin/* (the docs drawer) on demand. That
  // function runs the docs agent itself, so it carries the agent folder, the docs and a Linux egin.
  // 800 s is the most a Pro function runs, but production has cut the drawer's event stream at 120 s, so
  // the stream ends itself before that (STREAM_MS in src/pages/egin/[...path].ts): a function that
  // reaches its limit is killed with the agent.
  adapter: vercel({
    maxDuration: 800,
    includeFiles: [
      "./agent/agent.toml",
      "./agent/instructions.md",
      "./agent/middleware/pre_tool",
      "./bin/egin-linux-x64",
      ...readdirSync("./src/content/docs").map((f) => `./src/content/docs/${f}`),
    ],
  }),
  // Straight quotes as written: smart punctuation turned every ' and " in the docs curly, unlike the rest of the site.
  markdown: { syntaxHighlight: false, smartypants: false, processor: satteri({ hastPlugins: [chartsPlugin, highlightsPlugin, tablesPlugin] }) },
});
