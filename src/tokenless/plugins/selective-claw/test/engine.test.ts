import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { SelectiveContextEngine } from "../src/engine.js";
import { createConnection, closeConnection } from "../src/db/connection.js";
import type { DatabaseSync } from "node:sqlite";
import type { AgentMessage } from "../src/openclaw-bridge.js";

describe("SelectiveContextEngine", () => {
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;

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
});
