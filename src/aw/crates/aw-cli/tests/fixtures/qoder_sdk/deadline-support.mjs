/** Real CLI dispatch against a bounded framed service with an intentionally stalled close. */
import assert from "node:assert/strict";
import { mkdtemp, writeFile, chmod, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
import { createQuery } from "../../../../aw-adapter/native/qoder-sdk/bridge.mjs";

export const retryReason = "Tokenless retry: submit fresh Bash arguments {\"command\":\"printf corrected\"}";
export async function deadlineFixture(t, executable, mode) {
  const root = await mkdtemp(join(tmpdir(), "aw-sdk-deadline-"));
  const socket = join(root, "aw.sock"), binding = join(root, "binding");
  const identity = { generation: "fixture", config_revision: "a".repeat(64) };
  const calls = [], connections = new Set(), timers = new Set(), errors = [];
  const wait = ms => new Promise(resolve => {
    const timer = setTimeout(() => { timers.delete(timer); resolve(); }, ms);
    timers.add(timer);
  });
  const server = createServer(stream => {
    connections.add(stream);
    stream.on("close", () => connections.delete(stream));
    stream.on("error", error => { if (error.code !== "EPIPE") errors.push(error); });
    let buffer = Buffer.alloc(0);
    stream.on("data", async chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length < 4 || buffer.length < 4 + buffer.readUInt32BE(0)) return;
      stream.removeAllListeners("data");
      try {
        const request = JSON.parse(buffer.subarray(4));
        const op = request.operation;
        calls.push(op);
        let result = {};
        if (op.method === "hook_plan") result = { plan: { budget_ms: 100,
          groups: [[{ id: "last", on_error: "block" }]], replacement: false, observe: false } };
        else if (op.method === "open_hook_event") result = { event_id: "event", instance_id: "instance",
          steps: ["last"], candidate_revision: "candidate" };
        else if (op.method === "invoke_step") {
          // Use the actual RPC deadline, preserving the CLI's two-second allowance.
          const remaining = Number(BigInt(request.deadline_ns) - process.hrtime.bigint()) / 1e6;
          if (mode === "timeout") return;
          await wait(Math.max(0, remaining - 100));
          result = { status: "ok", kind: "provider", effects: [
            { type: "block", reason_code: "tokenless_retry", text: retryReason }] };
        } else if (op.method === "close_hook_event") return;
        else assert.equal(op.method, "status");
        const bytes = Buffer.from(JSON.stringify({ api_version: request.api_version, identity,
          result, error: null, audit_key: null }));
        const prefix = Buffer.alloc(4); prefix.writeUInt32BE(bytes.length);
        stream.end(Buffer.concat([prefix, bytes]));
      } catch (error) { errors.push(error); stream.destroy(); }
    });
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  await chmod(socket, 0o600);
  t.after(async () => {
    for (const timer of timers) clearTimeout(timer);
    for (const stream of connections) stream.destroy();
    await new Promise(resolve => server.close(resolve));
    await rm(root, { recursive: true, force: true });
    assert.deepEqual(errors, []);
  });
  await writeFile(binding, JSON.stringify({ adapter: "qoder-sdk", reloadable: true,
    binding: { identity, instance_id: "instance", target: "sdk", audit_key: "audit" },
    socket, cwd: root, events: {} }), { mode: 0o600 });
  const sdk = join(root, "sdk.mjs");
  await writeFile(sdk, "export function query({options}) { return { options, async initializationResult(){}, async close(){} }; }");
  const config = { sdk, executable, binding, cwd: root, ready_path: join(root, "ready"), ready_token: "fixture",
    hooks: [{ event: "tool.before", step: "$aw.dispatch", on_error: "block", budget_ms: 100, replace_result: false }] };
  const file = join(root, "config.json");
  await writeFile(file, JSON.stringify(config));
  const previous = process.env.AW_QODER_SDK_CONFIG;
  process.env.AW_QODER_SDK_CONFIG = file;
  let query;
  try { query = await createQuery({ prompt: "fixture", options: { cwd: root, settingSources: [] } }); }
  finally {
    if (previous === undefined) delete process.env.AW_QODER_SDK_CONFIG;
    else process.env.AW_QODER_SDK_CONFIG = previous;
  }
  const group = query.options.hooks.PreToolUse.at(-1);
  return { root, calls, group, async verify(input, result, elapsed) {
    assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
    assert.equal(result.hookSpecificOutput.permissionDecisionReason,
      mode === "timeout" ? "Tool blocked by AW policy" : retryReason);
    assert(elapsed >= 3800, "callback must survive the old budget + 3000 deadline");
    const close = calls.filter(call => call.method === "close_hook_event");
    assert.equal(close.length, 1);
    assert.equal(close[0].session_id, input.session_id);
    assert.equal(close[0].call_id, input.tool_use_id);
    assert.equal(close[0].instance_id, "instance");
    assert.equal(calls.filter(call => call.method === "invoke_step").length, 1);
    // execute() resolves only after the child and its pipe handles have closed.
    for (let i = 0; connections.size && i < 100; i++) await delay(10);
    assert.equal(connections.size, 0, "CLI sockets must close before returning the denial");
  } };
}
