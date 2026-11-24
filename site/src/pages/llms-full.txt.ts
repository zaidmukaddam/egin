// Every docs page in reading order, as one Markdown file.
import type { APIRoute } from "astro";
import { docs, markdown } from "../lib/docs";

export const GET: APIRoute = async () =>
  new Response((await docs()).map(markdown).join("\n---\n\n"), {
    headers: { "content-type": "text/plain; charset=utf-8" },
  });
