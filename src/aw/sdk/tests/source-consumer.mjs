/** Out-of-tree Node agent proving observation and blocking with only core @anolisa/aw-sdk. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { LanguageSource, SourceDenied } from "@anolisa/aw-sdk/sources";

class BrandNewJsAgent {
  constructor() {
    this.prompts = [];
    this.toolRuns = [];
    this.streamClosed = 0;
  }
  async ask(prompt, command) {
    this.prompts.push(prompt);
    return this.shell({ command });
  }
  shell(args) {
    const cmd = args.command;
    this.toolRuns.push(cmd);
    args.command = "mutated-in-tool";
    if (cmd === "boom") throw new Error("tool_exploded");
    return `ran:${cmd}`;
  }
  async *stream() {
    try {
      yield "part-1";
      yield "part-2";
    } finally {
      this.streamClosed++;
    }
  }
}

async function checkToolSnapshots(session) {
  class Request {
    constructor(command) { this.command = command; }
  }
  for (const request of [{ command: "draft" }, new Request("draft")]) {
    const entered = Promise.withResolvers();
    const release = Promise.withResolvers();
    async function* readTool(value) {
      entered.resolve();
      await release.promise;
      yield value.command;
    }
    const options = { extractInput: (value) => ({ command: value.command }) };
    if (request instanceof Request) {
      await assert.rejects(session.wrapToolStream(readTool, options)(request).next(), {
        code: "source_input_snapshot_required",
      });
      options.snapshotArg = (value) => new Request(value.command);
    }
    const stream = session.wrapToolStream(readTool, options)(request);
    const first = stream.next();
    await entered.promise;
    request.command = "revised";
    release.resolve();
    assert.deepEqual(await first, { done: false, value: "draft" });
    assert.equal((await stream.next()).done, true);
  }
}

async function checkPendingCleanup() {
  const source = LanguageSource.create({ endpoint, target: "app" });
  assert.equal(await source.connect(), true);
  const session = source.session();
  const caller = new AbortController();
  const abort = new Error("caller aborted");
  const release = Promise.withResolvers();
  const cleanup = new Error("native cleanup failed");
  const entered = Promise.withResolvers();
  const wrapped = session.wrapToolStream(async function* pendingTool(signal) {
    try {
      entered.resolve();
      await new Promise((_, reject) =>
        signal.addEventListener("abort", () => reject(signal.reason), { once: true }),
      );
    } finally {
      await release.promise;
      throw cleanup;
    }
  }, { extractInput: () => ({}), extractSignal: (signal) => signal });
  const read = wrapped(caller.signal).next();
  await entered.promise;
  const interrupted = assert.rejects(read, (error) => error === abort);
  caller.abort(abort);
  await interrupted;
  try {
    const closing = source.close({ timeoutMs: 100 });
    assert.throws(() => source.session(), { code: "source_closed" });
    await assert.rejects(closing, { code: "source_close_pending" });
    assert.equal(bindings(), 2);
  } finally {
    release.resolve();
  }
  await assert.rejects(session.close(), (error) => error === cleanup);
  assert.equal(bindings(), 1);
  await assert.rejects(source.close(), (error) => error === cleanup);
  assert.equal(bindings(), 0);
}

async function checkMixedCleanup() {
  const source = LanguageSource.create({ endpoint, target: "app" });
  assert.equal(await source.connect(), true);
  const cleanup = new Error("first cleanup failed");
  const release = Promise.withResolvers();
  let failed = false;

  async function* chunks() {
    try {
      yield "ready";
    } finally {
      if (!failed) {
        failed = true;
        throw cleanup;
      }
      await release.promise;
    }
  }

  for (let i = 0; i < 2; i++) {
    const stream = source.session().wrapToolStream(chunks, { extractInput: () => ({}) })();
    assert.deepEqual(await stream.next(), { value: "ready", done: false });
  }
  assert.equal(bindings(), 3);
  try {
    await assert.rejects(source.close({ timeoutMs: 100 }), { code: "source_close_pending" });
    assert.equal(bindings(), 2);
  } finally {
    release.resolve();
  }
  await assert.rejects(source.close(), (error) => error === cleanup);
  assert.equal(bindings(), 0);
}

const [endpoint, missingEndpoint, binary] = process.argv.slice(2);
function bindings() {
  const status = execFileSync(binary, ["service", "status", "--socket", endpoint], {
    encoding: "utf8", timeout: 5000,
  });
  return JSON.parse(status).result.bindings;
}

async function checkCallDrain(source) {
  for (const timeOut of [false, true]) {
    const session = source.session({ timeoutMs: timeOut ? 100 : 5000 });
    const entered = Promise.withResolvers();
    const release = Promise.withResolvers();
    const wrapped = session.wrapTool(async function pausedTool(input) {
      entered.resolve();
      await release.promise;
      return input.command;
    });
    const call = wrapped({ command: "held" });
    await entered.promise;
    const closing = session.close();
    try {
      assert.equal(bindings(), 2);
      if (timeOut) {
        await assert.rejects(closing, { code: "source_close_pending" });
        assert.equal(bindings(), 2);
      }
    } finally {
      release.resolve();
    }
    const [result] = await Promise.all([
      call,
      timeOut ? session.close({ timeoutMs: 5000 }) : closing,
    ]);
    assert.equal(result, "held");
    assert.equal(bindings(), 1);
  }
}

async function checkInstrumentedDrain(source) {
  const entered = Promise.withResolvers();
  const release = Promise.withResolvers();
  const target = {
    toolRuns: [],
    async ask(prompt, command) {
      void prompt;
      entered.resolve();
      await release.promise;
      return this.shell({ command });
    },
    async shell(args) {
      this.toolRuns.push(args.command);
      return `ran:${args.command}`;
    },
  };
  const session = source.session();
  const handle = session.instrument(target, {
    inputMethods: ["ask"],
    toolMethods: { shell: "shell" },
  });
  await assert.rejects(
    target.shell({ command: "deny" }),
    (error) =>
      error instanceof SourceDenied &&
      error.event === "tool.before" &&
      error.toolName === "shell",
  );
  const call = target.ask("drain-input", "deny");
  call.catch(() => {});
  await entered.promise;
  try {
    await assert.rejects(session.close({ timeoutMs: 100 }), {
      code: "source_close_pending",
    });
    assert.equal(bindings(), 2);
    assert.equal(handle.restored, false);
  } finally {
    release.resolve();
  }
  await assert.rejects(call, { code: "source_session_closed" });
  assert.deepEqual(target.toolRuns, []);
  await session.close();
  assert.equal(handle.restored, true);
  assert.equal(bindings(), 1);
}

async function checkFactoryCleanup(source) {
  const session = source.session();
  const signal = session.signal;
  const entered = Promise.withResolvers();
  const cleanup = new Error("native factory cleanup failed");
  const wrapped = session.wrapToolStream(async function pendingFactory() {
    entered.resolve();
    try {
      await delay(60_000, undefined, { signal, ref: false });
    } finally {
      throw cleanup;
    }
  }, { extractInput: () => ({}) });
  const read = wrapped().next();
  await entered.promise;
  assert.equal(bindings(), 2);
  const interrupted = assert.rejects(read, { code: "source_session_closed" });
  await assert.rejects(session.close(), (error) => error === cleanup);
  await interrupted;
  assert.equal(bindings(), 1);
}

await checkPendingCleanup();
await checkMixedCleanup();
const agent = new BrandNewJsAgent();
const source = LanguageSource.create({
  endpoint,
  target: "app",
  availability: "required",
});
try {
  assert.equal(await source.connect(), true);
  let handle;
  await source.withSession({ sessionId: "nth-js-agent" }, async (session) => {
    handle = session.instrument(agent, {
      inputMethods: ["ask"],
      toolMethods: { shell: "shell", stream: "stream" },
    });
    class Prompt {
      constructor(text) { this.text = text; }
    }
    const request = new Prompt("draft");
    const entered = Promise.withResolvers();
    const release = Promise.withResolvers();
    async function* read(prompt) {
      entered.resolve();
      await release.promise;
      yield prompt.text;
    }
    const extractText = (prompt) => prompt.text;
    await assert.rejects(session.wrapInput(read, { extractText })(request).next(), {
      code: "source_input_snapshot_required",
    });
    const stream = session.wrapInput(read, {
      extractText,
      snapshotArg: (prompt) => new Prompt(prompt.text),
    })(request);
    const first = stream.next();
    await entered.promise;
    request.text = "revised";
    release.resolve();
    assert.deepEqual(await first, { done: false, value: "draft" });
    await stream.return();
    await checkToolSnapshots(session);

    // 1. Blocked input never runs the agent or tools.
    await assert.rejects(
      agent.ask("deny", "echo"),
      (error) => error instanceof SourceDenied && error.event === "input.submit",
    );
    assert.deepEqual(agent.prompts, []);
    assert.deepEqual(agent.toolRuns, []);

    // 2. Allowed input with blocked tool never executes the tool.
    await assert.rejects(
      agent.ask("allow-then-deny-tool", "deny"),
      (error) =>
        error instanceof SourceDenied &&
        error.event === "tool.before" &&
        error.toolName === "shell",
    );
    assert.deepEqual(agent.prompts, ["allow-then-deny-tool"]);
    assert.deepEqual(agent.toolRuns, []);

    // 3. Allowed input + tool executes once and emits tool.after with original snapshot.
    assert.equal(await agent.ask("allow-all", "echo"), "ran:echo");
    assert.deepEqual(agent.toolRuns, ["echo"]);

    // 4. Failing tool executes once, propagates error, skips tool.after.
    await assert.rejects(agent.shell({ command: "boom" }), /tool_exploded/);
    assert.deepEqual(agent.toolRuns, ["echo", "boom"]);

    // 5. Early-broken async generator closes inner stream and skips tool.after.
    const seen = [];
    for await (const item of agent.stream({ mode: "early" })) {
      seen.push(item);
      break;
    }
    assert.deepEqual(seen, ["part-1"]);
    assert.equal(agent.streamClosed, 1);

    // 6. Fully-consumed async generator emits tool.after once.
    const full = [];
    for await (const item of agent.stream({ mode: "full" })) full.push(item);
    assert.deepEqual(full, ["part-1", "part-2"]);
    assert.equal(agent.streamClosed, 2);
  });
  assert.equal(handle.restored, true);
  await checkCallDrain(source);
  await checkInstrumentedDrain(source);
  await checkFactoryCleanup(source);

  // 6b. Narrowed session cannot bypass admitted required tool.before route.
  await source.withSession(
    { sessionId: "nth-js-narrow", events: ["tool.after"] },
    async (narrow) => {
      const wrappedNarrow = narrow.wrapTool(agent.shell.bind(agent), {
        name: "shell",
      });
      await assert.rejects(
        wrappedNarrow({ command: "bypass" }),
        (error) => error.code === "source_route_coverage_incomplete",
      );
    },
  );
  assert.deepEqual(agent.toolRuns, ["echo", "boom"]);
} finally {
  await source.close();
}

// 7. Restored method runs natively; native_fallback runs natively without AW.
assert.equal(agent.shell({ command: "plain" }), "ran:plain");
const fallbackSource = LanguageSource.create({
  endpoint: missingEndpoint,
  target: "app",
  availability: "native_fallback",
  events: ["tool.before", "tool.after"],
});
try {
  assert.equal(await fallbackSource.connect(), false);
  await fallbackSource.withSession(
    { sessionId: "nth-js-fallback" },
    async (session) => {
      session.instrument(agent, { toolMethods: { shell: "shell" } });
      assert.equal(await agent.shell({ command: "offline" }), "ran:offline");
      assert.equal(session.fallback, true);
    },
  );
} finally {
  await fallbackSource.close();
}
