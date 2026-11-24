// llms.txt (llmstxt.org) built from the docs at build time, so it lists every page as it is now.
// Each link is the page's plain Markdown; /llms-full.txt is all of them in one file.
import type { APIRoute } from "astro";
import { docs, GROUPS } from "../lib/docs";

export const GET: APIRoute = async ({ site }) => {
  const all = await docs();
  const sections = GROUPS.map((g) => {
    const rows = all
      .filter((e) => e.data.group === g)
      .map((e) => `- [${e.data.title}](${new URL(`/docs/${e.id}.md`, site)}): ${e.data.description}`);
    return rows.length ? `## ${g}\n\n${rows.join("\n")}` : "";
  }).filter(Boolean);
  const txt = `# egin

> Write an agent as a folder of files and run it with one binary. Tools run in a microVM, and a killed run carries on where it stopped.

Every page below is plain Markdown. All of them in one file: ${new URL("/llms-full.txt", site)}

${sections.join("\n\n")}
`;
  return new Response(txt, { headers: { "content-type": "text/plain; charset=utf-8" } });
};
