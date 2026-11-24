// egin's own web_fetch (You.com Contents, as markdown) in place of eve's direct fetch, so pages pass the
// same exclude_domains / exclude_terms filters as for the egin arm. Off (an error) when the task's web is off.
import { defineTool } from "eve/tools";
import { z } from "zod";
import { egin } from "../lib/egin";

export default defineTool({
  description: "Fetch a web page as markdown.",
  inputSchema: z.object({ url: z.string().min(1) }),
  async execute(input) {
    return egin("web_fetch", input);
  },
});
