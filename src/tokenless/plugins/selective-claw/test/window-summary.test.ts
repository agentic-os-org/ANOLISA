import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { SelectiveContextEngine } from "../src/engine.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import type { DatabaseSync } from "node:sqlite";

const history: AgentMessage[] = Array.from({ length: 6 }, (_, index) => [
  { role: "user", content: `question-${index + 1}` },
  { role: "assistant", content: `answer-${index + 1}` },
]).flat();
const summarize = async (text: string) => `SUMMARY(${text})`;
const config = { freshTailTurns: 3, dbPath: ":memory:", enabled: true };
let db: DatabaseSync;
let engine: SelectiveContextEngine;
beforeEach(() => { db = createConnection(":memory:"); engine = new SelectiveContextEngine(db, config); });
afterEach(() => closeConnection(db));

it("projects archived summaries into suffix-window recall IDs without rewriting them", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const archived = engine.getStore().getTurnSummaries("s");
  for (let iteration = 0; iteration < 2; iteration++) {
    const result = await engine.assemble({ sessionId: "s", messages: history.slice(4) });
    expect(result.messages[0].content).toContain("Turn 1: SUMMARY(user: question-3\nassistant: answer-3)");
    expect(result.messages[0].content).not.toContain("question-1");
    expect(engine.expandTurns("s", [1]).turns[0].messages[0].content).toBe("question-3");
    expect(engine.getStore().getTurnSummaries("s")).toEqual(archived);
  }
  await engine.afterTurn({ sessionId: "s" });
  const full = await engine.assemble({ sessionId: "s", messages: history });
  expect(full.messages[0].content).toContain("Turn 1: SUMMARY(user: question-1");
  expect(engine.expandTurns("s", [1]).turns[0].messages[0].content).toBe("question-1");
});

it("persists a newly generated suffix summary under its archived turn", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  engine.setSummarizeFn(summarize);
  await engine.assemble({ sessionId: "s", messages: history.slice(4) });
  expect(engine.getStore().getTurnSummaries("s")).toEqual([
    { turnSeq: 3, summary: "SUMMARY(user: question-3\nassistant: answer-3)" },
  ]);
});

it("reuses suffix summaries after file archive reopen", async () => {
  const directory = mkdtempSync(join(tmpdir(), "selective-window-"));
  const fileConfig = { ...config, dbPath: join(directory, "archive.db") };
  let fileDb = createConnection(fileConfig.dbPath);
  try {
    const writer = new SelectiveContextEngine(fileDb, fileConfig);
    writer.setSummarizeFn(summarize);
    await writer.afterTurn({ sessionId: "s", messages: history });
    closeConnection(fileDb);
    fileDb = createConnection(fileConfig.dbPath);
    const reader = new SelectiveContextEngine(fileDb, fileConfig);
    const regenerate = vi.fn(summarize);
    reader.setSummarizeFn(regenerate);
    const result = await reader.assemble({ sessionId: "s", messages: history.slice(4) });
    expect(result.messages[0].content).toContain("question-3");
    expect(result.messages[0].content).not.toContain("question-1");
    expect(regenerate).not.toHaveBeenCalled();
  } finally {
    closeConnection(fileDb);
    rmSync(directory, { recursive: true, force: true });
  }
});

it("does not use or overwrite an archived summary for a replaced live turn", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const archived = engine.getStore().getTurnSummaries("s").find((s) => s.turnSeq === 1);
  const replaced = history.map((message, index) => index === 1 ? { role: "assistant", content: "replacement answer" } : message);
  const result = await engine.assemble({ sessionId: "s", messages: replaced });
  expect(result.messages[0].content).toContain("SUMMARY(user: question-1\nassistant: replacement answer)");
  expect(engine.expandTurns("s", [1]).turns[0].messages[1].content).toBe("replacement answer");
  expect(engine.getStore().getTurnSummaries("s").find((s) => s.turnSeq === 1)).toEqual(archived);
});

it("keeps a partial leading turn summary local", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const archived = engine.getStore().getTurnSummaries("s");
  const result = await engine.assemble({ sessionId: "s", messages: history.slice(5) });
  expect(result.messages[0].content).toContain("Turn 1: SUMMARY(assistant: answer-3)");
  expect(result.messages[0].content).not.toContain("question-");
  expect(engine.getStore().getTurnSummaries("s")).toEqual(archived);
});

it("uses durable IDs for newly appended replacement-window turns", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const replacement = ["a", "b", "c", "d"].map((content) => ({ role: "user", content }));
  const result = await engine.assemble({ sessionId: "s", messages: replacement });
  expect(result.messages[0].content).toContain("Turn 1: SUMMARY(user: a)");
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({ turnSeq: 7, summary: "SUMMARY(user: a)" });
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({ turnSeq: 1, summary: "SUMMARY(user: question-1\nassistant: answer-1)" });
});

it("does not borrow another archived turn summary when no summarizer is available", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const reader = new SelectiveContextEngine(db, config);
  const result = await reader.assemble({ sessionId: "s", messages: history.slice(5) });
  expect(result.messages[0].content).toContain("[no summary]");
  expect(result.messages[0].content).not.toContain("question-1");
});

it("keeps summary projections isolated between sessions", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const other = history.map((m) => ({ ...m, content: `other-${m.content}` }));
  await engine.afterTurn({ sessionId: "other", messages: other });
  const result = await engine.assemble({ sessionId: "other", messages: other.slice(4) });
  expect(result.messages[0].content).toContain("other-question-3");
  expect(engine.expandTurns("s", [1]).turns[0].messages[0].content).toBe("question-1");
});

it("preserves durable summary anchors when a suffix window grows", async () => {
  engine.setSummarizeFn(summarize);
  await engine.afterTurn({ sessionId: "s", messages: history });
  const window = [...history.slice(4), { role: "user", content: "question-7" }, { role: "assistant", content: "answer-7" }];
  const result = await engine.assemble({ sessionId: "s", messages: window });
  expect(result.messages[0].content).toContain("Turn 2: SUMMARY(user: question-4\nassistant: answer-4)");
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({ turnSeq: 4, summary: "SUMMARY(user: question-4\nassistant: answer-4)" });
  await engine.afterTurn({ sessionId: "s", messages: window });
  expect(engine.getStore().getMessageCount("s")).toBe(14);
});
