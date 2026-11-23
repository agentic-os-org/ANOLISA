import test from "node:test";
import assert from "node:assert/strict";
import { fakeSource } from "./source-fixture.mjs";
import { sourceCapabilities } from "../src/sources.mjs";

test("language sources preserve terminal reasons and return the stop decision", async () => {
  const events = ["session.start", "turn.stop", "session.end"];
  assert.deepEqual(sourceCapabilities(events).events["turn.stop"], ["observe", "block"]);
  const { source, tasks } = fakeSource(async (event, payload) => ({
    decision: event === "turn.stop" && payload.reason === "timeout" ? "block" : "continue",
    coverage: "checked",
  }), { events });
  await source.withSession({ sessionId: "session" }, async (session) => {
    assert.equal((await session.sessionStart()).decision, "continue");
    assert.equal((await session.turnStop("turn", "timeout", { attemptId: "retry" })).decision, "block");
    await session.sessionEnd("cancelled");
  });
  assert.deepEqual(tasks[0].events, [
    ["session.start", { session_id: "session" }],
    ["turn.stop", { session_id: "session", turn_id: "turn", reason: "timeout", attempt_id: "retry" }],
    ["session.end", { session_id: "session", reason: "cancelled" }],
  ]);
});
