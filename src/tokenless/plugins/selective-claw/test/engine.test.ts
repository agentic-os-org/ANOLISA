import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { SelectiveContextEngine } from "../src/engine.js";
import { createConnection, closeConnection } from "../src/db/connection.js";
import type { DatabaseSync } from "node:sqlite";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

describe("SelectiveContextEngine", () => {
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;

  it.each(["stored", "cached", "mixed"])("expands repeated IDs once from %s turns", async (source) => {
    const messages: AgentMessage[] = [
      { role: "user", content: "first question" },
      { role: "assistant", content: "first answer" },
      { role: "user", content: "second question" },
      { role: "assistant", content: "second answer" },
    ];
    if (source === "cached") {
      await engine.assemble({ sessionId: "s1", messages });
    } else if (source === "mixed") {
      await engine.assemble({ sessionId: "s1", messages: messages.slice(0, 2) });
      for (const message of messages.slice(2)) await engine.ingest({ sessionId: "s1", message });
    } else {
      for (const message of messages) await engine.ingest({ sessionId: "s1", message });
    }
    const turnIds = [2, 1, 2, 999, 1];
    const result = engine.expandTurns("s1", turnIds);
    expect(result.found).toBe(2);
    expect(result.turns.map((turn) => turn.turnSeq)).toEqual([2, 1]);
    expect(result.turns[0].messages.map((message) => message.content)).toEqual(["second question", "second answer"]);
    expect(result.turns[1].messages.map((message) => message.content)).toEqual(["first question", "first answer"]);
    expect(turnIds).toEqual([2, 1, 2, 999, 1]);
  });

  beforeEach(() => {
    db = createConnection(":memory:");
    engine = new SelectiveContextEngine(db, {
      freshTailTurns: 3,
      dbPath: ":memory:",
      enabled: true,
    });
  });

  afterEach(() => {
    closeConnection(db);
  });

  describe("info", () => {
    it("has correct engine metadata", () => {
      expect(engine.info.id).toBe("selective-claw");
      expect(engine.info.ownsCompaction).toBe(true);
    });
  });

  describe("bootstrap", () => {
    it("creates a conversation for the session", async () => {
      const result = await engine.bootstrap({ sessionId: "test-session" });
      expect(result.bootstrapped).toBe(true);
    });

    it("is idempotent", async () => {
      await engine.bootstrap({ sessionId: "test-session" });
      const result = await engine.bootstrap({ sessionId: "test-session" });
      expect(result.bootstrapped).toBe(true);
    });

    it("imports messages on first bootstrap", async () => {
      const messages: AgentMessage[] = [
        { role: "user", content: "hello" },
        { role: "assistant", content: "hi there" },
      ];
      const result = await engine.bootstrap({ sessionId: "s1", messages });
      expect(result.importedMessages).toBe(2);
    });

    it("does not re-import on second bootstrap", async () => {
      const messages: AgentMessage[] = [
        { role: "user", content: "hello" },
      ];
      await engine.bootstrap({ sessionId: "s1", messages });
      const result = await engine.bootstrap({ sessionId: "s1", messages });
      expect(result.importedMessages).toBe(0);
    });

    it("assigns turn_seq correctly during import", async () => {
      const messages: AgentMessage[] = [
        { role: "user", content: "question 1" },
        { role: "assistant", content: "answer 1" },
        { role: "user", content: "question 2" },
        { role: "assistant", content: "answer 2" },
      ];
      await engine.bootstrap({ sessionId: "s1", messages });
      const store = engine.getStore();
      const msgs = store.getMessages("s1");
      expect(msgs[0].turnSeq).toBe(1);
      expect(msgs[1].turnSeq).toBe(1);
      expect(msgs[2].turnSeq).toBe(2);
      expect(msgs[3].turnSeq).toBe(2);
    });
  });

  describe("ingest", () => {
    it("stores a user message", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      const result = await engine.ingest({
        sessionId: "s1",
        message: { role: "user", content: "hello world" },
      });
      expect(result.ingested).toBe(true);
    });

    it("assigns turn_seq: user starts new turn", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "q1" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "a1" } });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "q2" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "a2" } });

      const store = engine.getStore();
      const msgs = store.getMessages("s1");
      expect(msgs[0].turnSeq).toBe(1);
      expect(msgs[1].turnSeq).toBe(1);
      expect(msgs[2].turnSeq).toBe(2);
      expect(msgs[3].turnSeq).toBe(2);
    });

    it("tool messages inherit current turn", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "do something" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "calling tool" } });
      await engine.ingest({ sessionId: "s1", message: { role: "toolResult", content: "tool output" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "done" } });

      const store = engine.getStore();
      const msgs = store.getMessages("s1");
      expect(msgs.every((m) => m.turnSeq === 1)).toBe(true);
    });
  });

  describe("assemble", () => {
    it.each(["bootstrap", "ingest", "gateway"] as const)("restores structured %s messages after reopening the archive", async (source) => {
      const directory = mkdtempSync(join(tmpdir(), "selective-claw-restore-"));
      const dbPath = join(directory, "archive.db");
      let archive = createConnection(dbPath);
      try {
        const config = { freshTailTurns: 3, dbPath, enabled: true };
        const writer = new SelectiveContextEngine(archive, config);
        const messages: AgentMessage[] = [
          { role: "user", content: [{ type: "text", text: "inspect project" }], timestamp: 100 },
          { role: "assistant", content: [{ type: "toolCall", id: "call-7", name: "read", arguments: { path: "README.md" } }], timestamp: 101 },
          { role: "toolResult", toolCallId: "call-7", toolUseId: "use-7", toolName: "read", isError: true, content: [{ type: "text", text: "file unavailable" }], timestamp: 102 },
          { role: "assistant", content: [{ type: "text", text: "Please check the path." }], timestamp: 103 },
        ];
        if (source === "bootstrap") {
          await writer.bootstrap({ sessionId: "s1", messages });
        } else if (source === "ingest") {
          for (const message of messages) await writer.ingest({ sessionId: "s1", message });
        } else {
          const live = await writer.assemble({ sessionId: "s1", messages, tokenBudget: 100000 });
          expect(live.messages[2]).toBe(messages[2]);
        }
        closeConnection(archive);
        archive = createConnection(dbPath);

        const reader = new SelectiveContextEngine(archive, config);
        const result = await reader.assemble({ sessionId: "s1", messages: [], tokenBudget: 100000 });
        expect(result.messages).toEqual(messages);
        expect(reader.getStore().getMessages("s1").map((message) => JSON.parse(message.rawMessage!))).toEqual(messages);
        await reader.afterTurn({ sessionId: "s1", messages: [] });
        expect(reader.expandTurns("s1", [1]).turns[0].messages[2].role).toBe("toolResult");
      } finally {
        closeConnection(archive);
        rmSync(directory, { recursive: true, force: true });
      }
    });

    it.each([undefined, "{broken", "null", "[]", '{"content":"no role"}'])("uses legacy text for unavailable or invalid raw payload %s", async (rawMessage) => {
      engine.getStore().createMessage({
        sessionId: "legacy", seq: 1, turnSeq: 1, role: "tool", content: "archived output", tokenCount: 2, rawMessage,
      });

      const result = await engine.assemble({ sessionId: "legacy", messages: [], tokenBudget: 100000 });
      expect(result.messages).toEqual([{ role: "tool", content: "archived output" }]);
      await engine.afterTurn({ sessionId: "legacy", messages: [] });
      expect(engine.expandTurns("legacy", [1]).turns[0].messages).toEqual([{ role: "tool", content: "archived output" }]);
    });

    it("returns ingested messages", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "hello" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "hi there" } });

      const result = await engine.assemble({
        sessionId: "s1",
        messages: [],
        tokenBudget: 100000,
      });
      expect(result.messages).toHaveLength(2);
      expect(result.estimatedTokens).toBeGreaterThan(0);
    });

    it("returns fallback for unknown session", async () => {
      const fallback: AgentMessage[] = [
        { role: "user", content: "test" },
      ];
      const result = await engine.assemble({
        sessionId: "unknown",
        messages: fallback,
        tokenBudget: 100000,
      });
      expect(result.messages).toEqual(fallback);
    });
  });

  describe("afterTurn", () => {
    it("generates summaries for old turns", async () => {
      await engine.bootstrap({ sessionId: "s1" });

      // Ingest 5 turns
      for (let i = 0; i < 5; i++) {
        await engine.ingest({ sessionId: "s1", message: { role: "user", content: `question ${i + 1}` } });
        await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: `answer ${i + 1}` } });
      }

      // Set a mock summarizer
      engine.setSummarizeFn(async (text: string) => `Summary of: ${text.slice(0, 20)}`);

      await engine.afterTurn({ sessionId: "s1" });

      const store = engine.getStore();
      const summaries = store.getTurnSummaries("s1");

      // 5 turns, freshTailTurns=3, so 2 old turns should have summaries
      expect(summaries.length).toBe(2);
    });

    it("does nothing without summarizeFn", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      for (let i = 0; i < 5; i++) {
        await engine.ingest({ sessionId: "s1", message: { role: "user", content: `q${i}` } });
        await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: `a${i}` } });
      }

      await engine.afterTurn({ sessionId: "s1" });

      const store = engine.getStore();
      expect(store.getTurnSummaries("s1")).toHaveLength(0);
    });

    it("does not re-summarize existing summaries", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      for (let i = 0; i < 5; i++) {
        await engine.ingest({ sessionId: "s1", message: { role: "user", content: `q${i}` } });
        await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: `a${i}` } });
      }

      let callCount = 0;
      engine.setSummarizeFn(async () => { callCount++; return "summary"; });

      await engine.afterTurn({ sessionId: "s1" });
      const firstCallCount = callCount;

      await engine.afterTurn({ sessionId: "s1" });
      expect(callCount).toBe(firstCallCount);
    });

    it("skips when all turns are within freshTailTurns", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "q1" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "a1" } });

      let called = false;
      engine.setSummarizeFn(async () => { called = true; return "summary"; });

      await engine.afterTurn({ sessionId: "s1" });
      expect(called).toBe(false);
    });
  });

  describe("compact", () => {
    it("returns compacted: true", async () => {
      const result = await engine.compact({ sessionId: "s1" });
      expect(result.ok).toBe(true);
      expect(result.compacted).toBe(true);
      expect(result.reason).toBe("context managed by assemble");
    });
  });

  describe("reconcile via assemble", () => {
    it("does not reimport a replacement across lifecycle calls or engine restart", async () => {
      const original: AgentMessage[] = [
        { role: "user", content: "q1" },
        { role: "assistant", content: "thinking" },
        { role: "user", content: "q2" },
        { role: "assistant", content: "a2" },
        { role: "user", content: "q3" },
        { role: "assistant", content: "a3" },
      ];
      const replaced = original.map((message, index) => index === 1 ? { role: "assistant", content: "final answer" } : message);
      engine.setSummarizeFn(async () => "saved summary");
      await engine.assemble({ sessionId: "s1", messages: original });
      await engine.assemble({ sessionId: "s1", messages: replaced });
      const store = engine.getStore();
      const archived = store.getMessages("s1");
      expect(archived).toHaveLength(11);
      await engine.afterTurn({ sessionId: "s1", messages: replaced });
      const summaries = store.getTurnSummaries("s1");
      await engine.assemble({ sessionId: "s1", messages: replaced });
      expect(store.getMessages("s1")).toEqual(archived);
      expect(store.getTurnSummaries("s1")).toEqual(summaries);

      const restored = new SelectiveContextEngine(db, { freshTailTurns: 3, dbPath: ":memory:", enabled: true });
      await restored.afterTurn({ sessionId: "s1", messages: replaced });
      const newMessage: AgentMessage = { role: "user", content: "q4" };
      const result = await restored.assemble({ sessionId: "s1", messages: [...replaced, newMessage] });
      expect(store.getMessageCount("s1")).toBe(12);
      expect(store.getMessages("s1").slice(0, 11)).toEqual(archived);
      expect(store.getLastMessage("s1")!.content).toBe("q4");
      expect(result.messages.at(-1)).toBe(newMessage);
    });

    it("recognizes overlapping repeated message patterns without duplicating an extended tail", async () => {
      const user: AgentMessage = { role: "user", content: "q" };
      await engine.assemble({ sessionId: "s1", messages: [user, { role: "assistant", content: "old" }] });
      const replaced = [user, ...["x", "y", "x", "y", "x", "z"].map((content) => ({ role: "assistant", content }))];
      await engine.assemble({ sessionId: "s1", messages: replaced });
      expect(engine.getStore().getMessageCount("s1")).toBe(8);

      await engine.afterTurn({ sessionId: "s1", messages: replaced });
      await engine.assemble({ sessionId: "s1", messages: [...replaced, { role: "assistant", content: "y" }] });
      expect(engine.getStore().getMessages("s1").map((message) => message.content)).toEqual([
        "q", "old", "x", "y", "x", "y", "x", "z", "y",
      ]);
    });

    it("imports params.messages into store when store is empty", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      const result = await engine.assemble({
        sessionId: "s1",
        messages: [
          { role: "user", content: "q1" },
          { role: "assistant", content: "a1" },
          { role: "user", content: "q2" },
          { role: "assistant", content: "a2" },
        ],
        tokenBudget: 100000,
      });
      expect(result.messages).toHaveLength(4);
      expect(engine.getStore().getMessageCount("s1")).toBe(4);
    });

    it("works without prior bootstrap", async () => {
      const result = await engine.assemble({
        sessionId: "s1",
        messages: [
          { role: "user", content: "q1" },
          { role: "assistant", content: "a1" },
        ],
        tokenBudget: 100000,
      });
      expect(result.messages).toHaveLength(2);
      expect(engine.getStore().getMessageCount("s1")).toBe(2);
    });

    it("is incremental — does not duplicate existing messages", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "q1" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "a1" } });

      await engine.assemble({
        sessionId: "s1",
        messages: [
          { role: "user", content: "q1" },
          { role: "assistant", content: "a1" },
          { role: "user", content: "q2" },
          { role: "assistant", content: "a2" },
        ],
        tokenBudget: 100000,
      });
      expect(engine.getStore().getMessageCount("s1")).toBe(4);
    });

    it("handles gateway replacing messages (same count, different tail)", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      // First assemble: 4 messages
      await engine.assemble({
        sessionId: "s1",
        messages: [
          { role: "user", content: "q1" },
          { role: "assistant", content: "a1" },
          { role: "user", content: "q2" },
          { role: "assistant", content: "a2" },
        ],
        tokenBudget: 100000,
      });
      expect(engine.getStore().getMessageCount("s1")).toBe(4);

      // Second assemble: same count (4) but last message replaced with user q3
      await engine.assemble({
        sessionId: "s1",
        messages: [
          { role: "user", content: "q1" },
          { role: "assistant", content: "a1" },
          { role: "user", content: "q2" },
          { role: "user", content: "q3" },
        ],
        tokenBudget: 100000,
      });
      expect(engine.getStore().getMessageCount("s1")).toBe(5);
    });
  });

  describe("reconcile via afterTurn", () => {
    it("imports params.messages and generates summaries", async () => {
      const messages: AgentMessage[] = [];
      for (let i = 0; i < 10; i++) {
        messages.push({ role: "user", content: `q${i}` });
        messages.push({ role: "assistant", content: `a${i}` });
      }
      await engine.bootstrap({ sessionId: "s1" });
      engine.setSummarizeFn(async () => "summary");
      await engine.afterTurn({ sessionId: "s1", messages });

      expect(engine.getStore().getMessageCount("s1")).toBe(20);
      const summaries = engine.getStore().getTurnSummaries("s1");
      expect(summaries.length).toBe(7);
    });

    it("works without prior bootstrap", async () => {
      const messages: AgentMessage[] = [
        { role: "user", content: "q1" },
        { role: "assistant", content: "a1" },
      ];
      await engine.afterTurn({ sessionId: "s1", messages });
      expect(engine.getStore().getMessageCount("s1")).toBe(2);
    });
  });

  describe("getActiveSessionId", () => {
    it("returns null before any session activity", () => {
      expect(engine.getActiveSessionId()).toBeNull();
    });

    it("returns sessionId after bootstrap", async () => {
      await engine.bootstrap({ sessionId: "s1" });
      expect(engine.getActiveSessionId()).toBe("s1");
    });
  });

  describe("expandTurns", () => {
    it("merges cached turns and turns ingested after assembly in requested order", async () => {
      await engine.assemble({
        sessionId: "s1",
        messages: [{ role: "user", content: "cached question" }, { role: "assistant", content: "cached answer" }],
      });
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "new question" } });
      await engine.ingest({ sessionId: "s1", message: { role: "assistant", content: "new answer" } });

      const result = engine.expandTurns("s1", [2, 999, 1]);
      expect(result.found).toBe(2);
      expect(result.turns.map((turn) => turn.turnSeq)).toEqual([2, 1]);
      expect(result.turns[0].messages.map((message) => message.content)).toEqual(["new question", "new answer"]);
      expect(result.turns[1].messages.map((message) => message.content)).toEqual(["cached question", "cached answer"]);
    });

    it("keeps the live cached content when an archived turn has the same number", async () => {
      await engine.assemble({ sessionId: "s1", messages: [{ role: "user", content: "first version" }] });
      await engine.assemble({ sessionId: "s1", messages: [{ role: "user", content: "current version" }] });

      expect(engine.expandTurns("s1", [1]).turns[0].messages[0].content).toBe("current version");
      expect(engine.expandTurns("s1", [])).toEqual({ found: 0, turns: [] });
    });

    it("returns stored turns when no live cache exists", async () => {
      await engine.ingest({ sessionId: "s1", message: { role: "user", content: "stored question" } });
      expect(engine.expandTurns("s1", [1]).turns[0].messages[0].content).toBe("stored question");
      expect(engine.expandTurns("s1", [999])).toEqual({ found: 0, turns: [] });
    });
  });

  it.each(["assistant", "toolResult"])("keeps cached turn IDs consistent after a leading %s", async (role) => {
    const messages: AgentMessage[] = [
      { role, content: "leading context" },
      { role: "user", content: "new question" },
      { role: "assistant", content: "new answer" },
    ];
    await engine.bootstrap({ sessionId: "s1", messages });
    await engine.assemble({ sessionId: "s1", messages });

    expect(engine.getStore().getDistinctTurnSeqs("s1")).toEqual([1, 2]);
    const recalled = engine.expandTurns("s1", [1, 2]);
    expect(recalled.turns.map((turn) => turn.turnSeq)).toEqual([1, 2]);
    expect(recalled.turns[0].messages).toEqual([{ role, content: "leading context" }]);
    expect(recalled.turns[1].messages.map((message) => message.content)).toEqual(["new question", "new answer"]);
  });
});
