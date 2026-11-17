import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";

// The trigger shipped before metadata-only updates stopped rewriting the index.
const LEGACY_UPDATE_TRIGGER = `
  CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.message_id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.message_id, new.content);
  END;
`;

function changes(db: DatabaseSync): number {
  return (
    db.prepare("SELECT total_changes() AS count").get() as { count: number }
  ).count;
}

function installLegacyTrigger(db: DatabaseSync): void {
  db.exec("DROP TRIGGER messages_au");
  db.exec(LEGACY_UPDATE_TRIGGER);
}

describe("summary writes and the full-text index", () => {
  const connections: DatabaseSync[] = [];
  const directories: string[] = [];

  function open(path = ":memory:") {
    const db = createConnection(path);
    connections.push(db);
    runMigrations(db);
    return { db, store: new MessageStore(db) };
  }

  afterEach(() => {
    vi.restoreAllMocks();
    for (const db of connections.splice(0)) closeConnection(db);
    for (const directory of directories.splice(0))
      rmSync(directory, { recursive: true, force: true });
  });

  it.each(["fresh", "upgraded"])(
    "does not rewrite indexed text when saving a %s archive's summary",
    (schema) => {
      const { db, store } = open();
      const content = "deployment payload ".repeat(4096);
      store.createMessage({
        sessionId: "a",
        seq: 1,
        turnSeq: 1,
        role: "user",
        content,
        tokenCount: 4096,
      });
      store.createMessage({
        sessionId: "b",
        seq: 1,
        turnSeq: 1,
        role: "user",
        content: "other conversation",
        tokenCount: 2,
      });
      if (schema === "upgraded") {
        installLegacyTrigger(db);
        const beforeMigration = changes(db);
        runMigrations(db);
        expect(changes(db)).toBe(beforeMigration);
      }

      for (const summary of ["first summary", "replacement summary"]) {
        const before = changes(db);
        store.setTurnSummary("a", 1, summary);
        // Only the archive row changes; FTS writes would grow with the original payload.
        expect(changes(db) - before).toBe(1);
        expect(store.getTurnSummaries("a")).toEqual([{ turnSeq: 1, summary }]);
        expect(store.getMessages("a")[0].content).toBe(content);
        expect(store.searchMessages("a", "deployment", 10)).toHaveLength(1);
        expect(store.searchMessages("b", "deployment", 10)).toEqual([]);
      }
    },
  );

  it("does not rewrite the index for unchanged content or other metadata", () => {
    const { db, store } = open();
    store.createMessage({
      sessionId: "a",
      seq: 1,
      turnSeq: 1,
      role: "user",
      content: "retainedword",
      tokenCount: 1,
    });
    for (const sql of [
      "UPDATE messages SET content = content",
      "UPDATE messages SET token_count = 7, raw_message = 'changed metadata', turn_seq = 3",
    ]) {
      const before = changes(db);
      db.exec(sql);
      expect(changes(db) - before).toBe(1);
      expect(store.searchMessages("a", "retainedword", 10)).toHaveLength(1);
    }
  });

  it.each(["fresh", "upgraded"])(
    "keeps %s text and row-ID changes searchable",
    (schema) => {
      const { db, store } = open();
      store.createMessage({
        sessionId: "a",
        seq: 1,
        turnSeq: 1,
        role: "user",
        content: "originalword",
        tokenCount: 1,
      });
      if (schema === "upgraded") {
        installLegacyTrigger(db);
        runMigrations(db);
      }

      db.exec(
        "UPDATE messages SET content = 'replacementword' WHERE session_id = 'a'",
      );
      expect(store.searchMessages("a", "originalword", 10)).toEqual([]);
      expect(store.searchMessages("a", "replacementword", 10)).toHaveLength(1);
      db.exec(
        "UPDATE messages SET message_id = message_id + 100 WHERE session_id = 'a'",
      );
      expect(store.searchMessages("a", "replacementword", 10)).toHaveLength(1);
      db.exec("UPDATE messages SET rowid = rowid + 100 WHERE session_id = 'a'");
      expect(
        store.searchMessages("a", "replacementword", 10)[0].messageId,
      ).toBe(201);
      db.exec("DELETE FROM messages WHERE session_id = 'a'");
      expect(store.searchMessages("a", "replacementword", 10)).toEqual([]);
      expect(() =>
        db.exec(
          "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
        ),
      ).not.toThrow();
    },
  );

  it("persists the trigger upgrade across reopen without repeated schema writes", () => {
    const directory = mkdtempSync(
      join(tmpdir(), "selective-claw-summary-index-"),
    );
    directories.push(directory);
    const path = join(directory, "archive.db");
    const initial = open(path);
    initial.store.createMessage({
      sessionId: "a",
      seq: 1,
      turnSeq: 1,
      role: "user",
      content: "archivedword",
      tokenCount: 1,
    });
    installLegacyTrigger(initial.db);
    closeConnection(initial.db);

    const upgraded = open(path);
    const schemaVersion = upgraded.db.prepare("PRAGMA schema_version").get();
    runMigrations(upgraded.db);
    expect(upgraded.db.prepare("PRAGMA schema_version").get()).toEqual(
      schemaVersion,
    );
    closeConnection(upgraded.db);

    const reopened = open(path);
    const before = changes(reopened.db);
    reopened.store.setTurnSummary("a", 1, "persisted summary");
    expect(changes(reopened.db) - before).toBe(1);
    expect(reopened.store.getTurnSummaries("a")).toEqual([
      { turnSeq: 1, summary: "persisted summary" },
    ]);
    expect(reopened.store.searchMessages("a", "archivedword", 10)).toHaveLength(
      1,
    );
  });

  it("keeps trigger migration inside an existing caller transaction", () => {
    const { db, store } = open();
    installLegacyTrigger(db);
    db.exec("BEGIN");
    store.createMessage({
      sessionId: "uncommitted",
      seq: 1,
      turnSeq: 1,
      role: "user",
      content: "temporaryword",
      tokenCount: 1,
    });
    runMigrations(db);
    db.exec("ROLLBACK");
    expect(store.getMessages("uncommitted")).toEqual([]);
    expect(store.searchMessages("uncommitted", "temporaryword", 10)).toEqual(
      [],
    );
  });

  it("restores the legacy trigger if creating its replacement fails", () => {
    const { db, store } = open();
    store.createMessage({
      sessionId: "a",
      seq: 1,
      turnSeq: 1,
      role: "user",
      content: "retainedword",
      tokenCount: 1,
    });
    installLegacyTrigger(db);
    const original = db
      .prepare("SELECT sql FROM sqlite_master WHERE name = 'messages_au'")
      .get();
    const failure = new Error("injected trigger creation failure");
    const exec = db.exec.bind(db);
    const boundary = vi.spyOn(db, "exec").mockImplementation((sql) => {
      if (sql.startsWith("CREATE TRIGGER messages_au")) throw failure;
      exec(sql);
    });

    expect(() => runMigrations(db)).toThrow(failure);
    boundary.mockRestore();
    expect(
      db
        .prepare("SELECT sql FROM sqlite_master WHERE name = 'messages_au'")
        .get(),
    ).toEqual(original);
    db.exec("UPDATE messages SET content = 'replacementword'");
    expect(store.searchMessages("a", "retainedword", 10)).toEqual([]);
    expect(store.searchMessages("a", "replacementword", 10)).toHaveLength(1);
    runMigrations(db);
    const before = changes(db);
    store.setTurnSummary("a", 1, "retried upgrade");
    expect(changes(db) - before).toBe(1);
  });
});
