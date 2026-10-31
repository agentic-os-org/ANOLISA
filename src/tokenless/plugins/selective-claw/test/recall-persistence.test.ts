import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { buildSync } from "esbuild";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { DatabaseSync } from "node:sqlite";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveContextEngine } from "../src/engine.js";
import { Assembler } from "../src/assembler.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import type { RecallResult } from "../src/recall-tool.js";

type ToolFactory = (context: { sessionId?: string }) => {
  execute(id: string, params: { turn_ids: number[] }): Promise<{ details: RecallResult }>;
};

const history: AgentMessage[] = Array.from({ length: 6 }, (_, index) => [
  { role: "user", content: `question-${index + 1}` },
  { role: "assistant", content: `answer-${index + 1}` },
]).flat();
let directory: string;
let archivePath: string;
let db: DatabaseSync;
let engine: SelectiveContextEngine;
let toolFactory: ToolFactory;
let pluginModule: string;

beforeAll(() => {
  const built = buildSync({
    entryPoints: [fileURLToPath(new URL("../src/plugin/index.ts", import.meta.url))],
    bundle: true,
    platform: "node",
    format: "esm",
    write: false,
  });
  pluginModule = `data:text/javascript;base64,${Buffer.from(built.outputFiles[0].text).toString("base64")}`;
});

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), "selective-recall-"));
  archivePath = join(directory, "archive.db");
  const opened = vi.spyOn(connection, "createConnection");
  activate({
    pluginConfig: { dbPath: archivePath, freshTailTurns: 3 },
    runtime: { llm: { complete: async ({ messages }: { messages: Array<{ content: string }> }) => ({ text: `SUMMARY(${messages[0].content})` }) } },
    registerContextEngine(_id, factory) { engine = factory() as SelectiveContextEngine; },
    registerTool(factory: ToolFactory) { toolFactory = factory; },
  });
  db = opened.mock.results[0].value as DatabaseSync;
});

afterEach(() => {
  connection.closeConnection(db);
  vi.restoreAllMocks();
  rmSync(directory, { recursive: true, force: true });
});

async function recall(sessionId = "s", turnIds = [1]): Promise<RecallResult> {
  return (await toolFactory({ sessionId }).execute("recall", { turn_ids: turnIds })).details;
}

function reopenAndRecall(sessionId = "s", turnIds = [1]): RecallResult {
  connection.closeConnection(db);
  const source = `
    const { default: activate } = await import(${JSON.stringify(pluginModule)});
    let toolFactory;
    activate({
      pluginConfig: { dbPath: ${JSON.stringify(archivePath)} },
      registerContextEngine() {},
      registerTool(factory) { toolFactory = factory; }
    });
    const result = await toolFactory({ sessionId: ${JSON.stringify(sessionId)} })
      .execute("reopened", { turn_ids: ${JSON.stringify(turnIds)} });
    console.log(JSON.stringify(result.details));
  `;
  const child = spawnSync(process.execPath, ["--input-type=module", "-e", source], {
    encoding: "utf8",
    timeout: 10_000,
    env: { HOME: directory, PATH: process.env.PATH },
  });
  expect(child.error).toBeUndefined();
  expect(child.status, child.stderr).toBe(0);
  return JSON.parse(child.stdout) as RecallResult;
}

async function suffixWindow(sessionId = "s") {
  await engine.bootstrap({ sessionId, messages: history });
  const assembled = await engine.assemble({ sessionId, messages: history.slice(4) });
  expect(assembled.messages[0].content).toContain("Turn 1: SUMMARY(user: question-3");
  // The archive's own turn 1 deliberately differs from the model-visible label.
  expect(engine.getStore().getMessagesByTurnSeqs(sessionId, [1])[0].content).toBe("question-1");
}

it("keeps the registered tool's suffix-window identity after cache eviction", async () => {
  await suffixWindow();
  const expected = await recall();
  expect(expected.turns[0].messages[0].content).toBe("question-3");
  const reads = vi.spyOn(engine.getStore(), "getMessagesByTurnSeqs");
  for (let i = 0; i < 10; i++) await engine.bootstrap({ sessionId: `other-${i}` });
  const actual = await recall();
  // Fail if a larger cache makes this stop exercising the archive fallback.
  expect(reads.mock.calls.some(([, ids]) => ids.length > 0)).toBe(true);
  expect(actual).toEqual(expected);
});

it("keeps suffix labels through a new process and preserves mixed request order", async () => {
  await suffixWindow();
  const expected = await recall("s", [2, 1, 2, 99]);
  expect(expected.turns.map((turn) => turn.messages[0].content)).toEqual(["question-4", "question-3"]);
  expect(reopenAndRecall("s", [2, 1, 2, 99])).toEqual(expected);
});

it("does not substitute a whole archived turn for a partial leading turn", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  await engine.assemble({ sessionId: "s", messages: history.slice(5) });
  expect((await recall()).turns[0].messages).toEqual([{ role: "assistant", content: "answer-3" }]);
  expect(reopenAndRecall()).toEqual({ found: 0, turns: [] });
});

it("does not substitute the archived original for a rewritten turn", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  const replacement = history.map((message, index) => index === 1
    ? { role: "assistant", content: "replacement answer" } : message);
  await engine.assemble({ sessionId: "s", messages: replacement });
  expect((await recall()).turns[0].messages[1].content).toBe("replacement answer");
  expect(reopenAndRecall()).toEqual({ found: 0, turns: [] });
});

it("recalls newly archived replacement-window turns by their live label", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  const replacement = ["new-a", "new-b", "new-c", "new-d"].map((content) => ({ role: "user", content }));
  await engine.assemble({ sessionId: "s", messages: replacement });
  expect(engine.getStore().getMessagesByTurnSeqs("s", [7])[0].content).toBe("new-a");
  expect(reopenAndRecall().turns[0].messages[0].content).toBe("new-a");
});

it("preserves legacy direct archive recall before any window is assembled", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  expect(reopenAndRecall().turns[0].messages[0].content).toBe("question-1");
});

it("keeps another session's archive numbering independent of suffix labels", async () => {
  await suffixWindow();
  await engine.bootstrap({ sessionId: "other", messages: history });
  expect(reopenAndRecall("other").turns[0].messages[0].content).toBe("question-1");
});

it("retains the accepted window when assemble cannot save its aliases", async () => {
  await suffixWindow();
  const accepted = await recall();
  const aliases = engine.getStore().getRecallAliases("s", [1, 2, 3, 4, 5, 6]);
  db.exec(`CREATE TRIGGER reject_alias BEFORE INSERT ON recall_aliases
    WHEN new.session_id = 's' AND new.live_turn_seq = 2
    BEGIN SELECT RAISE(FAIL, 'fixture alias write failure'); END`);
  await expect(engine.assemble({ sessionId: "s", messages: history })).rejects.toThrow("fixture alias write failure");
  expect(engine.getStore().getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(aliases);
  expect(await recall()).toEqual(accepted);
  expect(reopenAndRecall()).toEqual(accepted);
});

it("does not query durable aliases for a complete cache hit or an empty request", async () => {
  await suffixWindow();
  const readAliases = vi.spyOn(engine.getStore(), "getRecallAliases");
  expect((await recall()).turns[0].messages[0].content).toBe("question-3");
  expect(await recall("s", [])).toEqual({ found: 0, turns: [] });
  expect(readAliases).not.toHaveBeenCalled();
});

async function evictSession(): Promise<void> {
  for (let i = 0; i < 10; i++) await engine.bootstrap({ sessionId: `other-${i}` });
}

it.each(["warm", "evicted", "reopened"] as const)("keeps the assembled window after afterTurn with a %s cache", async (cacheState) => {
  await suffixWindow();
  const expected = await recall();
  await engine.afterTurn({ sessionId: "s" });
  if (cacheState === "reopened") {
    expect(reopenAndRecall()).toEqual(expected);
  } else {
    const reads = vi.spyOn(engine.getStore(), "getMessagesByTurnSeqs");
    if (cacheState === "evicted") await evictSession();
    expect(await recall()).toEqual(expected);
    if (cacheState === "evicted") {
      expect(reads.mock.calls.some(([, ids]) => ids.length > 0)).toBe(true);
    }
  }
});

it("keeps archive ingestion and summaries working without rewriting recall aliases at afterTurn", async () => {
  await suffixWindow();
  const expected = await recall();
  db.exec(`CREATE TRIGGER reject_alias BEFORE INSERT ON recall_aliases
    BEGIN SELECT RAISE(FAIL, 'afterTurn must not rewrite aliases'); END`);
  const extended = [...history, { role: "user", content: "question-7" }, { role: "assistant", content: "answer-7" }];
  await engine.afterTurn({ sessionId: "s", messages: extended });
  expect(engine.getStore().getMessageCount("s")).toBe(14);
  expect(engine.getStore().getTurnSummaries("s")).toContainEqual({
    turnSeq: 4, summary: "SUMMARY(user: question-4\nassistant: answer-4)",
  });
  expect(await recall()).toEqual(expected);
  expect(reopenAndRecall()).toEqual(expected);
});

it("replaces stale cache keys when a later assembled window shrinks", async () => {
  await suffixWindow();
  await engine.assemble({ sessionId: "s", messages: history.slice(10) });
  const expected = ["question-6", "question-2", "question-3"];
  const warm = await recall("s", [1, 2, 3]);
  expect(warm.turns.map((turn) => turn.messages[0].content)).toEqual(expected);
  await evictSession();
  expect(await recall("s", [1, 2, 3])).toEqual(warm);
  expect(reopenAndRecall("s", [1, 2, 3])).toEqual(warm);
});

it.each(["evicted", "reopened"] as const)("preserves original tool roles and extracted content with a %s cache", async (cacheState) => {
  const toolHistory: AgentMessage[] = Array.from({ length: 6 }, (_, index) => [
    { role: "user", content: `question-${index + 1}` },
    { role: "assistant", content: `answer-${index + 1}` },
    { role: "toolResult", toolCallId: `tool-${index + 1}`, toolName: "read", content: [
      { type: "tool_result", output: `file-${index + 1}` },
      { type: "text", text: `detail-${index + 1}` },
    ] },
  ]).flat();
  await engine.bootstrap({ sessionId: "s", messages: toolHistory });
  await engine.assemble({ sessionId: "s", messages: toolHistory.slice(6) });
  const warm = await recall();
  expect(warm.turns[0].messages[2]).toEqual({ role: "toolResult", content: "file-3\ndetail-3" });
  if (cacheState === "evicted") {
    await evictSession();
    expect(await recall()).toEqual(warm);
  } else {
    expect(reopenAndRecall()).toEqual(warm);
  }
});

it("keeps the published recall window when context assembly fails before returning", async () => {
  await suffixWindow();
  const accepted = await recall();
  const assemble = vi.spyOn(Assembler.prototype, "assemble").mockImplementationOnce(() => {
    throw new Error("fixture assembly failure");
  });
  try {
    await expect(engine.assemble({ sessionId: "s", messages: history })).rejects.toThrow("fixture assembly failure");
    expect(assemble).toHaveBeenCalledOnce();
  } finally {
    assemble.mockRestore();
  }
  expect(await recall()).toEqual(accepted);
  expect(reopenAndRecall()).toEqual(accepted);
});


it.each([1, 5])("evicts %i recall windows published after eviction during pending summaries", async (count) => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  const summarize = vi.fn(async (text: string) => {
    await gate;
    return `SUMMARY(${text})`;
  });
  engine.setSummarizeFn(summarize);
  const sessions = Array.from({ length: count }, (_, index) => `victim-${index}`);
  const pending = [];
  for (const sessionId of sessions) {
    const messages = history.map((message) => ({
      ...message, content: `${sessionId}: ${message.content}`,
    }));
    await engine.bootstrap({ sessionId, messages });
    pending.push(engine.assemble({ sessionId, messages: messages.slice(4) }));
  }
  expect(summarize).toHaveBeenCalled();
  // Evict every pending session while its real registered engine is awaiting a summary.
  for (let index = 0; index < 10; index++) {
    await engine.ingest({ sessionId: `during-${index}`, message: { role: "user", content: "other session" } });
  }
  release();
  await Promise.all(pending);
  expect(summarize).toHaveBeenCalledTimes(count);
  const accepted = [];
  for (const sessionId of sessions) {
    const result = await recall(sessionId);
    expect(result.turns[0].messages[0].content).toBe(`${sessionId}: question-3`);
    accepted.push(result);
  }

  const reads = vi.spyOn(engine.getStore(), "getMessagesByTurnSeqs");
  for (let index = 0; index < 30; index++) {
    await engine.ingest({ sessionId: `later-${index}`, message: { role: "user", content: "later session" } });
  }
  const cold = [];
  for (const sessionId of sessions) cold.push(await recall(sessionId));
  expect(cold).toEqual(accepted);
  // A stranded cache would return the same content but never consult real SQLite.
  expect(reads.mock.calls).toEqual(sessions.map((sessionId) => [sessionId, [3]]));
});

it("does not evict another published window when a resumed alias write fails", async () => {
  await suffixWindow();
  const accepted = await recall();
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  engine.setSummarizeFn(async (text) => { await gate; return `SUMMARY(${text})`; });
  const pending = engine.assemble({ sessionId: "s", messages: history });
  const sessions = Array.from({ length: 10 }, (_, index) => `published-${index}`);
  for (const sessionId of sessions) {
    await engine.assemble({ sessionId, messages: [{ role: "user", content: sessionId }] });
  }
  db.exec(`CREATE TRIGGER reject_resumed_alias BEFORE INSERT ON recall_aliases
    WHEN new.session_id = 's' AND new.live_turn_seq = 2
    BEGIN SELECT RAISE(FAIL, 'fixture resumed alias failure'); END`);
  const rejection = expect(pending).rejects.toThrow("fixture resumed alias failure");
  release();
  await rejection;

  const reads = vi.spyOn(engine.getStore(), "getMessagesByTurnSeqs");
  for (const sessionId of sessions) {
    expect((await recall(sessionId)).turns[0].messages[0].content).toBe(sessionId);
  }
  expect(reads).not.toHaveBeenCalled();
  expect(await recall()).toEqual(accepted);
  expect(reads).toHaveBeenCalledWith("s", [3]);
});
