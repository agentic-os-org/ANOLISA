/** Ordinary resident Queries switch policy only after complete scope and native cleanup. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { runFixture, state } from "@qoder-ai/qoder-agent-sdk";
const { createQuery, startDaemon, createSdkMcpServer } =
  await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(fs.readFileSync(process.env.AW_QODER_SDK_CONFIG));
const binding = JSON.parse(fs.readFileSync(config.binding));
assert.equal(config.resident_policy_reload, true);
const initialPid = process.pid;
const aw = args => spawnSync(config.executable, args, { encoding: "utf8", timeout: 10000 });
const status = () => {
  const result = aw(["service", "status", "--socket", binding.socket]);
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout).result.instances.find(instance => instance.instance_id === binding.binding.instance_id);
};
const update = (name, busy = false) => {
  const previous = status().policy_revision;
  const result = aw(["update", "--socket", binding.socket, "--agent", "sdk", "--policy", path.resolve(name)]);
  if (busy) {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /busy/);
    assert.equal(status().policy_revision, previous);
  } else assert.equal(result.status, 0, result.stderr);
};
const daemon = await startDaemon({});
const create = async (extra = {}) => createQuery({ prompt: "fixture", options: {
  daemon, settingSources: [], cwd: process.cwd(), ...extra,
} });
let open;
try {
  open = await create();
  assert.equal(status().requests, 1);
  assert.equal((await runFixture()).denied, false);
  update("deny.json", true);
  await open.close();
  open = null;
  assert.equal(status().requests, 0);
  update("deny.json");
  open = await create();
  assert.equal((await runFixture()).denied, true, "new Query must execute newly added scan");
  assert.equal(state.query.options.hooks.PreToolUse.at(-1).timeout, 5.2,
    "native timeout must use the new 1200ms policy budget");
  await open.close();
  open = null;
  update("empty.json");
  open = await create();
  assert.equal((await runFixture()).denied, false, "removed scan must not remain attached");
  assert.equal(state.query.options.hooks.PreToolUse[0].timeout, undefined);
  let releaseClose, enteredClose;
  const entered = new Promise(resolve => { enteredClose = resolve; });
  const gate = new Promise(resolve => { releaseClose = resolve; });
  state.closing = async () => { enteredClose(); await gate; };
  const closing = open.close();
  await entered;
  update("strict.json", true);
  releaseClose();
  await closing;
  state.closing = null;
  open = null;
  update("strict.json");
  open = await create();
  let strictFailure;
  try { await runFixture(); } catch (error) { strictFailure = error; }
  assert(strictFailure, "new strict Post policy must prevent result delivery");
  await open.close();
  open = null;
  update("report.json");
  open = await create();
  assert.equal((await runFixture()).denied, false, "report Post update must permit delivery");
  await open.close();
  open = null;
  update("mcp-deny.json");
  let executed = 0;
  const server = await createSdkMcpServer({ name: "files", tools: [{ name: "read", handler: async () => {
    executed++;
    return { content: [{ type: "text", text: "receipt" }], isError: false };
  } }] });
  open = await create({ mcpServers: { files: server } });
  const response = await state.query.handleControlRequest({ request_id: "mcp-denied", request: {
    subtype: "mcp_message", server_name: "files",
    message: { method: "tools/call", params: { name: "read", arguments: {} } },
  } }).then(value => ({ value }), error => ({ error }));
  assert(response.error, "updated structured MCP policy must reject dispatch");
  assert.equal(executed, 0);
  await open.close();
  open = null;
  assert.equal(status().requests, 0);
  assert.equal(process.pid, initialPid);
  assert.equal(state.started, 1, "daemon remains alive across policy revisions");
  console.log(JSON.stringify({ passed: true, boundaries: 7 }));
} finally {
  state.closing = null;
  await open?.close();
  await daemon.stop();
  assert.equal(state.stopped, 1);
}
