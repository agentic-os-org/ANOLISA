import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";

test("a changed agent-loop build fails before it can run", () => {
  const root = mkdtempSync(join(tmpdir(), "aw-dsh-preload-"));
  try {
    const module = join(root, "node_modules/@deepseek-ai/dsh-agent-loop/lib/index.js");
    mkdirSync(join(root, "node_modules/@deepseek-ai/dsh-agent-loop/lib"), { recursive: true });
    writeFileSync(module, "globalThis.unsupportedAgentLoopRan = true;\n");
    const preload = pathToFileURL(resolve(import.meta.dirname, "final-preload.mjs"));
    const result = spawnSync(process.execPath, [
      "--input-type=module",
      "--eval",
      `await import(${JSON.stringify(pathToFileURL(module).href)});`,
    ], {
      encoding: "utf8",
      timeout: 5000,
      env: { ...process.env, NODE_OPTIONS: `--import=${preload}` },
    });
    if (result.error) throw result.error;
    if (result.status === 0 || !result.stderr.includes("does not support this DSH agent-loop build")) {
      throw Error(`unsupported DSH build was not rejected: ${result.stderr}`);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the DSH process removes only its preload before launching a Node tool", () => {
  const root = mkdtempSync(join(tmpdir(), "aw-dsh-child-"));
  try {
    const module = join(root, "node_modules/@deepseek-ai/dsh-agent-loop/lib/index.js");
    mkdirSync(join(root, "node_modules/@deepseek-ai/dsh-agent-loop/lib"), { recursive: true });
    writeFileSync(module, "export const childModule = true;\n");
    const preload = pathToFileURL(resolve(import.meta.dirname, "final-preload.mjs"));
    const original = "--no-warnings";
    const childScript = `await import(${JSON.stringify(pathToFileURL(module).href)}); console.log(process.env.NODE_OPTIONS ?? '<unset>')`;
    const parent = spawnSync(process.execPath, [
      "--input-type=module",
      "--eval",
      `import { spawnSync } from "node:child_process";
       const child = spawnSync(process.execPath, ["--input-type=module", "--eval",
         ${JSON.stringify(childScript)}],
         { encoding: "utf8", timeout: 5000 });
       if (child.error) throw child.error;
       if (child.status !== 0) throw Error(child.stderr);
       console.log(JSON.stringify({ parent: process.env.NODE_OPTIONS, child: child.stdout.trim() }));`,
    ], {
      encoding: "utf8",
      timeout: 5000,
      env: { ...process.env, NODE_OPTIONS: `${original} --import=${preload}` },
    });
    if (parent.error) throw parent.error;
    if (parent.status !== 0) throw Error(parent.stderr);
    const observed = JSON.parse(parent.stdout.trim());
    if (observed.parent !== original || observed.child !== original) {
      throw Error(`original NODE_OPTIONS was not preserved: ${parent.stdout}`);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
