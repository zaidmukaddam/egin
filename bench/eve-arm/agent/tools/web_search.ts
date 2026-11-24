// egin's own web_search, served by the run's metering proxy: the same You.com search, result format,
// exclude_domains and exclude_terms the egin arm gets. Off (an error) when the task's web is off.
import { defineTool } from "eve/tools";
import { z } from "zod";
import { egin } from "../lib/egin";

export default defineTool({
  description: "Search the web. Returns ranked results with relevant passages.",
  inputSchema: z.object({
    query: z.string().min(1),
    count: z.number().int().positive().optional(),
    freshness: z.string().optional().describe("day, week, month, year or YYYY-MM-DDtoYYYY-MM-DD"),
  }),
  async execute(input) {
    return egin("web_search", input);
  },
});
