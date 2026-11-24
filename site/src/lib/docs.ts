import { getCollection, type CollectionEntry } from "astro:content";

export const GROUPS = ["Start", "Build", "Run", "Measure", "Reference"] as const;

/** The docs pages in sidebar order. */
export const docs = async () =>
  (await getCollection("docs")).sort(
    (a, b) => GROUPS.indexOf(a.data.group) - GROUPS.indexOf(b.data.group) || a.data.order - b.data.order,
  );

/** A page as plain Markdown, with its title and description on top. */
export const markdown = (entry: CollectionEntry<"docs">) =>
  `# ${entry.data.title}\n\n${entry.data.description}\n\n${(entry.body ?? "").trim()}\n`;
