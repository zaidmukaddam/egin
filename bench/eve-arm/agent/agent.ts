// eve's default harness, pointed at the same You.com router model and spend cap as the egin arms.
import { existsSync } from "node:fs";
import { createOpenAICompatible } from "@ai-sdk/openai-compatible";
import { defineAgent } from "eve";

/** The file the driver creates once the spend cap is reached (eve runs agent code in another folder,
 * so the driver passes its full path). */
const WRAP_UP = process.env.EGIN_EVE_WRAP_UP_FILE;
const NOTE =
  "You have reached this task's limit (spend limit reached). Do not call any more tools. From what you have found so far, give your best final answer now, in the exact format the task asks for.";

const router = createOpenAICompatible({
  name: "you",
  baseURL: process.env.EGIN_BASE_URL ?? "https://api.you.com/v1",
  apiKey: process.env.YDC_API_KEY,
  includeUsage: true,
});

// USD per million tokens, passed in by the driver from the router's /v1/models.
const price = JSON.parse(process.env.EGIN_EVE_PRICE ?? '{"input":0,"cached":0,"output":0}');
const maxCost = Number(process.env.EGIN_EVE_MAX_COST ?? "1");
const cost = (u: { inputTokens: number; cacheReadTokens: number; outputTokens: number }) =>
  ((u.inputTokens - u.cacheReadTokens) * price.input + u.cacheReadTokens * price.cached + u.outputTokens * price.output) / 1e6;

export default defineAgent({
  model: router(process.env.EGIN_EVE_MODEL ?? "deepseek-v4.1-flash"),
  // The router's models are not in AI Gateway's catalog; the driver passes /v1/models' value.
  modelContextWindowTokens: Number(process.env.EGIN_EVE_CONTEXT ?? "128000"),
  limits: {
    // At the spend cap eve pauses with a budget prompt. egin's arms get one last model call there,
    // with tools off, to answer from what they found. The driver gives eve the same: it accepts the
    // prompt with egin's wrap-up note queued and creates WRAP_UP, after which `approval`
    // below denies every tool call. After that Continue, up to the cap again is allowed: with tools
    // denied the model can only answer, which takes one or two model calls (a denied call, then text).
    sessionUsage: ({ total, window }) => {
      // max_cost = 0 means no cap, as in egin.
      if (!(maxCost > 0)) return false;
      const spent = cost(total);
      const sinceContinue = cost(window);
      return spent > sinceContinue ? sinceContinue >= maxCost : spent >= maxCost;
    },
  },
  approval: () => (WRAP_UP && existsSync(WRAP_UP) ? { type: "denied", reason: NOTE } : "not-applicable"),
});
