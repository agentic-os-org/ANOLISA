/** Isolate the irreversible process fence when a native consumer cannot be joined. */
import assert from "node:assert/strict";
import { fixture, pause } from "./parser-support.mjs";
import { ExecutionOwner } from "../../../../aw-adapter/native/qoder-sdk/private.mjs";
import { NATIVE_COMMAND_WRAPPER_VERSION } from "../../../../aw-adapter/native/qoder-sdk/command-wrapper.mjs";

const cleanup = [];
let released = false;
let entered = false;
let complete;
const completed = new Promise(resolve => { complete = resolve; });
const timeout = process.argv[3] === "timeout";
const missingRelease = process.argv[3] === "missing-release";
const f = await fixture({ after: callback => cleanup.push(callback) }, undefined, {
  canUseTool: async () => ({ behavior: "allow" }),
  awNativeExecutionFactory: async () => {
    const lease = { version: NATIVE_COMMAND_WRAPPER_VERSION,
      commandWrapper: { executable: "/trusted/wrapper", ticket: "opaque-ticket-0123456789" },
      settle: async () => {
        entered = true;
        if (timeout) await completed;
        else throw new Error("fixture-secret-do-not-expose");
      },
      release: async () => { released = true; },
    };
    if (missingRelease) delete lease.release;
    return lease;
  },
}, [], { native_command_wrappers: ["/trusted/wrapper"] });
try {
  f.model("uncertain");
  const responses = await f.parallelHooks("uncertain", "pre", "PreToolUse", { command: "fixture" });
  if (missingRelease)
    assert.equal(responses.at(-1).response.hookSpecificOutput.permissionDecision, "deny");
  else {
    assert.equal(responses.at(-1).response.hookSpecificOutput.updatedInput, undefined);
    f.transport.emit({ type: "control_request", request_id: "permission", request: {
      subtype: "can_use_tool", tool_name: "Bash", tool_use_id: "uncertain", input: { command: "fixture" } } });
    assert((await f.response("permission")).response.updatedInput);
    f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "uncertain" }] } });
  }
  for (let count = 0; !(timeout ? entered : f.resident.failure) && count < 1000; count++) await pause();
  if (timeout) {
    assert.equal(entered, true);
    f.resident.cleanupDeadline = Date.now() + 30;
    await assert.rejects(f.query.close(), /cleanup incomplete/);
  }
  assert(f.resident.failure);
  assert(!String(f.resident.failure).includes("fixture-secret"));
  assert.equal(released, false);
  assert.equal(f.scope.invocations.size, 1);
  assert.throws(() => new ExecutionOwner(async () => {}).check(), /restart/);
  await assert.rejects(f.query.close(), /cleanup incomplete/);
  assert.equal(released, false);
  if (timeout) {
    complete();
    await Promise.allSettled([...f.resident.nativeCommands.records.values()].map(record => record.pending));
    assert.equal(released, true);
    assert.throws(() => new ExecutionOwner(async () => {}).check(), /restart/);
  }
} finally {
  complete();
  for (const callback of cleanup) await callback().catch(() => {});
}
