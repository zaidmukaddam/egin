// Run after `astro build`, as `npm run build` does. The Workflow integration writes the function that
// runs workflow steps only when VERCEL_DEPLOYMENT_ID is set, which Vercel does when it builds and the
// build script does for a build made here and deployed with `vercel deploy --prebuilt`. A build
// without it would deploy a site whose Ask AI starts sessions that nothing runs.
//
// The Workflow SDK builds that function as one bundled file, and the docs agent's steps need more
// than code: the agent folder, the docs it reads, and an egin binary. This copies them in, at the
// paths they have in the pages' function, so the same agent description (src/lib/egin-agent.ts)
// holds in both.
//
// It also sets the function's architecture to x86_64, like the pages' function. The SDK asks for
// arm64, and on 23 Nov 2026 a preview ran it as x64 all the same while production ran it as arm64,
// where an arm64 egin built on a newer Linux failed to load ("GLIBC_2.39 not found"). One
// architecture means one binary, the one bin/ already holds for the pages.
import { cpSync, existsSync, readFileSync, writeFileSync } from "node:fs";

const fn = ".vercel/output/functions/.well-known/workflow/v1/flow.func";
if (!existsSync(fn)) {
  console.error(`${fn} is missing: the Workflow integration only writes it when VERCEL_DEPLOYMENT_ID is set, as \`npm run build\` does.`);
  process.exit(1);
}
// The SDK compiles workflow functions only outside node_modules, so `egin` has to be linked from ../npm/egin
// (npm does that for a file: dependency) rather than copied in (bun does). A copy builds and deploys cleanly,
// then every Ask AI session fails with "'start' received an invalid workflow function".
if (!readFileSync("src/pages/.well-known/workflow/v1/_manifest.json", "utf8").includes("npm/egin/dist/workflow/")) {
  console.error("The egin workflow was not compiled: node_modules/egin must be a symlink to ../npm/egin. Install with `npm install`, not bun.");
  process.exit(1);
}
const config = JSON.parse(readFileSync(`${fn}/.vc-config.json`, "utf8"));
writeFileSync(`${fn}/.vc-config.json`, JSON.stringify({ ...config, architecture: "x86_64" }, null, 2));

const files = ["agent/agent.toml", "agent/instructions.md", "agent/middleware", "src/content/docs", "bin/egin-linux-x64"];
for (const file of files) {
  if (!existsSync(file)) {
    console.error(`${file} is missing, and the workflow function needs it.`);
    process.exit(1);
  }
  cpSync(file, `${fn}/${file}`, { recursive: true, dereference: true });
}
console.log(`workflow function: x86_64, with ${files.join(", ")}`);
