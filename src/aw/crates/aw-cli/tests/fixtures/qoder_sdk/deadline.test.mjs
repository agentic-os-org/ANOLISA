/** Exercise real bridge/execute timers and the real CLI; no clock mocks or credentials. */
import test from "node:test";
import { deadlineFixture } from "./deadline-support.mjs";

for (const mode of ["denial", "timeout"])
  test(`late ${mode} and stalled cleanup reach the native callback`, { timeout: 12000 }, async t => {
    const f = await deadlineFixture(t, process.env.AW_DEADLINE_TEST_EXECUTABLE, mode);
    const input = { hook_event_name: "PreToolUse", session_id: "session", tool_use_id: "call",
      tool_name: "Bash", tool_input: { command: "fixture" }, cwd: f.root };
    const started = performance.now();
    const result = await f.group.hooks[0](input, input.tool_use_id,
      { signal: AbortSignal.timeout(f.group.timeout * 1000) });
    await f.verify(input, result, performance.now() - started);
  });
