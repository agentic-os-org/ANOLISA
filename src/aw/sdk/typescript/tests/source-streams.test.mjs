import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { fakeSource } from "./source-fixture.mjs";

test("tool streams use the checked arguments through the first native read", async () => {
  class Request {
    constructor(command) { this.command = command; }
  }
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  await source.withSession({}, async (session) => {
    const caller = new AbortController();
    const tool = {
      async *read(request, signal) {
        assert.equal(signal, caller.signal);
        yield request.command;
      },
    };
    for (const request of [{ command: "draft" }, new Request("draft")]) {
      let extracts = 0;
      const extractInput = (value) => {
        if (++extracts === 2) queueMicrotask(() => { request.command = "revised"; });
        return { command: value.command };
      };
      const options = { extractInput, extractSignal: (_, signal) => signal, stream: true };
      if (request instanceof Request) {
        for (const snapshotArg of [undefined, (value) => value]) {
          const stream = session.wrapToolStream(tool.read, { ...options, snapshotArg });
          await assert.rejects(stream(request, caller.signal).next(), {
            code: "source_input_snapshot_required",
          });
        }
        options.snapshotArg = (value) => new Request(value.command);
      }
      const instrument = session.instrument(tool, { toolMethods: { read: options } });
      const items = await Array.fromAsync(tool.read(request, caller.signal));
      instrument.restore();
      assert.equal(request.command, "revised");
      assert.deepEqual(items, ["draft"]);
    }
  });
  assert.deepEqual(tasks[0].events.filter(([event]) => event === "tool.before")
    .map(([, payload]) => payload.input), [{ command: "draft" }, { command: "draft" }]);
});

test("tool streams recheck the admitted snapshot before opening the native stream", async () => {
  const { source } = fakeSource(async () => ({ decision: "continue" }));
  await source.withSession({}, async (session) => {
    let extracts = 0;
    const stream = session.wrapToolStream(async function* read() {
      assert.fail("native stream opened with a changed snapshot");
    }, {
      extractInput: (request) => {
        if (++extracts === 2) queueMicrotask(() => { request.command = "revised"; });
        return { command: request.command };
      },
    });
    await assert.rejects(
      stream({ command: "draft" }).next(),
      (error) => error.code === "source_input_mutated" && !error.toolExecuted,
    );
  });
});

test("tool arguments with custom array behavior require a snapshot callback", async () => {
  class Request extends Array {}
  const { source } = fakeSource(async () => assert.fail("admission ran"));
  await source.withSession({}, async (session) => {
    const wrapped = session.wrapTool(() => assert.fail("tool ran"), { name: "read" });
    await assert.rejects(wrapped(new Request("draft")), {
      code: "source_input_snapshot_required",
    });
  });
});

test("close reports cleanup errors from interrupted native reads", { timeout: 3000 }, async () => {
  for (const stop of ["caller", "session", "both"]) {
    const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
    const session = source.session();
    const caller = new AbortController();
    const abort = new Error("caller aborted");
    const cleanup = new Error("native cleanup failed");
    const entered = Promise.withResolvers();
    // In the combined case the caller stops the consumer, then the native read
    // responds cooperatively to the session's later and distinct signal.
    const nativeSignal = stop === "caller" ? caller.signal : session.signal;
    const wrap = session.wrapToolStream(async function* read(signal) {
      try {
        entered.resolve();
        await new Promise((_, reject) => signal.addEventListener(
          "abort", () => reject(signal.reason), { once: true },
        ));
      } finally {
        assert.equal(tasks[0].closed, false);
        if (stop !== "both") throw cleanup;
      }
    }, { extractSignal: () => caller.signal });
    const read = wrap(nativeSignal).next();
    await entered.promise;
    const rejection = assert.rejects(read, (error) =>
      stop === "session" ? error === session.signal.reason : error === abort,
    );
    if (stop !== "session") {
      caller.abort(abort);
      await rejection;
    }
    if (stop === "both") await session.close();
    else await assert.rejects(session.close(), (error) => error === cleanup);
    await rejection;
    assert.equal(tasks[0].closed, true);
  }
});

test("close does not report an already delivered native read error", async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  const failure = new Error("native read failed");
  const session = source.session();
  const stream = session.wrapToolStream(async function* read() { throw failure; })();
  await assert.rejects(stream.next(), (error) => error === failure);
  await session.close();
  assert.equal(tasks[0].closed, true);
});

test("cancelled stream factories report independent cleanup errors", { timeout: 3000 }, async () => {
  for (const wrapper of ["wrapToolStream", "wrapInputStream"]) {
    const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
    const session = source.session();
    const caller = new AbortController();
    const callerOwnsStop = wrapper === "wrapInputStream";
    const signal = callerOwnsStop ? caller.signal : session.signal;
    const entered = Promise.withResolvers();
    const failure = new Error("native factory cleanup failed");
    const wrapped = session[wrapper](async function factory() {
      entered.resolve();
      try {
        await delay(60_000, undefined, { signal, ref: false });
      } finally {
        assert.equal(tasks[0].closed, false);
        throw failure;
      }
    }, { extractSignal: () => caller.signal });
    const read = wrapped("prompt").next();
    await entered.promise;
    const interrupted = assert.rejects(read, (error) => error === signal.reason);
    if (callerOwnsStop) caller.abort(new Error("caller aborted"));
    await assert.rejects(session.close(), (error) => error === failure);
    await interrupted;
    assert.equal(tasks[0].closed, true);
    assert.equal(tasks[0].events.some(([event]) => event === "tool.after"), false);
    await source.close();
  }
});

test("factory and read cancellation recognize direct and wrapped abort reasons", { timeout: 3000 }, async () => {
  for (const phase of ["direct", "wrapped", "read"]) {
    const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
    const session = source.session();
    const signal = session.signal;
    const entered = Promise.withResolvers();
    async function* chunks() {
      entered.resolve();
      await delay(60_000, undefined, { signal, ref: false });
      yield "late";
    }
    const stream = session.wrapToolStream(async function factory() {
      if (phase === "read") return chunks();
      entered.resolve();
      if (phase === "direct") {
        await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
        signal.throwIfAborted();
      }
      await delay(60_000, undefined, { signal, ref: false });
      return chunks();
    })();
    const read = stream.next();
    await entered.promise;
    const interrupted = assert.rejects(read, (error) => error === signal.reason);
    await session.close();
    await interrupted;
    assert.equal(tasks[0].closed, true);
    await source.close();
  }
});

test("close does not repeat a factory error already delivered to the consumer", async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  const session = source.session();
  const error = new Error("factory failed");
  const stream = session.wrapToolStream(async function factory() { throw error; })();
  await assert.rejects(stream.next(), (failure) => failure === error);
  await session.close();
  assert.equal(tasks[0].closed, true);
  await source.close();
});

test("close retains the Task until a late iterator is available for cleanup", { timeout: 3000 }, async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  const session = source.session({ timeoutMs: 100 });
  const entered = Promise.withResolvers();
  const release = Promise.withResolvers();
  let returned = false;
  const stream = session.wrapToolStream(async function factory() {
    entered.resolve();
    await release.promise;
    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      async next() {
        assert.fail("cancelled iterator read");
      },
      async return() {
        assert.equal(tasks[0].closed, false);
        returned = true;
        return { done: true };
      },
    };
  })();
  const read = stream.next();
  await entered.promise;
  const interrupted = assert.rejects(read, { code: "source_session_closed" });
  try {
    await assert.rejects(session.close(), { code: "source_close_pending" });
    assert.equal(tasks[0].closed, false);
  } finally {
    release.resolve();
    await session.close();
    await source.close();
  }
  await interrupted;
  assert.equal(returned, true);
  assert.equal(tasks[0].closed, true);
});

test("stream extractResult receives native chunks while isolating plain JSON chunks", async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  await source.withSession({}, async (session) => {
    const wrappedBigInt = session.wrapToolStream(
      async function* bigints() {
        yield 1n;
        yield 2n;
      },
      { name: "bigints", extractResult: (items) => items[0] + items[1] },
    );
    const seen = [];
    for await (const chunk of wrappedBigInt({ k: 1 })) seen.push(chunk);
    assert.deepEqual(seen, [1n, 2n]);
    assert.equal(tasks[0].events.at(-1)[0], "tool.after");
    assert.equal(tasks[0].events.at(-1)[1].result, "3");

    const wrappedObjects = session.wrapToolStream(
      async function* objs() {
        yield { v: "a" };
        yield { v: "b" };
      },
      { name: "objs", extractResult: (items) => items.map((item) => item.v) },
    );
    for await (const item of wrappedObjects({ k: 2 })) item.v = "mutated";
    assert.deepEqual(tasks[0].events.at(-1)[1].result, ["a", "b"]);
  });
});

test("plain objects with optional undefined properties snapshot cleanly", async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  await source.withSession({}, async (session) => {
    const ask = session.wrapInput(async (req) => ({ text: req.text, hasTurn: "turnId" in req, turnId: req.turnId }));
    const run = session.wrapTool(async (args) => ({ cmd: args.command, hasCwd: "cwd" in args, cwd: args.cwd }), { name: "sh" });
    assert.deepEqual(await ask({ text: "hello", turnId: undefined }), {
      text: "hello",
      hasTurn: true,
      turnId: undefined,
    });
    assert.deepEqual(await run({ command: "ls", cwd: undefined }), {
      cmd: "ls",
      hasCwd: true,
      cwd: undefined,
    });
  });
  assert.equal(tasks[0].events[0][0], "input.submit");
  assert.equal(tasks[0].events[0][1].text, "hello");
  assert.deepEqual(tasks[0].events[1][1].input, { command: "ls" });
});
