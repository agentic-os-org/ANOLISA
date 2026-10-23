/** Deliberately synthetic SDK double; real SDK acceptance is a separate suite. */
import { writeFileSync } from "node:fs";
export function query({ options }) {
  const signal = new AbortController().signal;
  const native = { session_id: "session-1", tool_use_id: "call-1", cwd: process.cwd(),
    tool_name: "Bash", tool_input: { command: "fixture" }, scenario: process.env.QODER_SCENARIO ?? "observe" };
  async function emit(name, fields = {}) {
    for (const group of options.hooks[name] ?? [])
      for (const callback of group.hooks) {
        const result = await callback({ ...native, hook_event_name: name, ...fields }, "call-1", { signal });
        if (result.hookSpecificOutput?.permissionDecision === "deny") return true;
      }
    return false;
  }
  return {
    async initializationResult() {
      if (process.env.QODER_INIT_FAIL) throw new Error("fixture initialization failed");
    },
    async return() {},
    async fixture() {
      const denied = await emit("PreToolUse");
      if (!denied) {
        writeFileSync("executed", "yes");
        if (process.env.QODER_FAILURE) await emit("PostToolUseFailure", { error: "tool failed", is_interrupt: false });
        else await emit("PostToolUse", { tool_response: "native result" });
      }
      return { denied };
    },
  };
}
