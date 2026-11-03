/** Native rules see logical commands before any executable wrapper is published. */
import test from "node:test";
import assert from "node:assert/strict";
import { deferred, pause } from "./parser-support.mjs";
import { setup, preTool, retire } from "./parser-command-wrapper-support.mjs";

test("wrapper publication waits for explicit approval of the logical command", { timeout: 5000 }, async t => {
  const entered = deferred(), approved = deferred();
  const logical = { command: "echo logical" };
  const f = await setup(t, { canUseTool: async (_name, input) => {
    assert.deepEqual(input, logical);
    entered.resolve();
    await approved.promise;
    return { behavior: "allow", updatedInput: input };
  } });
  const result = await preTool(f);
  assert.equal(result.hookSpecificOutput.permissionDecision, "ask");
  assert.equal(result.hookSpecificOutput.updatedInput, undefined);
  const entry = f.resident.identities.active.get("call");
  const record = f.resident.nativeCommands.records.get(entry.id);
  assert.equal(record.published, undefined);
  assert.equal(entry.nativeInput, undefined);
  f.transport.emit({ type: "control_request", request_id: "permission", request: {
    subtype: "can_use_tool", tool_name: "Bash", tool_use_id: "call", input: logical } });
  await entered.promise;
  await pause();
  assert.equal(record.published, undefined);
  assert.equal(entry.nativeInput, undefined);
  approved.resolve();
  const permission = (await f.response("permission")).response;
  assert.equal(permission.behavior, "allow");
  assert.notEqual(permission.updatedInput.command, logical.command);
  assert.equal(record.published, true);
  assert.deepEqual(entry.nativeInput, permission.updatedInput);
  await retire(f, "call");
});

test("native rejection before the permission callback releases an unconsumed ticket", { timeout: 5000 }, async t => {
  for (const permissionMode of ["default", "acceptEdits"]) {
    let approvals = 0, settlements = 0;
    const f = await setup(t, { permissionMode,
      canUseTool: async () => { approvals++; return { behavior: "allow" }; },
      awNativeExecutionFactory: async () => ({
        version: "aw.native-command-wrapper/v1",
        commandWrapper: { executable: "/opt/trusted/tool-wrapper", ticket: "opaque-ticket-0123456789" },
        settle: async () => { settlements++; }, release: async () => f.state.released.push("unused"),
      }),
    });
    const result = await preTool(f);
    assert.equal(result.hookSpecificOutput.updatedInput, undefined);
    // The real Worker rejects a command-specific rule without asking the host.
    f.transport.emit({ type: "user", message: { content: [
      { type: "tool_result", tool_use_id: "call", is_error: true, content: "Native rule denied" },
    ] } });
    for (let count = 0; f.scope.invocations.size && count < 1000; count++) await pause();
    assert.equal(f.scope.invocations.size, 0);
    assert.equal(approvals, 0);
    assert.equal(settlements, 0);
    assert.deepEqual(f.state.released, ["unused"]);
  }
});

test("native Hook mode overrides cannot execute logical commands outside the wrapper", { timeout: 5000 }, async t => {
  for (const permissionMode of ["bypassPermissions", "bypass_permissions", "auto", "unknown", null]) {
    const f = await setup(t);
    const result = await preTool(f, "mode-change", { command: "echo logical" }, { permissionMode });
    assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
    assert.equal(result.hookSpecificOutput.updatedInput, undefined);
    assert.equal(f.state.acquired.length, 0);
    await retire(f, "mode-change");
  }
});
