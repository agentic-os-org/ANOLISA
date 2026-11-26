/** Run a directly booted rc.5 Cordis registry with an application-owned SDK Task. */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { createDeepSeekBridge } from "../../../../../sdk/typescript-adapters/deepseek-harness/src/index.mjs";
import { verifyPublication } from "./publication.mjs";

const [binary, npm] = process.argv.slice(2);
const require = createRequire(resolve(npm, "package.json"));
const { Context } = await import(require.resolve("@deepseek-ai/cordis"));
const { ToolRuntime, defineTool } = await import(require.resolve("@deepseek-ai/dsh-tools"));
const configFile = resolve("aw.json");
const descriptorFile = resolve("descriptor.json");
const aw = (...args) => promisify(execFile)(binary, args, { timeout: 12000 });
const descriptor = await aw("config", "descriptor", "--config", configFile, "--agent", "dsh",
  "--profile", "deepseek-cordis-hooks");
await writeFile(descriptorFile, descriptor.stdout, { mode: 0o600 });
const bridge = await createDeepSeekBridge({ configFile, descriptorFile });
const ctx = new Context();
const original = "left sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP right";
let observed;
try {
  ctx.provide("systemPrompt", { tools() {} });
  await ctx.plugin(ToolRuntime);
  ctx.tools.register(defineTool({
    name: "bash", description: "Deterministic Bridge tool",
    parameters: { command: { type: "string", required: true } },
    output: { schema: { type: "object", properties: {}, additionalProperties: true },
      render: (_args, value) => [{ type: "text", text: value.stdout.text }] },
    execute() { return { kind: "foreground", stdout: { text: original } }; },
  }));
  ctx.on("tools/post-execute", async (_exec, result, next) => {
    const decision = await next();
    return { ...decision, value: { ...result.value, stdout: { text: "platform: " + result.value.stdout.text } } };
  });
  ctx.on("tools/result", (_exec, result) => { observed = result; });
  await ctx.plugin(bridge.plugin);
  assert(ctx.awDeepseek);
  const attached = await aw("connect", "--config", configFile, "--sdk-pid", String(process.pid), "--timeout-ms", "5000");
  assert.equal(JSON.parse(attached.stdout).result.state, "completed");
  const result = await bridge.runTask("session", () => ctx.tools.execute({
    name: "bash", agent: { id: "session" }, callId: "call", arguments: { command: "echo" },
    signal: AbortSignal.timeout(9000),
  }));
  assert.equal(result, observed);
  assert.equal(result.content[0].text, "platform: left [MASKED] right", JSON.stringify(result));
  assert.equal(result.value.stdout.text, result.content[0].text);
} finally { await ctx.fiber.dispose(); }
await verifyPublication(Context, ToolRuntime, defineTool);
