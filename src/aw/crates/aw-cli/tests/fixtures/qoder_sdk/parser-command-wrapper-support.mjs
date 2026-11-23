/** Drive PreTool and explicit native authorization as separate SDK protocol stages. */
import assert from "node:assert/strict";
import { fixture, pause } from "./parser-support.mjs";
import { NATIVE_COMMAND_WRAPPER_VERSION } from "../../../../aw-adapter/native/qoder-sdk/command-wrapper.mjs";

export const wrapper = "/opt/trusted/tool-wrapper";
export const ticket = "opaque-ticket-0123456789";
const hooks = [{ event: "tool.before", step: "security", on_error: "block", budget_ms: 100 },
  { event: "tool.after", step: "completion", on_error: "report", budget_ms: 100 }];
export const lease = release => ({ version: NATIVE_COMMAND_WRAPPER_VERSION, release, settle: async () => {},
  commandWrapper: { executable: wrapper, ticket } });

export async function setup(t, extra = {}, callback, source) {
  const state = { acquired: [], released: [], policies: [] };
  const f = await fixture(t, callback, {
    permissionMode: "default",
    canUseTool: async () => ({ behavior: "allow" }),
    awNativeExecutionFactory: async context => {
      assert(Object.isFrozen(context));
      assert(Object.isFrozen(context.input));
      state.acquired.push(context);
      return lease(async () => state.released.push(context.invocationId));
    }, ...extra,
  }, hooks, { native_command_wrappers: [wrapper] }, source);
  f.scope.invoke = async (hook, payload) => {
    state.policies.push({ event: hook.event, payload });
    return { code: 0, stdout: Buffer.from("{}") };
  };
  return { ...f, state };
}

export async function preTool(f, id = "call", input = { command: "echo logical" }, native = {}) {
  f.model(id, input);
  const responses = await f.parallelHooks(id, `${id}-pre`, "PreToolUse", input, native);
  return responses.at(-1).response;
}

export async function prepare(f, id = "call", input = { command: "echo logical" }) {
  const result = await preTool(f, id, input);
  if (result.hookSpecificOutput.permissionDecision === "deny") return result;
  assert.equal(result.hookSpecificOutput.permissionDecision, "ask");
  assert.equal(result.hookSpecificOutput.updatedInput, undefined, "native rules must receive logical input");
  f.transport.emit({ type: "control_request", request_id: `${id}-permission`, request: {
    subtype: "can_use_tool", tool_name: "Bash", tool_use_id: id, input } });
  const approved = (await f.response(`${id}-permission`)).response;
  assert.equal(approved.behavior, "allow");
  return { ...result, hookSpecificOutput: { ...result.hookSpecificOutput, updatedInput: approved.updatedInput } };
}

export async function retire(f, id) {
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: id }] } });
  for (let count = 0; f.scope.invocations.size && count < 1000; count++) await pause();
  assert.equal(f.scope.invocations.size, 0);
}
