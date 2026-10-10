import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";
import type { DatabaseSync } from "node:sqlite";

describe("MessageStore", () => {
  let db: DatabaseSync;
  let store: MessageStore;

  beforeEach(() => {
    db = createConnection(":memory:");
    runMigrations(db);
    store = new MessageStore(db);
  });

  afterEach(() => {
    closeConnection(db);
  });

  describe("messages", () => {
    it("creates and retrieves a message", () => {
      store.createMessage({
        sessionId: "s1",
        seq: 1,
        turnSeq: 1,
        role: "user",
        content: "hello world",
        tokenCount: 3,
        rawMessage: '{"role":"user","content":"hello world"}',
      });
      const messages = store.getMessages("s1");
      expect(messages).toHaveLength(1);
      expect(messages[0].content).toBe("hello world");
      expect(messages[0].role).toBe("user");
      expect(messages[0].seq).toBe(1);
      expect(messages[0].turnSeq).toBe(1);
    });

    it("returns messages ordered by seq", () => {
      store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 1, role: "assistant", content: "hi", tokenCount: 1 });
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "hello", tokenCount: 1 });
      const messages = store.getMessages("s1");
      expect(messages[0].seq).toBe(1);
      expect(messages[1].seq).toBe(2);
    });

    it("counts messages", () => {
      expect(store.getMessageCount("s1")).toBe(0);
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "a", tokenCount: 1 });
      expect(store.getMessageCount("s1")).toBe(1);
    });

    it("gets next seq", () => {
      expect(store.getNextSeq("s1")).toBe(1);
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "a", tokenCount: 1 });
      expect(store.getNextSeq("s1")).toBe(2);
    });

    it("isolates messages by sessionId", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "a", tokenCount: 1 });
      store.createMessage({ sessionId: "s2", seq: 1, turnSeq: 1, role: "user", content: "b", tokenCount: 1 });
      expect(store.getMessages("s1")).toHaveLength(1);
      expect(store.getMessages("s2")).toHaveLength(1);
      expect(store.getMessages("s1")[0].content).toBe("a");
      expect(store.getMessages("s2")[0].content).toBe("b");
    });
  });

  describe("FTS5 search", () => {
    function hasFts5(): boolean {
      const row = db.prepare(
        `SELECT name FROM sqlite_master WHERE type='table' AND name='messages_fts'`
      ).get();
      return !!row;
    }

    it("finds messages by keyword", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "implement authentication with JWT tokens", tokenCount: 5 });
      store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 1, role: "assistant", content: "I will use React for the frontend", tokenCount: 5 });
      store.createMessage({ sessionId: "s1", seq: 3, turnSeq: 3, role: "user", content: "add OAuth support to the auth system", tokenCount: 5 });

      const results = store.searchMessages("s1", "authentication", 10);
      if (hasFts5()) {
        expect(results.length).toBeGreaterThanOrEqual(1);
        expect(results[0].content).toContain("authentication");
      } else {
        expect(results).toHaveLength(0);
      }
    });

    it("returns empty array for no matches", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "hello world", tokenCount: 2 });
      const results = store.searchMessages("s1", "xyznonexistent", 10);
      expect(results).toHaveLength(0);
    });

    it("respects limit parameter", () => {
      for (let i = 1; i <= 10; i++) {
        store.createMessage({ sessionId: "s1", seq: i, turnSeq: i, role: "user", content: `test message number ${i}`, tokenCount: 3 });
      }
      const results = store.searchMessages("s1", "test message", 3);
      if (hasFts5()) {
        expect(results).toHaveLength(3);
      } else {
        expect(results).toHaveLength(0);
      }
    });
  });

  describe("turn summaries", () => {
    it("sets and gets turn summary", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "q1", tokenCount: 1 });
      store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 1, role: "assistant", content: "a1", tokenCount: 1 });

      store.setTurnSummary("s1", 1, "Discussed q1");
      const summaries = store.getTurnSummaries("s1");
      expect(summaries).toHaveLength(1);
      expect(summaries[0].turnSeq).toBe(1);
      expect(summaries[0].summary).toBe("Discussed q1");
    });

    it("returns empty for no summaries", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "q1", tokenCount: 1 });
      expect(store.getTurnSummaries("s1")).toHaveLength(0);
    });

    it("overwrites existing summary", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "q1", tokenCount: 1 });
      store.setTurnSummary("s1", 1, "first");
      store.setTurnSummary("s1", 1, "second");
      const summaries = store.getTurnSummaries("s1");
      expect(summaries[0].summary).toBe("second");
    });
  });

  describe("legacy turn migration", () => {
    it("assigns ordinal turns per session including system and implicit turns", () => {
      const sessions = [
        { id: "user-start", roles: ["user", "assistant", "tool", "user", "assistant"], turns: [1, 1, 1, 2, 2] },
        { id: "system-start", roles: ["system", "assistant", "user", "assistant", "system"], turns: [1, 1, 2, 2, 3] },
        { id: "implicit-start", roles: ["assistant", "tool", "user", "assistant", "system"], turns: [1, 1, 2, 2, 3] },
      ] as const;
      for (const session of sessions) {
        session.roles.forEach((role, index) => {
          store.createMessage({ sessionId: session.id, seq: index * 2 + 3, turnSeq: 99, role, content: `${session.id} ${index}`, tokenCount: 1 });
        });
      }
      // Model an older archive while retaining its populated FTS index.
      db.exec("ALTER TABLE messages DROP COLUMN turn_seq");

      runMigrations(db);

      for (const session of sessions) {
        expect(store.getMessages(session.id).map((message) => message.turnSeq)).toEqual(session.turns);
      }
      expect(store.getMessagesByTurnSeqs("user-start", [2]).map((message) => message.role)).toEqual(["user", "assistant"]);
    });

    it("preserves existing turn identifiers on subsequent migrations", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 5, role: "user", content: "archived turn", tokenCount: 1 });
      store.setTurnSummary("s1", 5, "saved summary");

      runMigrations(db);
      runMigrations(db);

      expect(store.getDistinctTurnSeqs("s1")).toEqual([5]);
      expect(store.getTurnSummaries("s1")).toEqual([{ turnSeq: 5, summary: "saved summary" }]);
    });
  });

  describe("getDistinctTurnSeqs", () => {
    it("returns distinct turn_seqs in order", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "q1", tokenCount: 1 });
      store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 1, role: "assistant", content: "a1", tokenCount: 1 });
      store.createMessage({ sessionId: "s1", seq: 3, turnSeq: 3, role: "user", content: "q2", tokenCount: 1 });
      store.createMessage({ sessionId: "s1", seq: 4, turnSeq: 3, role: "assistant", content: "a2", tokenCount: 1 });

      const turns = store.getDistinctTurnSeqs("s1");
      expect(turns).toEqual([1, 3]);
    });

    it("returns empty for empty conversation", () => {
      expect(store.getDistinctTurnSeqs("s1")).toEqual([]);
    });
  });

  describe("FTS availability migration", () => {
    it("indexes archived rows when the full-text table is created later", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "legacyunique", tokenCount: 1 });
      db.exec("DROP TRIGGER messages_ai; DROP TRIGGER messages_ad; DROP TRIGGER messages_au; DROP TABLE messages_fts;");

      runMigrations(db);
      expect(store.searchMessages("s1", "legacyunique", 10).map((message) => message.seq)).toEqual([1]);
      runMigrations(db);
      expect(store.searchMessages("s1", "legacyunique", 10)).toHaveLength(1);
    });

    it("keeps insert, update and delete triggers usable for rebuilt archive rows", () => {
      store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "archivedword", tokenCount: 1 });
      db.exec("DROP TRIGGER messages_ai; DROP TRIGGER messages_ad; DROP TRIGGER messages_au; DROP TABLE messages_fts;");
      runMigrations(db);

      expect(() => db.exec("UPDATE messages SET content = 'updatedword' WHERE seq = 1")).not.toThrow();
      expect(store.searchMessages("s1", "archivedword", 10)).toEqual([]);
      expect(store.searchMessages("s1", "updatedword", 10)).toHaveLength(1);
      store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 2, role: "user", content: "insertedword", tokenCount: 1 });
      expect(store.searchMessages("s1", "insertedword", 10)).toHaveLength(1);
      db.exec("DELETE FROM messages WHERE seq = 1");
      expect(store.searchMessages("s1", "updatedword", 10)).toEqual([]);
    });
  });
});
