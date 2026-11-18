import test from "node:test";
import assert from "node:assert/strict";
import { Admission } from "../src/admission.mjs";

const deferred = () => Promise.withResolvers();
function fixture(state = "detached") {
  const opened = deferred();
  const task = { routes: {}, async close() {} };
  let taskSignal;
  const instance = {
    state,
    options: {
      availability: "required",
      capabilities: { events: { "tool.before": [] } },
    },
    async task({ signal }) {
      taskSignal = signal;
      return opened.promise;
    },
  };
  const admission = new Admission(instance, {
    events: ["tool.before"],
    timeoutMs: 1000,
  });
  return {
    admission,
    instance,
    opened,
    task,
    get taskSignal() {
      return taskSignal;
    },
  };
}

for (const cancelled of ["first", "second"]) {
  test(
    `only the ${cancelled} admission waiter is cancelled`,
    { timeout: 3000 },
    async () => {
      const f = fixture();
      const controller = new AbortController();
      const first = f.admission.prepare(
        cancelled === "first" ? controller.signal : undefined,
      );
      const second = f.admission.prepare(
        cancelled === "second" ? controller.signal : undefined,
      );
      const rejected = assert.rejects(
        cancelled === "first" ? first : second,
        /cancelled/,
      );
      controller.abort(new Error("cancelled"));
      await rejected;
      try {
        f.instance.state = "ready";
        f.opened.resolve(f.task);
        await (cancelled === "first" ? second : first);
        await f.admission.prepare();
        assert.equal(f.admission.task, f.task);
        assert.equal(f.taskSignal.aborted, false);
      } finally {
        await f.admission.close();
      }
    },
  );
}

test(
  "caller cancellation covers Scope acquisition without cancelling another waiter",
  { timeout: 3000 },
  async () => {
    const f = fixture("ready");
    const controller = new AbortController();
    const first = f.admission.prepare(controller.signal);
    const second = f.admission.prepare();
    try {
      assert(f.taskSignal);
      controller.abort(new Error("cancelled during scope"));
      await assert.rejects(first, /cancelled during scope/);
      assert.equal(f.taskSignal.aborted, false);
      f.opened.resolve(f.task);
      await second;
      assert.equal(f.admission.task, f.task);
    } finally {
      await f.admission.close();
    }
  },
);

test(
  "closing admission cancels every waiter and its shared Scope operation",
  { timeout: 3000 },
  async () => {
    const f = fixture("ready");
    const first = f.admission.prepare();
    const second = f.admission.prepare();
    const settled = Promise.allSettled([first, second]);
    f.taskSignal.addEventListener(
      "abort",
      () => f.opened.reject(f.taskSignal.reason),
      { once: true },
    );
    await f.admission.close();
    assert.equal(f.taskSignal.aborted, true);
    assert((await settled).every((result) => result.status === "rejected"));
  },
);
