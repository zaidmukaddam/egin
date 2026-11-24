// Every docs page as plain Markdown at /docs/<page>.md (the overview is /docs/index.md), for "Copy page",
// "View as Markdown" and handing a page to another chat.
import type { APIRoute, GetStaticPaths } from "astro";
import { getCollection, type CollectionEntry } from "astro:content";
import { markdown } from "../../lib/docs";

export const getStaticPaths = (async () =>
  (await getCollection("docs")).map((entry) => ({ params: { slug: entry.id }, props: { entry } }))) satisfies GetStaticPaths;

export const GET: APIRoute = ({ props }) => {
  const { entry } = props as { entry: CollectionEntry<"docs"> };
  return new Response(markdown(entry), { headers: { "content-type": "text/markdown; charset=utf-8" } });
};
