import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import test from "node:test";
import { fakeSource } from "./source-fixture.mjs";

test("close drains native calls and their tool.after observations", { timeout: 3000 }, async () => {
  for (const method of ["wrapInput", "wrapTool"]) {
    for (const timeOut of [false, true]) {
      const entered = Promise.withResolvers();
      const release = Promise.withResolvers();
      const observed = Promise.withResolvers();
      const finish = Promise.withResolvers();
      const { source, tasks } = fakeSource(async (event) => {
        assert.equal(tasks[0].closed, false);
        if (event === "tool.after") {
          observed.resolve();
          await finish.promise;
        }
        return { decision: "continue" };
      });
      const session = source.session({ timeoutMs: timeOut ? 100 : 5000 });
      let closing;
      try {
        const wrapped = session[method](async function native(text) {
          entered.resolve();
          await release.promise;
          return text;
        });
        const call = wrapped("done");
        // Observe a rejection even if an assertion stops the rest of this case.
        call.catch(() => {});
        await entered.promise;
        if (timeOut) {
          await assert.rejects(source.close(), { code: "source_close_pending" });
          assert.equal(source.instance.closed, false);
        } else {
          closing = session.close();
          await setImmediate();
        }
        assert.equal(tasks[0].closed, false);
        await assert.rejects(session.afterTool("external", {}, null, { callId: "external" }), {
          code: "source_session_closed", toolExecuted: true,
        });
        release.resolve();
        if (method === "wrapTool") {
          await observed.promise;
          assert.equal(tasks[0].closed, false);
        }
        finish.resolve();
        const [result] = await Promise.all([
          call,
          closing ?? session.close({ timeoutMs: 5000 }),
        ]);
        assert.equal(result, "done");
        assert.equal(tasks[0].closed, true);
      } finally {
        release.resolve();
        finish.resolve();
        await source.close({ timeoutMs: 5000 });
      }
      assert.equal(source.instance.closed, true);
      assert.throws(() => source.session(), { code: "source_closed" });
      assert.deepEqual(tasks[0].events.map(([event]) => event),
        method === "wrapTool" ? ["tool.before", "tool.after"] : ["input.submit"]);
    }
  }
});

test(
  "close keeps instrumented methods in place until admitted requests drain",
  { timeout: 3000 },
  async () => {
    for (const asyncTool of [false, true]) {
      const entered = Promise.withResolvers();
      const release = Promise.withResolvers();
      const toolRuns = [];
      const agent = {
        async ask(prompt, command) {
          void prompt;
          entered.resolve();
          await release.promise;
          return this.shell({ command });
        },
        shell: asyncTool
          ? async (args) => {
              toolRuns.push(args.command);
              return `ran:${args.command}`;
            }
          : (args) => {
              toolRuns.push(args.command);
              return `ran:${args.command}`;
            },
      };
      const { source, tasks } = fakeSource(async (event, payload) => {
        if (event === "tool.before" && payload.input?.command === "deny")
          return { decision: "block", coverage: "checked" };
        return { decision: "continue", coverage: "checked" };
      });
      const session = source.session({ timeoutMs: 100 });
      await assert.rejects(session.close({ timeoutMs: 50 }), {
        code: "invalid_attach_timeout",
      });
      assert.equal(session.closed, false);
      const handle = session.instrument(agent, {
        inputMethods: ["ask"],
        toolMethods: { shell: "shell" },
      });
      try {
        await assert.rejects(agent.shell({ command: "deny" }), {
          code: "source_denied",
        });
        const call = agent.ask("admitted", "deny");
        call.catch(() => {});
        await entered.promise;
        await assert.rejects(source.close(), { code: "source_close_pending" });
        assert.equal(handle.restored, false);
        assert.equal(tasks[0].closed, false);
        assert.equal(source.instance.closed, false);
        release.resolve();
        await assert.rejects(call, { code: "source_session_closed" });
        assert.deepEqual(toolRuns, []);
        await source.close({ timeoutMs: 5000 });
        assert.equal(handle.restored, true);
        assert.equal(tasks[0].closed, true);
        assert.equal(source.instance.closed, true);
        assert.deepEqual(
          tasks[0].events.map(([event]) => event),
          ["tool.before", "input.submit"],
        );
      } finally {
        release.resolve();
        await source.close({ timeoutMs: 5000 });
      }
    }
  },
);

test("close retains completed streams through iterator cleanup and observation", { timeout: 3000 }, async () => {
  const returning = Promise.withResolvers();
  const release = Promise.withResolvers();
  const observed = Promise.withResolvers();
  const finish = Promise.withResolvers();
  const { source, tasks } = fakeSource(async (event) => {
    if (event === "tool.after") {
      observed.resolve();
      await finish.promise;
      assert.equal(tasks[0].closed, false);
    }
    return { decision: "continue" };
  });
  const session = source.session();
  const wrapped = session.wrapToolStream(function native() {
    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      async next() {
        return { done: true };
      },
      async return() {
        returning.resolve();
        await release.promise;
        return { done: true };
      },
    };
  });
  try {
    const completion = wrapped().next();
    completion.catch(() => {});
    await returning.promise;
    const closing = session.close();
    assert.equal(tasks[0].closed, false);
    release.resolve();
    await observed.promise;
    assert.equal(tasks[0].closed, false);
    finish.resolve();
    const [step] = await Promise.all([completion, closing]);
    assert.equal(step.done, true);
    assert.equal(tasks[0].closed, true);
  } finally {
    release.resolve();
    finish.resolve();
    await source.close();
  }
});

test(
  "source.close closes all active sessions concurrently without escape",
  { timeout: 3000 },
  async () => {
    const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
    const s1 = source.session({ sessionId: "s1" });
    const s2 = source.session({ sessionId: "s2" });
    const e1 = Promise.withResolvers();
    const e2 = Promise.withResolvers();
    const release = Promise.withResolvers();
    const t1 = s1.wrapTool(async function a() {
      e1.resolve();
      await release.promise;
      return 1;
    })({ id: 1 });
    const t2 = s2.wrapTool(async function b() {
      e2.resolve();
      await release.promise;
      return 2;
    })({ id: 2 });
    await Promise.all([e1.promise, e2.promise]);
    const started = performance.now();
    try {
      const closing = source.close({ timeoutMs: 120 });
      await setImmediate();
      assert.equal(s1.closed, true);
      assert.equal(s2.closed, true);
      assert.equal(s2.signal.aborted, true);
      await assert.rejects(s2.beforeTool("fast", {}), { code: "source_session_closed" });
      await assert.rejects(closing, { code: "source_close_pending" });
      // Two blocked sessions share the single 120ms close budget rather than 240ms.
      assert.ok(performance.now() - started < 210);
      assert.equal(tasks.some((task) => task.closed), false);
    } finally {
      release.resolve();
    }
    assert.deepEqual(await Promise.all([t1, t2]), [1, 2]);
    await source.close({ timeoutMs: 5000 });
    assert.equal(tasks.every((task) => task.closed), true);
    assert.equal(source.instance.closed, true);
  },
);

test(
  "direct submit and beforeTool track occupancy and reject after close",
  { timeout: 3000 },
  async () => {
    for (const kind of ["submit", "beforeTool"]) {
      const entered = Promise.withResolvers();
      const release = Promise.withResolvers();
      const { source, tasks } = fakeSource(async (_event, _payload, signal) => {
        assert.equal(tasks[0].closed, false);
        entered.resolve();
        // Ignore abort inside the stub to exercise post-RPC close race as well.
        void signal;
        await release.promise;
        assert.equal(tasks[0].closed, false);
        return { decision: "continue", coverage: "checked" };
      });
      const session = source.session({ timeoutMs: 100 });
      try {
        const call =
          kind === "submit"
            ? session.submit("hello")
            : session.beforeTool("shell", { cmd: "ls" });
        call.catch(() => {});
        await entered.promise;
        await assert.rejects(session.close({ timeoutMs: 100 }), {
          code: "source_close_pending",
        });
        assert.equal(tasks[0].closed, false);
        release.resolve();
        await assert.rejects(call, { code: "source_session_closed" });
        await session.close({ timeoutMs: 5000 });
        assert.equal(tasks[0].closed, true);
      } finally {
        release.resolve();
        await source.close({ timeoutMs: 5000 });
      }
    }
  },
);

test(
  "source.close omits stale pending error when all sessions finish before return",
  { timeout: 3000 },
  async () => {
    for (const failShort of [false, true]) {
      const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
      const shortSession = source.session({ sessionId: "short", timeoutMs: 100 });
      const longSession = source.session({ sessionId: "long", timeoutMs: 5000 });
      const shortRelease = Promise.withResolvers();
      const longRelease = Promise.withResolvers();
      const cleanupError = new Error("short cleanup failed");
      const shortGen = shortSession.wrapToolStream(async function* shortStream() {
        try {
          yield "ready";
        } finally {
          await shortRelease.promise;
          if (failShort) throw cleanupError;
        }
      })({});
      const longGen = longSession.wrapToolStream(async function* longStream() {
        try {
          yield "ready";
        } finally {
          await longRelease.promise;
        }
      })({});
      assert.equal((await shortGen.next()).value, "ready");
      assert.equal((await longGen.next()).value, "ready");
      const t1 = setTimeout(() => shortRelease.resolve(), 140);
      const t2 = setTimeout(() => longRelease.resolve(), 180);
      try {
        if (failShort) {
          await assert.rejects(source.close(), (error) => error === cleanupError);
        } else {
          await source.close();
        }
      } finally {
        clearTimeout(t1);
        clearTimeout(t2);
        shortRelease.resolve();
        longRelease.resolve();
      }
      assert.equal(tasks.every((task) => task.closed), true);
      assert.equal(source.instance.closed, true);
    }
  },
);
