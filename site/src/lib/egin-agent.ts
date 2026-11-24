// The docs agent as this deployment holds it: its folder, the egin binary, and the docs it reads.
import type { AgentRef } from "egin/workflow";
import { existsSync } from "node:fs";
import path from "node:path";

// The site's files: process.cwd() locally, and wherever the function bundle put agent/ on Vercel.
const root = [process.cwd(), path.join(process.cwd(), "site")].find((d) => existsSync(path.join(d, "agent", "agent.toml")));

export function docsAgent(): AgentRef {
  if (!root) throw new Error("agent/ isn't in this deployment");
  return {
    // EGIN_AGENT_DIR runs another agent folder through the same routes, to try one out.
    dir: process.env.EGIN_AGENT_DIR ?? path.join(root, "agent"),
    // On Vercel the pages' function is x64 and the one that runs workflow steps is arm64, each with
    // its own binary in bin/: `{arch}` picks the one for whichever runs the agent.
    bin: process.env.EGIN_BIN ?? path.join(root, process.platform === "linux" ? "bin/egin-linux-{arch}" : "../target/release/egin"),
    // What the agent reads: the site's own pages, copied into its workspace.
    mounts: { "workspace/docs": path.join(root, "src", "content", "docs") },
    // The agent always calls the Claude API itself, with ANTHROPIC_API_KEY from the project's environment.
    env: { ANTHROPIC_BASE_URL: "" },
  };
}
