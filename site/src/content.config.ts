import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { z } from "astro/zod";

const docs = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/docs" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    group: z.enum(["Start", "Build", "Run", "Measure", "Reference"]),
    order: z.number(),
  }),
});

const blog = defineCollection({
  loader: glob({ pattern: "*.md", base: "./src/content/blog" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    date: z.coerce.date(),
  }),
});

// The repository's CHANGELOG.md, so GitHub and /changelog read the same file.
const changelog = defineCollection({
  loader: glob({ pattern: "CHANGELOG.md", base: ".." }),
});

export const collections = { docs, blog, changelog };
