/** Exercise the registered hook against a real MCP child and temporary store. */
import { it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { McpStdioClient } from "../src/mcp-client.js";
import { resolveConfig } from "../src/config.js";
import plugin from "../src/index.js";

const binaryPath = process.env.AGENT_MEMORY_TEST_BINARY;
assert.ok(binaryPath, "set AGENT_MEMORY_TEST_BINARY to the built agent-memory binary");

for (const scope of ["capture", "all", undefined] as const) {
  it(`agent_end honors ${scope ?? "default allow"} consent and tracks captures`, { timeout: 60000 }, async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "memory-capture-"));
    const keys = ["MEMORY_BASE_DIR", "MEMORY_MOUNT_STRATEGY", "MEMORY_EMBEDDING_BACKEND", "MEMORY_GIT_ENABLED"];
    const previous = keys.map((key) => process.env[key]);
    Object.assign(process.env, {
      MEMORY_BASE_DIR: dir, MEMORY_MOUNT_STRATEGY: "userland",
      MEMORY_EMBEDDING_BACKEND: "none", MEMORY_GIT_ENABLED: "false",
    });
    const hooks = new Map<string, (...args: any[]) => Promise<void>>();
    const messages: string[] = [];
    const api = {
      pluginConfig: { binaryPath, userId: "capturetest", sessionDir: path.join(dir, "sessions") },
      resolvePath: (p: string) => p,
      logger: { info: (s: string) => messages.push(s), warn: (s: string) => messages.push(s), debug() {}, error() {} },
      on: (name: string, handler: (...args: any[]) => Promise<void>) => hooks.set(name, handler),
      registerTool() {}, registerMemoryCapability() {}, registerMemoryCorpusSupplement() {},
    };
    const client = new McpStdioClient(resolveConfig(api as never));
    const files = (): string[] => {
      const observed = path.join(dir, "user-capturetest", "notes", "observed");
      return fs.existsSync(observed) ? fs.readdirSync(observed).map((name) => path.join(observed, name)) : [];
    };
    const capture = async (content: string) => hooks.get("agent_end")!({ messages: [{ role: "assistant", content }] }, {});
    try {
      plugin.register(api as never);
      const repeated = "Important: user prefers examples alongside explanations.";
      if (scope) {
        assert.match(await client.callTool("memory_consent", { action: "deny", scope }), /deny/);
        await capture(repeated);
        assert.deepEqual(files(), [], "a denied hook must not persist an observation");
        assert.ok(!messages.some((m) => m.includes("auto-captured observation")), "a skipped hook must not log a successful capture");

        await client.callTool("memory_consent", { action: "allow", scope: "capture" });
      }
      await Promise.all([capture(repeated), capture(repeated)]);
      await capture(repeated);
      assert.equal(files().length, 1);
      const automaticPath = files()[0];
      assert.match(fs.readFileSync(automaticPath, "utf8"), /^source: auto-capture$/m);
      const automatic = await client.callTool("memory_auto_created", {});
      assert.ok(automatic.includes(path.basename(automaticPath)));
      assert.ok(automatic.includes("auto-capture"));

      await client.callTool("memory_consent", { action: "deny", scope: "all" });
      await capture("Important: user wants short examples after revocation.");
      assert.equal(files().length, 1, "a running hook must observe another client's new denial");
      const manual = await client.callTool("memory_observe", { content: "Please remember this explicit note.", hint: "manual" });
      assert.match(manual, /observed at notes\/observed\//);
      assert.equal(files().length, 2);
      const manualPath = files().find((file) => file !== automaticPath)!;
      assert.match(fs.readFileSync(manualPath, "utf8"), /^source: manual-observe$/m);

      fs.writeFileSync(path.join(dir, "user-capturetest", ".anolisa", "consent.toml"), "auto_capture = [");
      await capture("Important: user prefers a final checklist.");
      assert.equal(files().length, 2, "unparseable consent must not authorize an automatic write");
      assert.ok(messages.some((m) => m.includes("cannot read capture consent")));
      await client.callTool("memory_consent", { action: "allow", scope: "capture" });
      await capture("Important: user prefers a final checklist.");
      assert.equal(files().length, 3, "a failed capture must be retryable after consent is repaired");
      await capture("Important: user prefers a final checklist.");
      assert.equal(files().length, 3, "a successful retry must still be deduplicated");
    } finally {
      await hooks.get("gateway_stop")?.();
      await client.stop();
      keys.forEach((key, i) => { if (previous[i] === undefined) delete process.env[key]; else process.env[key] = previous[i]; });
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
}
