#!/usr/bin/env node
// Runs the egin binary for this machine. The package carries one per platform, in bin/<platform>-<arch>/egin.
import { spawnSync } from "node:child_process";
import { accessSync, chmodSync, constants, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const bin = fileURLToPath(new URL(`./${process.platform}-${process.arch}/egin`, import.meta.url));
if (!existsSync(bin)) {
  console.error(`egin has no prebuilt binary for ${process.platform}-${process.arch}.`);
  console.error("Install it with Cargo instead: cargo install egin");
  process.exit(1);
}
try {
  accessSync(bin, constants.X_OK);
} catch {
  chmodSync(bin, 0o755);
}
// Under npx, `egin` isn't on PATH, so commands egin prints must go through npx too.
const env = process.env.npm_command === "exec" ? { ...process.env, EGIN_LAUNCHER: "npx egin-ts@latest" } : process.env;
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", env });
if (r.error) throw r.error;
process.exit(r.status ?? 1);
