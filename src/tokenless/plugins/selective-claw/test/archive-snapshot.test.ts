import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { DatabaseSync } from "node:sqlite";
import { afterEach, expect, it, vi } from "vitest";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveContextEngine } from "../src/engine.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import type { RuntimeLlmCompleteFn } from "../src/summarize.js";

const createConnection = connection.createConnection;
const databases: DatabaseSync[] = [];
const directories: string[] = [];

afterEach(() => {
  vi.restoreAllMocks();
  for (const db of databases.splice(0)) connection.closeConnection(db);
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function registeredEngine(dbPath = ":memory:") {
  const db = createConnection(dbPath);
  databases.push(db);
  vi.spyOn(connection, "createConnection").mockReturnValue(db);
  let engine!: SelectiveContextEngine;
  const complete = vi.fn<RuntimeLlmCompleteFn>(async ({ messages }) => ({ text: `SUMMARY(${messages[0].content})` }));
  activate({
    pluginConfig: { enabled: true, dbPath, freshTailTurns: 1 },
    runtime: { llm: { complete } },
    registerContextEngine(_id, factory) { engine = factory() as SelectiveContextEngine; },
    registerTool() {},
  });
  return { engine, db, complete };
}

function history(turns: number, start = 1): AgentMessage[] {
  return Array.from({ length: turns }, (_, i) => [
    { role: "user", content: `question-${start + i}` },
    { role: "assistant", content: `answer-${start + i}` },
  ]).flat();
}

function observeReads(engine: SelectiveContextEngine) {
  const store = engine.getStore();
  const read = store.getMessages.bind(store);
  let rows = 0;
  const spy = vi.spyOn(store, "getMessages").mockImplementation((session) => {
    const records = read(session);
    rows += records.length;
    return records;
  });
  return {
    expectBound(maxRows: number) {
      // This change removes repeated full scans; a future narrower read may use zero.
      expect(spy.mock.calls.length).toBeLessThanOrEqual(1);
      expect(rows).toBeLessThanOrEqual(maxRows);
    },
    reset() { spy.mockClear(); rows = 0; },
  };
}

it.each(["assemble", "afterTurn"] as const)("bounds full archive reads for %s of a short live suffix", async (method) => {
  const { engine } = registeredEngine();
  const messages = history(500);
  await engine.bootstrap({ sessionId: "long", messages });
  const reads = observeReads(engine);
  await engine[method]({ sessionId: "long", messages: messages.slice(-4) });
  expect(engine.getStore().getMessageCount("long")).toBe(1000);
  expect(engine.getStore().getTurnSummaries("long")).toContainEqual({
    turnSeq: 499, summary: "SUMMARY(user: question-499\nassistant: answer-499)",
  });
  reads.expectBound(1000);
});

it.each(["assemble", "afterTurn"] as const)("includes newly inserted rows in the %s snapshot and deduplicates replay", async (method) => {
  const { engine } = registeredEngine();
  const old = history(4);
  await engine.bootstrap({ sessionId: "s", messages: old });
  engine.getStore().setTurnSummary("s", 3, "existing third turn");
  const fresh = history(2, 5);
  fresh[1] = { role: "toolResult", content: [{ type: "text", text: "tool-output" }], toolCallId: "call-5", isError: true };
  const live = [...old.slice(-4), ...fresh];
  const reads = observeReads(engine);
  await engine[method]({ sessionId: "s", messages: live });
  expect(engine.getStore().getMessageCount("s")).toBe(12);
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({ turnSeq: 3, summary: "existing third turn" });
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({ turnSeq: 5, summary: "SUMMARY(user: question-5\ntoolResult: tool-output)" });
  expect(JSON.parse(engine.getStore().getMessagesByTurnSeqs("s", [5])[1].rawMessage!)).toEqual(fresh[1]);
  reads.expectBound(8);
  reads.reset();
  await engine[method]({ sessionId: "s", messages: live });
  expect(engine.getStore().getMessageCount("s")).toBe(12);
  reads.expectBound(12);
});

it("summarizes newly imported turns when afterTurn starts with an empty archive", async () => {
  const { engine } = registeredEngine();
  const reads = observeReads(engine);
  await engine.afterTurn({ sessionId: "empty", messages: history(3) });
  expect(engine.getStore().getMessageCount("empty")).toBe(6);
  expect(engine.getStore().getTurnSummaries("empty").map((s) => s.turnSeq)).toEqual([1, 2]);
  reads.expectBound(0);
});

it.each(["assemble", "afterTurn"] as const)("reads a reopened archive once for %s without a live window", async (method) => {
  const dir = mkdtempSync(join(tmpdir(), "selective-claw-snapshot-"));
  directories.push(dir);
  const path = join(dir, "archive.db");
  const first = registeredEngine(path);
  const messages = history(3);
  await first.engine.afterTurn({ sessionId: "persisted", messages });
  connection.closeConnection(first.db);
  const second = registeredEngine(path);
  const reads = observeReads(second.engine);
  if (method === "assemble") {
    const result = await second.engine.assemble({ sessionId: "persisted", messages: [] });
    expect(result.messages[0].content).toContain("Turn 1: SUMMARY(user: question-1");
    expect(result.messages.slice(1)).toEqual(messages.slice(-2));
  } else {
    await expect(second.engine.afterTurn({ sessionId: "persisted", messages: [] })).resolves.toBeUndefined();
    expect(second.engine.getStore().getTurnSummaries("persisted")).toEqual([
      { turnSeq: 1, summary: "SUMMARY(user: question-1\nassistant: answer-1)" },
      { turnSeq: 2, summary: "SUMMARY(user: question-2\nassistant: answer-2)" },
    ]);
  }
  expect(second.complete).not.toHaveBeenCalled();
  reads.expectBound(6);
});

it("keeps complete summary anchors and idempotent imports across replacement windows", async () => {
  const { engine } = registeredEngine();
  const messages = history(3);
  await engine.afterTurn({ sessionId: "replace", messages });
  const anchored = engine.getStore().getTurnSummaries("replace");
  const rewritten = [messages[2], { role: "assistant", content: "replacement answer" }, ...history(1, 4)];
  const reads = observeReads(engine);
  await engine.assemble({ sessionId: "replace", messages: rewritten });
  expect(engine.getStore().getMessageCount("replace")).toBe(10);
  expect(engine.getStore().getTurnSummaries("replace")).toEqual(expect.arrayContaining(anchored));
  reads.expectBound(6);
  reads.reset();
  await engine.afterTurn({ sessionId: "replace", messages: rewritten });
  expect(engine.getStore().getMessageCount("replace")).toBe(10);
  expect(engine.getStore().getTurnSummaries("replace")).toEqual(expect.arrayContaining(anchored));
  reads.expectBound(10);
});
