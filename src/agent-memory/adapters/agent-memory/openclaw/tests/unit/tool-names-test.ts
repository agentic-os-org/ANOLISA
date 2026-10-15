/** Pin the OpenClaw names independently of the internal MCP operations. */
import { it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import plugin from "../../src/index.js";
import { McpStdioClient, resolveMcpToolName } from "../../src/mcp-client.js";

it("registers distinct tools and routes ANOLISA reads to the existing MCP backend", async (t) => {
  const sessionDir = fs.mkdtempSync(
    path.join(os.tmpdir(), "memory-tool-names-"),
  );
  t.after(() => fs.rmSync(sessionDir, { recursive: true, force: true }));
  type Tool = {
    name: string;
    parameters: {
      required: string[];
      properties: Record<string, { type: string }>;
    };
    execute: (
      id: string,
      params: Record<string, unknown>,
    ) => Promise<{ content: { text: string }[] }>;
  };
  const tools = new Map<string, Tool>();
  let promptBuilder: () => string[] = () => [];
  const calls: { name: string; params: unknown }[] = [];
  t.mock.method(
    McpStdioClient.prototype,
    "callTool",
    async (name: string, params: unknown) => {
      calls.push({ name: resolveMcpToolName(name), params });
      return resolveMcpToolName(name) === "memory_search"
        ? JSON.stringify([
            { path: "notes/seed.md", score: 1, snippet: "ANOLISA-SEED" },
          ])
        : "ANOLISA-SEED";
    },
  );
  plugin.register({
    pluginConfig: { binaryPath: process.execPath, userId: "1000", sessionDir },
    resolvePath: (p: string) => p,
    logger: { info() {}, warn() {}, error() {}, debug() {} },
    on() {},
    registerTool(spec: Tool, options: { names: string[] }) {
      assert.deepEqual(options.names, [spec.name]);
      tools.set(spec.name, spec);
    },
    registerMemoryCapability(capability: { promptBuilder: () => string[] }) {
      promptBuilder = capability.promptBuilder;
    },
    registerMemoryCorpusSupplement() {},
  } as never);

  const manifest = JSON.parse(
    fs.readFileSync(
      new URL("../../openclaw.plugin.json", import.meta.url),
      "utf8",
    ),
  );
  assert.deepEqual(
    [...tools.keys()].sort(),
    manifest.contracts.tools.toSorted(),
  );
  for (const name of tools.keys()) {
    assert.ok(
      manifest.toolMetadata?.[name]?.profiles?.includes("coding"),
      `registered memory contract ${name} must be available in coding conversations`,
    );
  }
  assert.equal(tools.has("memory_search"), false);
  assert.equal(tools.has("memory_get"), false);
  const prompt = promptBuilder().join("\n");
  assert.match(prompt, /`anolisa_memory_search\(query/);
  assert.match(prompt, /`anolisa_memory_get`/);
  assert.match(
    prompt,
    /OpenClaw's `memory_search` and `memory_get` belong to its own memory backend/,
  );

  const search = await tools
    .get("anolisa_memory_search")!
    .execute("search", { query: "ANOLISA-SEED" });
  assert.match(search.content[0].text, /ANOLISA-SEED/);
  const read = await tools
    .get("anolisa_memory_get")!
    .execute("read", { path: "notes/seed.md" });
  assert.equal(read.content[0].text, "ANOLISA-SEED");
  assert.deepEqual(calls, [
    { name: "memory_search", params: { query: "ANOLISA-SEED" } },
    { name: "mem_read", params: { path: "notes/seed.md" } },
  ]);

  // Keep the model-facing Tool Search example executable against the
  // registered contract, including its required MCP input.
  const example = /```json\n([^`]+)\n```/.exec(prompt);
  assert.ok(example, "memory guidance must include a Tool Search call example");
  const { id, args } = JSON.parse(example[1]);
  assert.equal(id, `openclaw:${manifest.id}:memory_observe`);
  const observe = tools.get("memory_observe")!;
  for (const key of observe.parameters.required) {
    assert.ok(Object.hasOwn(args, key), `example is missing required ${key}`);
  }
  for (const [key, value] of Object.entries(args)) {
    assert.equal(typeof value, observe.parameters.properties[key]?.type, key);
  }
  const observed = await observe.execute("observe-example", args);
  assert.equal(observed.content[0].text, "ANOLISA-SEED");
  assert.deepEqual(calls.at(-1), { name: "memory_observe", params: args });
});
