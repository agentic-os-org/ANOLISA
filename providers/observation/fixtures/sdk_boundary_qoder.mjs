/** Exercise real Qoder MCP dispatch using synthetic calls and owned local effects. */
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, writeSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [npmRoot, mode, marker, inheritedFd] = process.argv.slice(2);
const require = createRequire(`${npmRoot}/package.json`);
const sdk = await import(pathToFileURL(require.resolve("@qoder-ai/qoder-agent-sdk")));
const { Client } = await import(pathToFileURL(require.resolve("@modelcontextprotocol/sdk/client/index.js")));
const { InMemoryTransport } = await import(pathToFileURL(require.resolve("@modelcontextprotocol/sdk/inMemory.js")));
const state = { entered: 0, memory_effect: 0, operation_error: null };
const handler = async () => {
  state.entered++;
  try {
    if (mode === "memory" || mode === "partial") state.memory_effect++;
    if (mode === "file" || mode === "partial") writeFileSync(marker, "synthetic");
    if (mode === "exec") {
      const result = spawnSync("/bin/sh", ["-c", 'printf synthetic > "$1"', "fixture", marker]);
      if (result.error) throw result.error;
      if (result.status !== 0) throw new Error("child_failed");
    }
    if (mode === "inherited_fd") writeSync(Number(inheritedFd), "synthetic");
  } catch (error) { state.operation_error = error.code ?? "operation_failed"; }
  return { content: [{ type: "text", text: JSON.stringify(state) }] };
};
const server = sdk.createSdkMcpServer({ name: "boundary", tools: [sdk.tool("effect", "Synthetic effect", {}, handler)] });
const client = new Client({ name: "boundary-fixture", version: "1.0.0" });
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
try {
  await server.instance.connect(serverTransport);
  await client.connect(clientTransport);
  const result = await client.callTool({ name: "effect", arguments: {} });
  if (result.isError) throw new Error("SDK dispatch failed");
  console.log(JSON.stringify({ sdk: "qoder-agent-sdk", version: JSON.parse(readFileSync(`${npmRoot}/node_modules/@qoder-ai/qoder-agent-sdk/package.json`, "utf8")).version, mode, no_new_privs: /^NoNewPrivs:\s+1$/m.test(readFileSync("/proc/self/status", "utf8")), ...state }));
} finally {
  await client.close();
  await server.instance.close();
}
