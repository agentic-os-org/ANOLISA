/** Exercise the packaged Bridge contract against a real Service with a Cordis event fixture. */
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { resolve } from "node:path";
import assert from "node:assert/strict";
import { createDeepSeekBridge } from "../../../../../sdk/typescript-adapters/deepseek-harness/src/index.mjs";

const [binary, mode] = process.argv.slice(2);
const bridge = await createDeepSeekBridge({
  configFile: resolve("aw.json"), descriptorFile: resolve("deployment.json"), attachTimeoutMs: 3000,
});
const handlers = new Map();
let cleanup, guard;
const slot = { current: null };
bridge.plugin.apply({
  on(event, fn) { handlers.set(event, fn); return () => handlers.delete(event); },
  tools: { [Symbol.for("aw.deepseek.resultGate")]: slot, guard(fn) { guard = fn; return () => { guard = undefined; }; } },
  effect(fn) { cleanup = fn(); },
  provide(name) { assert.equal(name, "awDeepseek"); },
});
const invocation = (id) => ({
  token: Symbol(), callId: id, agent: { id }, name: "bash", arguments: { command: "echo" },
});
const before = handlers.get("tools/pre-execute");
const gate = slot.current;
try {
  await assert.rejects(before(invocation("unowned"), () => {}), /deepseek_task_required/);
  let started = false;
  await assert.rejects(bridge.runTask("waiting", () => { started = true; },
    { signal: AbortSignal.timeout(50) }), { name: "TimeoutError" });
  assert.equal(started, false);
  const connect = promisify(execFile);
  const [, { stdout }] = await Promise.all([
    bridge.runTask("waiting", () => { started = true; }),
    connect(binary, ["connect", "--socket", resolve("state/aw.sock"),
      "--sdk-pid", String(process.pid), "--timeout-ms", "5000"], { timeout: 7000 }),
  ]);
  assert.equal(started, true);
  assert.equal(JSON.parse(stdout).result.state, "completed");
  await Promise.all(["a", "b"].map(id => bridge.runTask(id, async () => {
    await assert.rejects(bridge.runTask(id, () => {}), /deepseek_session_busy/);
    const exec = invocation(id);
    await before(exec, () => {});
    assert.equal(guard(exec), undefined);
    const original = { isError: false, content: [{ type: "text", text: id }],
      value: { kind: "foreground", stdout: { text: id } }, meta: { preserved: true } };
    const result = await gate.after(exec, original);
    if (mode === "error-after") {
      assert.equal(result.isError, true);
      assert.equal(result.value, undefined);
      assert.equal(result.meta, undefined);
      assert.equal(result.content[0].text, "AW withheld the tool result");
    } else {
      assert.equal(result.value.stdout.text, "compact");
      assert.equal(result.content[0].text, "compact");
      assert.deepEqual(result.meta, original.meta);
    }
    handlers.get("tools/result")(exec, result);
  })));
} finally { await cleanup(); }
assert.equal(handlers.size, 0);
assert.equal(slot.current, null);
