const UPPER = new Set(["gpt", "glm", "oss"])
// Names whose capitals are not just the first letter.
const BRANDS: Record<string, string> = { deepseek: "DeepSeek", minimax: "MiniMax", openai: "OpenAI" }
const EFFORTS: Record<string, string> = { xhigh: "Extra high" }

/**
 * A model id as people say it: "anthropic/claude-haiku-5-5" is "Claude Haiku 5.5", "gpt-6-luna" is
 * "GPT-6 Luna", "us.anthropic.claude-sonnet-5-5" is "Claude Sonnet 5.5". The provider is left out.
 */
export function modelName(id: string): string {
  const bare = (id.split("/").pop() ?? id).replace(/^(?:[a-z]{2}\.)?(?:anthropic|openai|meta|mistral|amazon|deepseek)\./, "")
  const words: string[] = []
  for (const part of bare.split("-")) {
    const last = words.at(-1)
    if (/^\d+$/.test(part) && last && /\d$/.test(last)) {
      words[words.length - 1] = `${last}.${part}` // "5", "5" is version 5.5
    } else if (last === "GPT" && (/^\d/.test(part) || part === "oss")) {
      words[words.length - 1] = `GPT-${part.toUpperCase()}` // "gpt", "6" is GPT-6; "gpt", "oss" is GPT-OSS
    } else if (/^\d/.test(part) && last === "GLM") {
      words[words.length - 1] = `GLM-${part}`
    } else if (UPPER.has(part) || /^\d/.test(part) || /^[a-z]\d/.test(part)) {
      words.push(part.toUpperCase()) // "120b", "k3.1"
    } else {
      words.push(BRANDS[part] ?? part.charAt(0).toUpperCase() + part.slice(1))
    }
  }
  return words.join(" ")
}

/** "xhigh" is "Extra high"; the rest are capitalised. */
export function effortName(effort: string): string {
  return EFFORTS[effort] ?? effort.charAt(0).toUpperCase() + effort.slice(1)
}
