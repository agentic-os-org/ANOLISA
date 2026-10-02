import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";
import { executeExpandTurn } from "../src/recall-tool.js";
import type { DatabaseSync } from "node:sqlite";

describe("executeExpandTurn", () => {
  let db: DatabaseSync;
  let store: MessageStore;

  beforeEach(() => {
    db = createConnection(":memory:");
    runMigrations(db);
    store = new MessageStore(db);

    store.createMessage({ sessionId: "s1", seq: 1, turnSeq: 1, role: "user", content: "How to deploy with Docker?", tokenCount: 10 });
    store.createMessage({ sessionId: "s1", seq: 2, turnSeq: 1, role: "assistant", content: "Use docker compose for deployment.", tokenCount: 10 });
    store.createMessage({ sessionId: "s1", seq: 3, turnSeq: 2, role: "user", content: "What database should we use?", tokenCount: 10 });
    store.createMessage({ sessionId: "s1", seq: 4, turnSeq: 2, role: "assistant", content: "PostgreSQL is recommended for this project.", tokenCount: 10 });
    store.createMessage({ sessionId: "s1", seq: 5, turnSeq: 3, role: "user", content: "Fix the authentication bug.", tokenCount: 10 });
    store.createMessage({ sessionId: "s1", seq: 6, turnSeq: 3, role: "assistant", content: "The JWT token validation was fixed.", tokenCount: 10 });
  });

  afterEach(() => {
    closeConnection(db);
  });

  it("expands a single turn by turn_id", () => {
    const result = executeExpandTurn(store, "s1", [1]);
    expect(result.found).toBe(1);
    expect(result.turns[0].turnSeq).toBe(1);
    expect(result.turns[0].messages).toHaveLength(2);
    expect(result.turns[0].messages[0].content).toBe("How to deploy with Docker?");
    expect(result.turns[0].messages[1].content).toBe("Use docker compose for deployment.");
  });

  it("expands multiple turns", () => {
    const result = executeExpandTurn(store, "s1", [1, 2]);
    expect(result.found).toBe(2);
    expect(result.turns[0].turnSeq).toBe(1);
    expect(result.turns[1].turnSeq).toBe(2);
  });

  it("returns full content without truncation", () => {
    store.createMessage({
      sessionId: "s1",
      seq: 7,
      turnSeq: 4,
      role: "user",
      content: "longword ".repeat(200),
      tokenCount: 50,
    });
    const result = executeExpandTurn(store, "s1", [4]);
    expect(result.turns[0].messages[0].content).toBe("longword ".repeat(200));
  });

  it("skips non-existent turn_id", () => {
    const result = executeExpandTurn(store, "s1", [999]);
    expect(result.found).toBe(0);
    expect(result.turns).toHaveLength(0);
  });

  it("returns empty for empty input", () => {
    const result = executeExpandTurn(store, "s1", []);
    expect(result.found).toBe(0);
    expect(result.turns).toHaveLength(0);
  });
});
