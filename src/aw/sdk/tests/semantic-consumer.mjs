import assert from "node:assert/strict";
import { Instance } from "@anolisa/aw-sdk";
const capabilities = {
  adapter: "sdk",
  version: "aw-hook-source/v1",
  entrypoint: "application",
  events: {
    "input.submit": ["observe", "block"],
    "tool.before": ["observe", "block"],
    "tool.after": ["observe"],
  },
};
const instance = new Instance({
  endpoint: process.argv[2],
  target: "app",
  availability: "required",
  capabilities,
  install: (routes) =>
    routes.hooks.every(
      (h) => Object.hasOwn(capabilities.events, h.event) && !h.replace_result,
    ),
});
async function main() {
  if (process.env.AW_SEMANTIC_EMPTY === "1") {
    try {
      assert.equal(await instance.connect(), true);
      const task = await instance.task();
      try {
        const payload = {
          session_id: "session",
          call_id: "empty",
          name: "bash",
          input: {},
        };
        for (const event of ["tool.before", "tool.after"]) {
          assert.deepEqual(
            await task.emit(event, {
              ...payload,
              ...(event === "tool.after" ? { result: "done" } : {}),
            }),
            { decision: "continue", coverage: "checked" },
          );
        }
      } finally {
        await task.close();
      }
    } finally {
      await instance.close();
    }
    return;
  }
  let task;
  try {
    assert.equal(await instance.connect(), true);
    task = await instance.task();
    assert.deepEqual(
      await task.emit("input.submit", {
        session_id: "session",
        turn_id: "turn",
        text: "deny",
      }),
      { decision: "block", coverage: "checked" },
    );
    let executions = 0;
    for (const command of ["deny", "allow"]) {
      const payload = {
        session_id: "session",
        call_id: command,
        name: "bash",
        input: { command },
      };
      const reply = await task.emit("tool.before", payload);
      assert.equal(reply.coverage, "checked");
      if (reply.decision === "continue") {
        executions++;
        assert.deepEqual(
          await task.emit("tool.after", { ...payload, result: "done" }),
          { decision: "continue", coverage: "checked" },
        );
      }
    }
    assert.equal(executions, 1);
    await assert.rejects(
      task.emit("tool.before", {
        session_id: "session",
        call_id: "bad",
        name: "bash",
        input: {},
        instance_id: "foreign",
      }),
      { code: "sdk_input_invalid" },
    );
    await assert.rejects(task.emit("agent.end", {}), {
      code: "semantic_event_unavailable",
    });
  } finally {
    await task?.close();
    await instance.close();
  }
  const inflated = new Instance({
    endpoint: process.argv[2],
    target: "app",
    availability: "required",
    install: () => true,
    capabilities: {
      ...capabilities,
      events: {
        ...capabilities.events,
        "tool.after": ["observe", "block"],
      },
    },
  });
  try {
    await assert.rejects(inflated.connect());
  } finally {
    await inflated.close();
  }
}
await main();
