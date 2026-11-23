import test from "node:test";
import assert from "node:assert/strict";
import { AwError, Instance } from "../src/client.mjs";
import {
  LanguageSource,
  SourceDenied,
  confirmSourceRoutes,
  sourceCapabilities,
} from "../src/sources.mjs";

import { fakeSource } from "./source-fixture.mjs";

test("sourceCapabilities and confirmSourceRoutes enforce aw-hook-source/v1", () => {
  const caps = sourceCapabilities(["tool.before", "tool.after", "tool.before"]);
  assert.equal(caps.adapter, "sdk");
  assert.equal(caps.version, "aw-hook-source/v1");
  assert.deepEqual(Object.keys(caps.events), ["tool.before", "tool.after"]);
  assert.equal(
    confirmSourceRoutes(caps, {
      hooks: [{ event: "tool.before" }, { event: "tool.after" }],
    }),
    true,
  );
  assert.equal(
    confirmSourceRoutes(caps, { hooks: [{ event: "input.submit" }] }),
    false,
  );
  assert.equal(
    confirmSourceRoutes(caps, {
      hooks: [{ event: "tool.before", replace_input: true }],
    }),
    false,
  );
  assert.equal(
    confirmSourceRoutes(caps, {
      hooks: [{ event: "tool.after", replace_result: true }],
    }),
    false,
  );
  assert.throws(
    () => sourceCapabilities([]),
    (error) => error.code === "semantic_event_unavailable",
  );
  assert.throws(
    () => sourceCapabilities(["agent.end"]),
    (error) => error.code === "semantic_event_unavailable",
  );
});

test("instrument observes, blocks with zero side effects and restores on close", async () => {
  const { source, tasks } = fakeSource(async (event, payload) => {
    if (event === "input.submit" && payload.text === "block-input")
      return { decision: "block", coverage: "checked" };
    if (event === "tool.before" && payload.input.cmd === "rm")
      return { decision: "block", coverage: "checked" };
    return { decision: "continue", coverage: "checked" };
  });
  const runs = [];
  class Agent {
    async ask(prompt) {
      runs.push(`ask:${prompt}`);
      return this.runTool({ cmd: prompt });
    }
    runTool(payload) {
      runs.push(`tool:${payload.cmd}`);
      payload.cmd = "mutated";
      return "ok";
    }
  }
  const agent = new Agent();
  let handle;
  await source.withSession({ sessionId: "sess-1" }, async (session) => {
    handle = session.instrument(agent, {
      inputMethods: ["ask"],
      toolMethods: { runTool: "shell" },
    });
    await assert.rejects(
      agent.ask("block-input"),
      (error) =>
        error instanceof SourceDenied &&
        error.code === "source_denied" &&
        error.event === "input.submit",
    );
    assert.deepEqual(runs, []);

    await assert.rejects(
      agent.ask("rm"),
      (error) =>
        error instanceof SourceDenied &&
        error.event === "tool.before" &&
        error.toolName === "shell",
    );
    assert.deepEqual(runs, ["ask:rm"]);

    assert.equal(await agent.ask("ls"), "ok");
    assert.deepEqual(runs, ["ask:rm", "ask:ls", "tool:ls"]);
    assert.equal(handle.restored, false);
  });

  assert.equal(handle.restored, true);
  assert.equal(tasks.length, 1);
  assert.equal(tasks[0].closed, true);
  assert.deepEqual(
    tasks[0].events.map(([event]) => event),
    [
      "input.submit",
      "input.submit",
      "tool.before",
      "input.submit",
      "tool.before",
      "tool.after",
    ],
  );
  assert.deepEqual(tasks[0].events.at(-1)[1].input, { cmd: "ls" });
  assert.equal(tasks[0].events.at(-1)[1].result, "ok");
  assert.equal(agent.runTool({ cmd: "post" }), "ok");
});

test("tool exception, early async generator exit and AbortSignal preserve single side effect", async () => {
  const { source, tasks } = fakeSource(async (event, payload) => {
    if (event === "tool.before" && payload.input.deny)
      return { decision: "block", coverage: "checked" };
    return { decision: "continue", coverage: "checked" };
  });
  let calls = 0;
  let closedStreams = 0;
  async function failingTool() {
    calls++;
    throw new Error("boom");
  }
  async function* streamingTool() {
    calls++;
    try {
      yield "chunk-1";
      yield "chunk-2";
    } finally {
      closedStreams++;
    }
  }
  await source.withSession({ sessionId: "sess-2" }, async (session) => {
    const wrappedFail = session.wrapTool(failingTool, { name: "fail" });
    await assert.rejects(wrappedFail({ k: "v" }), /boom/);
    assert.equal(calls, 1);

    const wrappedStream = session.wrapTool(streamingTool, { name: "stream" });
    await assert.rejects(async () => {
      for await (const _ of wrappedStream({ deny: true })) {
      }
    }, SourceDenied);
    assert.equal(calls, 1);
    assert.equal(closedStreams, 0);

    const seen = [];
    for await (const item of wrappedStream({ deny: false })) {
      seen.push(item);
      break;
    }
    assert.deepEqual(seen, ["chunk-1"]);
    assert.equal(closedStreams, 1);

    const full = [];
    for await (const item of wrappedStream({ full: true })) full.push(item);
    assert.deepEqual(full, ["chunk-1", "chunk-2"]);
    assert.equal(closedStreams, 2);

    const aborted = new AbortController();
    aborted.abort(new Error("user_abort"));
    await assert.rejects(
      session.beforeTool("fail", { a: 1 }, { signal: aborted.signal }),
      /user_abort/,
    );
  });
  assert.deepEqual(
    tasks[0].events.map(([event, payload]) => [event, payload.name]),
    [
      ["tool.before", "fail"],
      ["tool.before", "stream"],
      ["tool.before", "stream"],
      ["tool.before", "stream"],
      ["tool.after", "stream"],
    ],
  );
  assert.deepEqual(tasks[0].events.at(-1)[1].result, ["chunk-1", "chunk-2"]);
});

test("instrument rejects conflicts, double patching and rolls back partial failures", async () => {
  const { source } = fakeSource(async () => ({
    decision: "continue",
    coverage: "checked",
  }));
  class Sample {
    async a(text) {
      return text;
    }
  }
  const obj = new Sample();
  await source.withSession({ sessionId: "sess-3" }, async (session) => {
    assert.throws(
      () => session.instrument(obj, { inputMethods: ["a"], toolMethods: ["a"] }),
      (error) => error.code === "source_method_conflict",
    );
    assert.throws(
      () =>
        session.instrument(obj, {
          inputMethods: ["a"],
          toolMethods: ["missing"],
        }),
      (error) => error.code === "source_attribute_missing",
    );
    assert.equal(Object.hasOwn(obj, "a"), false);
    session.instrument(obj, { inputMethods: ["a"] });
    assert.throws(
      () => session.instrument(obj, { inputMethods: ["a"] }),
      (error) => error.code === "source_already_instrumented",
    );
  });
});

test("validation, closed session and native_fallback work without AW", async () => {
  const nonSdk = new Instance({
    endpoint: "/tmp/aw.sock",
    target: "app",
    availability: "required",
    capabilities: {
      adapter: "qoder-sdk",
      version: "public-hooks/v1",
      entrypoint: "sdk",
      events: { "tool.before": ["observe"] },
    },
    install: () => true,
  });
  assert.throws(
    () => new LanguageSource(nonSdk),
    (error) => error instanceof AwError && error.code === "semantic_profile_required",
  );

  const source = LanguageSource.create({
    endpoint: "/tmp/definitely-missing-aw-source.sock",
    target: "app",
    availability: "native_fallback",
    events: ["tool.before", "tool.after"],
  });
  try {
    assert.equal(await source.connect(), false);
    await source.withSession({ sessionId: "fallback-1" }, async (session) => {
      await assert.rejects(
        session.submit("hello"),
        (error) => error.code === "semantic_event_unavailable",
      );
      await assert.rejects(
        session.beforeTool("t", { bad: Number.NaN }),
        (error) => error.code === "source_invalid_payload",
      );
      const cyclic = {};
      cyclic.self = cyclic;
      await assert.rejects(
        session.beforeTool("t", cyclic),
        (error) => error.code === "source_invalid_payload",
      );
      let calls = 0;
      const wrapped = session.wrapTool(
        async (args) => {
          calls++;
          return args.v;
        },
        { name: "echo" },
      );
      assert.equal(await wrapped({ v: "native" }), "native");
      assert.equal(session.fallback, true);
      assert.equal(calls, 1);
    });
    const closedSession = source.session({ sessionId: "closed-1" });
    await closedSession.close();
    await assert.rejects(
      closedSession.prepare(),
      (error) => error.code === "source_session_closed",
    );
  } finally {
    await source.close();
  }
});

test("narrowed session cannot skip admitted tool.before route and concurrent mutation aborts", async () => {
  const { source, tasks } = fakeSource(async () => ({
    decision: "continue",
    coverage: "checked",
  }));
  const origTask = source.instance.task;
  source.instance.task = async () => {
    const task = await origTask();
    task.routes = {
      events: {},
      hooks: [{ event: "tool.before", step: "policy" }],
    };
    return task;
  };
  let ran = 0;
  await source.withSession(
    { sessionId: "narrow", events: ["tool.after"] },
    async (session) => {
      const wrapped = session.wrapTool(
        async () => {
          ran++;
          return "ok";
        },
        { name: "sh" },
      );
      await assert.rejects(
        wrapped({ cmd: "id" }),
        (error) => error.code === "source_route_coverage_incomplete",
      );
    },
  );
  assert.equal(ran, 0);
  assert.equal(tasks[0].events.length, 0);

  const shared = { cmd: "safe" };
  let checked;
  const { source: mutSource } = fakeSource(async (event) => {
    if (event === "tool.before") checked.cmd = "evil";
    return { decision: "continue", coverage: "checked" };
  });
  let mutRuns = 0;
  await mutSource.withSession({ sessionId: "toctou" }, async (session) => {
    const wrapped = session.wrapTool(
      async (args) => {
        mutRuns++;
        return args.cmd;
      },
      {
        name: "sh",
        extractInput: (args) => {
          checked = args;
          return { cmd: args.cmd };
        },
      },
    );
    await assert.rejects(
      wrapped(shared),
      (error) => error.code === "source_input_mutated",
    );
  });
  assert.equal(mutRuns, 0);
  assert.equal(shared.cmd, "safe");
});

test("mid-stream abort, session.close stream teardown, chunk snapshot, __proto__ and after failure", async () => {
  const { source, tasks } = fakeSource(async (event) => {
    if (event === "tool.after") throw new AwError("sdk_step_failed");
    return { decision: "continue", coverage: "checked" };
  });
  let afterRuns = 0;
  await source.withSession({ sessionId: "after-fail" }, async (session) => {
    const wrapped = session.wrapTool(
      async (args) => {
        afterRuns++;
        return args;
      },
      { name: "sh" },
    );
    const protoPayload = JSON.parse('{"__proto__":{"a":1},"ok":true}');
    await assert.rejects(
      wrapped(protoPayload),
      (error) => error.code === "sdk_step_failed" && error.toolExecuted === true,
    );
  });
  assert.equal(afterRuns, 1);
  assert.deepEqual(
    Object.getOwnPropertyDescriptor(tasks[0].events[0][1].input, "__proto__")?.value,
    { a: 1 },
  );

  // Mid-stream abort and session.close teardown + chunk isolation + stream factory.
  const { source: s2, tasks: t2 } = fakeSource(async () => ({
    decision: "continue",
    coverage: "checked",
  }));
  let closed = 0;
  function makeStream() {
    return (async function* () {
      try {
        yield { value: "first" };
        yield { value: "second" };
      } finally {
        closed++;
      }
    })();
  }
  const abortController = new AbortController();
  await s2.withSession({ sessionId: "abort-stream" }, async (session) => {
    const wrapped = session.wrapToolStream(makeStream, {
      name: "st",
      extractSignal: () => abortController.signal,
    });
    const iter = wrapped({ k: 1 });
    assert.deepEqual((await iter.next()).value, { value: "first" });
    abortController.abort(new Error("mid_abort"));
    await assert.rejects(iter.next(), /mid_abort/);
  });
  assert.equal(closed, 1);
  assert.equal(t2[0].events.filter(([e]) => e === "tool.after").length, 0);

  const sessionClose = s2.session({ sessionId: "close-stream" });
  const wrappedClose = sessionClose.wrapToolStream(makeStream, { name: "st" });
  const held = wrappedClose({ k: 2 });
  assert.deepEqual((await held.next()).value, { value: "first" });
  await sessionClose.close();
  assert.equal(closed, 2);
  assert.equal(t2[1].closed, true);
  await assert.rejects(
    held.next(),
    (error) => error.code === "source_session_closed",
  );

  await s2.withSession({ sessionId: "chunk-snapshot" }, async (session) => {
    const wrapped = session.wrapToolStream(makeStream, {
      name: "st",
      maxStreamItems: 2,
    });
    for await (const chunk of wrapped({ k: 3 }))
      chunk.value = "mutated-by-consumer";
  });
  assert.deepEqual(t2[2].events.at(-1)[1].result, [
    { value: "first" },
    { value: "second" },
  ]);
});

test("instrument rejects accessors and closes Admission even if restore fails on frozen target", async () => {
  const { source, tasks } = fakeSource(async () => ({
    decision: "continue",
    coverage: "checked",
  }));
  const withSetter = {
    _fn: () => "orig",
    get run() {
      return this._fn;
    },
    set run(_v) {},
  };
  await source.withSession({ sessionId: "accessor" }, async (session) => {
    assert.throws(
      () => session.instrument(withSetter, { toolMethods: ["run"] }),
      (error) => error.code === "source_not_patchable",
    );
  });

  const target = { run: (x) => x.v };
  const session = source.session({ sessionId: "frozen-close" });
  await session.prepare();
  session.instrument(target, { toolMethods: ["run"] });
  Object.freeze(target);
  await assert.rejects(session.close(), TypeError);
  assert.equal(tasks.at(-1).closed, true);
});

test("input dispatch and admission share a snapshot for calls and streams", async () => {
  const request = { text: "draft" };
  const { source } = fakeSource(async (_, payload) => {
    assert.equal(payload.text, "draft");
    request.text = "revised";
    return { decision: "continue", coverage: "checked" };
  });
  await source.withSession({}, async (session) => {
    assert.equal(await session.wrapInput((arg) => arg.text)(request), "draft");
    request.text = "draft";
    const wrapped = session.wrapInput(async function* (arg) {
      yield arg.text;
    });
    const values = [];
    for await (const text of wrapped(request)) values.push(text);
    assert.deepEqual(values, ["draft"]);
  });
});

test(
  "close and abort interrupt pending reads and retain the Task until native exit",
  { timeout: 3000 },
  async () => {
    for (const kind of ["wrapTool", "wrapInput"]) {
      for (const cooperative of [true, false]) {
        const { source, tasks } = fakeSource(async () => ({
          decision: "continue",
        }));
        const session = source.session({ timeoutMs: 100 });
        const caller = new AbortController();
        const entered = Promise.withResolvers();
        const release = Promise.withResolvers();
        let exited = false;
        const read = session[kind](
          async function* read() {
            try {
              entered.resolve();
              if (cooperative) {
                await new Promise((_, reject) =>
                  session.signal.addEventListener(
                    "abort",
                    () => reject(session.signal.reason),
                    { once: true },
                  ),
                );
              } else {
                await release.promise;
              }
              yield "late";
            } finally {
              assert.equal(tasks[0].closed, false);
              exited = true;
            }
          },
          { extractSignal: () => caller.signal },
        );
        const reading = read("prompt").next();
        await entered.promise;
        const interrupted = assert.rejects(
          reading,
          cooperative ? /source_session_closed/ : /caller_abort/,
        );
        if (cooperative) {
          await session.close();
        } else {
          caller.abort(new Error("caller_abort"));
          await interrupted;
          await assert.rejects(source.close(), {
            code: "source_close_pending",
          });
          assert.equal(tasks[0].closed, false);
          assert.equal(source.instance.closed, false);
          release.resolve();
          await session.close();
        }
        await interrupted;
        assert.equal(exited, true);
        assert.equal(tasks[0].closed, true);
        assert.equal(
          tasks[0].events.some(([event]) => event === "tool.after"),
          false,
        );
        await source.close();
      }
    }
  },
);

test("result collection follows the admitted after route and skips native fallback", async () => {
  const cyclic = {};
  cyclic.self = cyclic;
  for (const fallback of [false, true]) {
    const { source, tasks } = fakeSource(
      async () => ({ decision: "continue" }),
      {
        events: ["tool.before", "tool.after"],
        fallback,
        availability: "native_fallback",
      },
    );
    await source.withSession({}, async (session) => {
      await session.prepare();
      if (!fallback) tasks[0].routes.hooks = [{ event: "tool.before" }];
      const options = {
        name: "tool",
        maxStreamItems: 1,
        extractResult: assert.fail,
      };
      assert.equal(await session.wrapTool(() => cyclic, options)(), cyclic);
      const stream = session.wrapTool(async function* () {
        yield cyclic;
        yield cyclic;
      }, options);
      const seen = [];
      for await (const item of stream()) seen.push(item);
      assert.equal(seen.length, 2);
      assert.ok(seen.every((item) => item === cyclic));
      assert.equal(
        tasks[0].events.some(([event]) => event === "tool.after"),
        false,
      );
    });
  }
});

test("errors during result extraction and chunk collection carry the execution flag", async () => {
  const { source } = fakeSource(async () => ({ decision: "continue" }));
  let calls = 0;
  function tool() {
    calls++;
    return 1;
  }
  async function* chunks() {
    calls++;
    yield 1;
    yield 2;
  }
  await source.withSession({}, async (session) => {
    const original = new Error("extract failed");
    const options = {
      extractResult() {
        throw original;
      },
    };
    const isOriginal = (error) =>
      error === original && error.toolExecuted === true;
    await assert.rejects(session.wrapTool(tool, options)(), isOriginal);
    delete original.toolExecuted;
    async function drain(stream) {
      for await (const _ of stream) {
      }
    }
    await assert.rejects(
      drain(session.wrapTool(chunks, options)()),
      isOriginal,
    );
    await assert.rejects(
      drain(session.wrapTool(chunks, { maxStreamItems: 1 })()),
      {
        code: "source_stream_result_too_large",
        toolExecuted: true,
      },
    );
    assert.equal(calls, 3);
  });
});

test("session close reports native cleanup failure after caller abort between reads", async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  const session = source.session();
  const caller = new AbortController();
  const cleaned = Promise.withResolvers();
  const failure = new Error("native cleanup failed");
  const wrapped = session.wrapInputStream(async function* () {
    try {
      yield "first";
    } finally {
      cleaned.resolve();
      throw failure;
    }
  }, { extractSignal: () => caller.signal });
  const stream = wrapped("prompt");
  assert.equal((await stream.next()).value, "first");
  caller.abort();
  await cleaned.promise;
  await new Promise((resolve) => setImmediate(resolve));
  await assert.rejects(session.close(), (error) => error === failure);
  assert.equal(tasks[0].closed, true);
  await stream.return();
});

test("a cooperative async tool aborts without emitting tool.after during close", { timeout: 3000 }, async () => {
  const { source, tasks } = fakeSource(async () => ({ decision: "continue" }));
  const session = source.session();
  const entered = Promise.withResolvers();
  const tool = session.wrapTool(async function native(signal) {
    entered.resolve();
    await new Promise((_, reject) =>
      signal.addEventListener("abort", () => reject(signal.reason), { once: true }),
    );
  });
  const call = tool(session.signal);
  await entered.promise;
  const interrupted = assert.rejects(call, { code: "source_session_closed" });
  await session.close();
  await interrupted;
  assert.deepEqual(tasks[0].events.map(([event]) => event), ["tool.before"]);
  assert.equal(tasks[0].closed, true);
  await source.close();
});

test("ordinary generator factories support awaited and explicit stream calls", async () => {
  const { source } = fakeSource(async () => ({ decision: "continue" }));
  async function* chunks(text) { yield text; }
  const factory = (text) => chunks(text);
  await source.withSession({}, async (session) => {
    for (const wrap of ["wrapInput", "wrapTool"]) {
      const result = session[wrap](factory)("prompt");
      assert.ok(result instanceof Promise);
      const native = await result;
      assert.equal((await native.next()).value, "prompt");
      await native.return();
      const stream = session[wrap](factory, { stream: true })("prompt");
      assert.equal((await stream.next()).value, "prompt");
      await stream.return();
    }
  });
});
