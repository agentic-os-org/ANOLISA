/** Deliberately synthetic SDK double; the real SDK parser has a separate native suite. */
import { writeFileSync } from "node:fs";
export async function startDaemon() {
  return { async stop() { writeFileSync("daemon-stopped", "yes"); } };
}
let fixtureQuery;
export async function runFixture() { return await fixtureQuery.fixture(); }
export function query({ options }) {
  const signal = new AbortController().signal;
  const native = { session_id: "session-1", tool_use_id: "call-1", cwd: options.cwd,
    tool_name: "Bash", tool_input: { command: "fixture" }, scenario: process.env.QODER_SCENARIO ?? "observe" };
  let sequence = 0;
  const callbacks = new Map();
  const q = {
    transport: { write() {} },
    async initializationResult() {
      if (process.env.QODER_INIT_FAIL) throw new Error("fixture initialization failed");
      return { capabilities: ["msg_lifecycle_v1"] };
    },
    recordInboundSessionMessage() {},
    async handleControlRequest(frame) {
      const { input, callback_id } = frame.request;
      return await callbacks.get(callback_id)(input, input.tool_use_id, { signal });
    },
    async close() {},
    async interrupt() {},
    async getContextUsage() {},
    async streamInput() {},
    async next() { return { done: true }; },
    async fixture() {
      this.recordInboundSessionMessage({ type: "assistant", session_id: native.session_id,
        message: { content: [{ type: "tool_use", id: native.tool_use_id,
          name: native.tool_name, input: native.tool_input }] } });
      const denied = await emit("PreToolUse");
      if (!denied) {
        writeFileSync("executed", "yes");
        if (process.env.QODER_FAILURE) await emit("PostToolUseFailure", { error: "tool failed", is_interrupt: false });
        else await emit("PostToolUse", { tool_response: "native result" });
      }
      this.recordInboundSessionMessage({ type: "user", message: { content: [
        { type: "tool_result", tool_use_id: native.tool_use_id, content: "result" }] } });
      return { denied };
    },
  };
  async function emit(name, fields = {}) {
    for (const group of options.hooks[name] ?? [])
      for (const callback of group.hooks) {
        const input = { ...native, hook_event_name: name, ...fields };
        const callback_id = `callback-${++sequence}`;
        callbacks.set(callback_id, callback);
        const result = await q.handleControlRequest({ request_id: `dispatch-${sequence}`,
          request: { subtype: "hook_callback", callback_id, input, tool_use_id: input.tool_use_id } });
        if (result.hookSpecificOutput?.permissionDecision === "deny") return true;
      }
    return false;
  }
  fixtureQuery = q;
  return q;
}
