import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { MockInstance } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveContextEngine } from "../src/engine.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import type { RuntimeLlmCompleteFn } from "../src/summarize.js";

const FRESH_TURNS = 3;
const CONCURRENCY = 4;
const BATCH_CONCURRENCY = 2;
let opened: MockInstance<typeof connection.createConnection>;

beforeEach(() => {
  opened = vi.spyOn(connection, "createConnection");
  vi.spyOn(console, "error").mockImplementation(() => {});
});
afterEach(() => {
  for (const result of opened.mock.results) {
    if (result.type === "return") connection.closeConnection(result.value as DatabaseSync);
  }
  vi.restoreAllMocks();
});

function history(session: string, count: number): AgentMessage[] {
  return Array.from({ length: count }, (_, index) => [
    { role: "user", content: `${session} question-${index + 1}` },
    { role: "assistant", content: `${session} decision-${index + 1}` },
  ]).flat();
}

function summary(text: string): string {
  return `SUMMARY(${text.replaceAll("\n", " | ")})`;
}

function registerEngine(complete: RuntimeLlmCompleteFn): SelectiveContextEngine {
  let engine: SelectiveContextEngine | undefined;
  activate({
    pluginConfig: { dbPath: ":memory:", freshTailTurns: FRESH_TURNS },
    runtime: { llm: { complete } },
    registerContextEngine(_id, factory) { engine = factory() as SelectiveContextEngine; },
    registerTool() {},
  });
  expect(engine).toBeDefined();
  return engine!;
}

function controlledProvider(capacity = Infinity) {
  const calls: Array<{ text: string; finish?: () => void }> = [];
  const completed: string[] = [];
  let active = 0;
  let peak = 0;
  let rejected = 0;
  const complete: RuntimeLlmCompleteFn = ({ messages }) => {
    const text = messages[0].content;
    const call: (typeof calls)[number] = { text };
    calls.push(call);
    if (active >= capacity) {
      rejected++;
      return Promise.reject(new Error("synthetic concurrent request limit"));
    }
    active++;
    peak = Math.max(peak, active);
    return new Promise<{ text: string }>((resolve) => {
      call.finish = () => {
        call.finish = undefined;
        completed.push(text);
        resolve({ text: summary(text) });
      };
    }).finally(() => { active--; });
  };
  return {
    complete, calls, completed,
    get active() { return active; },
    get peak() { return peak; },
    get rejected() { return rejected; },
    resetPeak() { peak = active; },
    releaseBatch() {
      // Reverse completions to distinguish result order from request completion order.
      for (const call of [...calls].reverse()) call.finish?.();
    },
  };
}

type ControlledProvider = ReturnType<typeof controlledProvider>;

async function drain<T>(operation: Promise<T>, ...providers: ControlledProvider[]): Promise<T> {
  let done = false;
  const observed = operation.then(
    (value) => { done = true; return value; },
    (error) => { done = true; throw error; },
  );
  // Attach a handler immediately; the original rejection is rethrown below.
  void observed.catch(() => {});
  for (let step = 0; step < 200 && !done; step++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
    for (const provider of providers) provider.releaseBatch();
  }
  await new Promise<void>((resolve) => setImmediate(resolve));
  expect(done, "queued summaries must finish after every provider gate is released").toBe(true);
  return observed;
}

function expectedSummaries(session: string, turns: number) {
  return Array.from({ length: Math.max(0, turns - FRESH_TURNS) }, (_, index) => ({
    turnSeq: index + 1,
    summary: summary(`user: ${session} question-${index + 1}\nassistant: ${session} decision-${index + 1}`),
  }));
}

it("bounds an archived history and keeps labels ordered when completions arrive out of order", async () => {
  const provider = controlledProvider();
  const engine = registerEngine(provider.complete);
  const messages = history("large", 100);
  await engine.bootstrap({ sessionId: "large", messages });
  const result = await drain(engine.assemble({ sessionId: "large", messages: [] }), provider);
  const expected = expectedSummaries("large", 100);
  expect(provider.calls).toHaveLength(expected.length);
  expect(provider.peak).toBe(BATCH_CONCURRENCY);
  expect(provider.active).toBe(0);
  expect(provider.completed[0]).toBe(provider.calls[BATCH_CONCURRENCY - 1].text);
  expect(engine.getStore().getTurnSummaries("large")).toEqual(expected);
  expect(result.messages[0].content).toBe([
    "[summary] Earlier conversation context:",
    ...expected.map((turn) => `Turn ${turn.turnSeq}: ${turn.summary}`),
  ].join("\n"));
  expect(result.messages.slice(1)).toEqual(messages.slice(-FRESH_TURNS * 2));
});

it("avoids quota fallbacks for all 97 summaries at a four-request provider", async () => {
  const provider = controlledProvider(CONCURRENCY);
  const engine = registerEngine(provider.complete);
  const messages = history("quota", 100);
  await drain(engine.assemble({ sessionId: "quota", messages }), provider);
  expect(provider.calls).toHaveLength(100 - FRESH_TURNS);
  expect(provider.peak).toBe(BATCH_CONCURRENCY);
  expect(provider.rejected).toBe(0);
  expect(engine.getStore().getTurnSummaries("quota")).toEqual(expectedSummaries("quota", 100));
  await engine.assemble({ sessionId: "quota", messages });
  expect(provider.calls).toHaveLength(100 - FRESH_TURNS);
});

it("shares one bound between overlapping assemble and afterTurn sessions", async () => {
  const provider = controlledProvider();
  const engine = registerEngine(provider.complete);
  const turnCount = 20;
  await engine.bootstrap({ sessionId: "after", messages: history("after", turnCount) });
  await drain(Promise.all([
    engine.assemble({ sessionId: "assemble", messages: history("assemble", turnCount) }),
    engine.afterTurn({ sessionId: "after" }),
  ]), provider);
  expect(provider.calls).toHaveLength(2 * (turnCount - FRESH_TURNS));
  expect(provider.peak).toBe(CONCURRENCY);
  expect(provider.active).toBe(0);
  for (const session of ["assemble", "after"]) {
    expect(engine.getStore().getTurnSummaries(session)).toEqual(expectedSummaries(session, turnCount));
  }
});

it("releases slots after synchronous throws and rejected summaries without stranding later calls", async () => {
  const provider = controlledProvider();
  const engine = registerEngine(provider.complete);
  const started: string[] = [];
  // Exercise the public summarizer setter as well as the registered engine path:
  // the normal host wrapper converts provider errors into successful fallbacks.
  engine.setSummarizeFn((text) => {
    started.push(text);
    if (text.startsWith("user: failing question-1\n")) throw new Error("synchronous failure");
    if (text.startsWith("user: failing question-2\n")) return Promise.reject(new Error("rejection"));
    return provider.complete({ messages: [{ role: "user", content: text }] }).then((result) => result.text);
  });
  const turnCount = 12;
  const result = await drain(engine.assemble({ sessionId: "failing", messages: history("failing", turnCount) }), provider);
  expect(started).toHaveLength(turnCount - FRESH_TURNS);
  expect(provider.peak).toBe(BATCH_CONCURRENCY);
  expect(provider.active).toBe(0);
  expect(engine.getStore().getTurnSummaries("failing")).toEqual(expectedSummaries("failing", turnCount).slice(2));
  expect(result.messages[0].content).toContain("Turn 1: [no summary]");
  expect(result.messages[0].content).toContain("Turn 2: [no summary]");
  provider.resetPeak();
  await drain(engine.assemble({ sessionId: "later", messages: history("later", 8) }), provider);
  expect(provider.peak).toBe(BATCH_CONCURRENCY);
  expect(engine.getStore().getTurnSummaries("later")).toEqual(expectedSummaries("later", 8));
});

it("does not request summaries for a history entirely within the fresh tail", async () => {
  const provider = controlledProvider();
  const engine = registerEngine(provider.complete);
  const messages = history("small", FRESH_TURNS);
  const result = await engine.assemble({ sessionId: "small", messages });
  await engine.afterTurn({ sessionId: "small" });
  expect(result.messages).toEqual(messages);
  expect(provider.calls).toHaveLength(0);
});

it("keeps single-summary and sequential calls working without additional requests", async () => {
  const provider = controlledProvider();
  const engine = registerEngine(provider.complete);
  for (const session of ["first", "second"]) {
    const messages = history(session, FRESH_TURNS + 1);
    await drain(engine.assemble({ sessionId: session, messages }), provider);
    await engine.assemble({ sessionId: session, messages });
    expect(engine.getStore().getTurnSummaries(session)).toEqual(expectedSummaries(session, FRESH_TURNS + 1));
  }
  expect(provider.calls).toHaveLength(2);
  expect(provider.peak).toBe(1);
  expect(provider.active).toBe(0);
});

it("keeps separate registered engine instances independent", async () => {
  const first = controlledProvider();
  const second = controlledProvider();
  const a = registerEngine(first.complete);
  const b = registerEngine(second.complete);
  const jobA = a.assemble({ sessionId: "a", messages: history("a", 10) });
  const jobB = b.assemble({ sessionId: "b", messages: history("b", 10) });
  await new Promise<void>((resolve) => setImmediate(resolve));
  const activeBeforeRelease = [first.active, second.active];
  await drain(Promise.all([jobA, jobB]), first, second);
  expect(activeBeforeRelease).toEqual([BATCH_CONCURRENCY, BATCH_CONCURRENCY]);
  expect(a.getStore().getTurnSummaries("a")).toEqual(expectedSummaries("a", 10));
  expect(b.getStore().getTurnSummaries("b")).toEqual(expectedSummaries("b", 10));
});

it("starts a short session without waiting for another batch's backlog", async () => {
  const provider = controlledProvider(CONCURRENCY);
  const engine = registerEngine(provider.complete);
  const big = engine.assemble({ sessionId: "big", messages: history("big", 60) });
  await new Promise<void>((resolve) => setImmediate(resolve));
  const small = engine.assemble({ sessionId: "small", messages: history("small", 4) });
  await new Promise<void>((resolve) => setImmediate(resolve));
  const firstSmall = provider.calls.findIndex((call) => call.text.startsWith("user: small "));
  const completedBeforeSmall = provider.completed.length;
  const startedBeforeRelease = provider.calls.length;
  await drain(Promise.all([big, small]), provider);
  expect(firstSmall).toBe(BATCH_CONCURRENCY);
  expect(completedBeforeSmall).toBe(0);
  expect(startedBeforeRelease).toBe(BATCH_CONCURRENCY + 1);
  expect(provider.rejected).toBe(0);
  expect(engine.getStore().getTurnSummaries("small")).toEqual(expectedSummaries("small", 4));
  expect(engine.getStore().getTurnSummaries("big")).toEqual(expectedSummaries("big", 60));
});

it("lets another session finish while one batch's active requests remain unsettled", async () => {
  const provider = controlledProvider(CONCURRENCY);
  const engine = registerEngine(provider.complete);
  let bigDone = false;
  let smallDone = false;
  const big = engine.assemble({ sessionId: "stalled", messages: history("stalled", 20) })
    .then((result) => { bigDone = true; return result; });
  const small = engine.assemble({ sessionId: "progress", messages: history("progress", 8) })
    .then((result) => { smallDone = true; return result; });
  // Hold every request from the first batch, but complete requests from the other.
  // No wall-clock latency or provider timeout assumption is needed for this probe.
  for (let step = 0; step < 20 && !smallDone; step++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
    for (const call of [...provider.calls]) {
      if (call.text.startsWith("user: progress ")) call.finish?.();
    }
  }
  await new Promise<void>((resolve) => setImmediate(resolve));
  const progressBeforeRelease = { bigDone, smallDone };
  const completedWhileStalled = [...provider.completed];
  const stalledRequests = provider.calls.filter((call) => call.text.startsWith("user: stalled ")).length;
  await drain(Promise.all([big, small]), provider);
  expect(progressBeforeRelease).toEqual({ bigDone: false, smallDone: true });
  expect(stalledRequests).toBe(BATCH_CONCURRENCY);
  expect(completedWhileStalled).toHaveLength(8 - FRESH_TURNS);
  expect(completedWhileStalled.every((text) => text.startsWith("user: progress "))).toBe(true);
  expect(provider.peak).toBe(CONCURRENCY);
  expect(provider.rejected).toBe(0);
  expect(engine.getStore().getTurnSummaries("progress")).toEqual(expectedSummaries("progress", 8));
});
