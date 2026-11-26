/** Exercise rc.5 publication with deterministic cancellation and independent runtimes. */
import assert from "node:assert/strict";
import { installTools } from "../../../../aw-adapter/native/deepseek-harness/hooks.mjs";

async function runtime(Context, ToolRuntime, defineTool) {
  const ctx = new Context();
  ctx.provide("systemPrompt", { tools() {} });
  await ctx.plugin(ToolRuntime);
  let calls = 0;
  const observed = [];
  ctx.on("tools/result", (_exec, result) => { observed.push(result); });
  ctx.tools.register(defineTool({
    name: "bash", description: "Publication probe", parameters: {},
    output: { schema: { type: "string" }, render: (_args, value) => [{ type: "text", text: value }] },
    execute() { calls++; return "ok"; },
  }));
  return {
    ctx,
    async execute(signal = AbortSignal.timeout(5000)) {
      const count = observed.length;
      const result = await ctx.tools.execute({
        name: "bash", agent: { id: "session" }, callId: "call", arguments: {}, signal,
      });
      assert.equal(observed.length, count + 1);
      assert.equal(observed[count], result);
      return result;
    },
    get calls() { return calls; },
  };
}

export async function verifyPublication(Context, ToolRuntime, defineTool) {
  const first = await runtime(Context, ToolRuntime, defineTool);
  const second = await runtime(Context, ToolRuntime, defineTool);
  function install(target, on_error, invoke) {
    return installTools(target.ctx, () => ({
      hooks: [{ event: "tool.after", step: "check", on_error }], invoke,
    }), { resultGate: true });
  }
  try {
    for (const action of ["report", "withhold_result"]) {
      const caller = new AbortController();
      const entered = Promise.withResolvers();
      const release = Promise.withResolvers();
      const dispose = install(first, action, async () => {
        entered.resolve();
        await release.promise;
        throw Error("result check cancelled");
      });
      const before = first.calls;
      const early = await first.execute(AbortSignal.abort());
      assert.equal(first.calls, before);
      const pending = first.execute(caller.signal);
      await entered.promise;
      assert.equal(first.calls, before + 1);
      assert.equal((await second.execute()).value, "ok");
      caller.abort();
      release.resolve();
      const late = await pending;
      assert.equal(early.isError, true);
      assert.equal(late.isError, true);
      if (action === "report") {
        assert.equal(early.error.info.code, "ABORTED_BEFORE_DISPATCH");
        assert.equal(late.error.info.code, "ABORTED");
        assert.equal(late.value, undefined);
      } else {
        for (const result of [early, late]) {
          assert.deepEqual(result.content, [{ type: "text", text: "AW withheld the tool result" }]);
          assert.equal(result.value, undefined);
        }
      }
      dispose();
      assert.equal((await second.execute()).value, "ok");
    }

    // A call already dispatched under a strict gate keeps it across uninstallation.
    const entered = Promise.withResolvers();
    const release = Promise.withResolvers();
    const removePost = first.ctx.on("tools/post-execute", async (_exec, _result, next) => {
      entered.resolve();
      await release.promise;
      return next();
    });
    const removeFirst = install(first, "withhold_result", () => ({ aw_withheld: true }));
    const removeSecond = install(second, "withhold_result", () => ({ aw_withheld: true }));
    const pending = first.execute();
    await entered.promise;
    removeFirst();
    release.resolve();
    assert.equal((await pending).error.message, "AW withheld the tool result");
    removePost();
    assert.equal((await first.execute()).value, "ok");
    assert.equal((await second.execute()).error.message, "AW withheld the tool result");
    removeSecond();
    assert.equal((await second.execute()).value, "ok");
  } finally {
    await Promise.all([first.ctx.fiber.dispose(), second.ctx.fiber.dispose()]);
  }
}
