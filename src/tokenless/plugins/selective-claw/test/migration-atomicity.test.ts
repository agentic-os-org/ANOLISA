import { afterEach, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function legacyArchive(db: DatabaseSync) {
  db.exec(`CREATE TABLE messages (
    message_id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL, seq INTEGER NOT NULL, role TEXT NOT NULL,
    content TEXT NOT NULL, token_count INTEGER NOT NULL DEFAULT 0,
    raw_message TEXT, created_at TEXT NOT NULL DEFAULT '2026-01-01',
    UNIQUE(session_id, seq)
  );
  INSERT INTO messages(session_id, seq, role, content) VALUES
    ('a', 1, 'assistant', 'leading'), ('a', 2, 'user', 'question'),
    ('a', 3, 'assistant', 'answer'), ('b', 1, 'user', 'other');`);
}

it("rolls back a failed turn backfill and retries after reopening the archive", () => {
  const directory = mkdtempSync(join(tmpdir(), "selective-migration-"));
  directories.push(directory);
  const path = join(directory, "archive.db");
  let db = new DatabaseSync(path);
  try {
    legacyArchive(db);
    db.exec(`CREATE TRIGGER fail_backfill BEFORE UPDATE ON messages
      WHEN old.session_id = 'b' BEGIN SELECT RAISE(ABORT, 'backfill write failed'); END;`);
    expect(() => runMigrations(db)).toThrow("backfill write failed");
    db.close();
    db = new DatabaseSync(path);
    const columns = db.prepare("PRAGMA table_info(messages)").all().map(row => row.name);
    expect(columns).not.toContain("turn_seq");
    expect(db.prepare("SELECT count(*) AS n FROM messages").get()?.n).toBe(4);
    db.exec("DROP TRIGGER fail_backfill");
    runMigrations(db);
    runMigrations(db);
    const store = new MessageStore(db);
    expect(store.getMessages("a").map(row => row.turnSeq)).toEqual([1, 2, 2]);
    expect(store.getMessages("b").map(row => row.turnSeq)).toEqual([1]);
    expect(store.searchMessages("a", "question", 5).map(row => row.content)).toEqual(["question"]);
  } finally {
    db.close();
  }
});

it("does not commit the caller transaction when migration succeeds", () => {
  const db = new DatabaseSync(":memory:");
  try {
    legacyArchive(db);
    db.exec("BEGIN");
    runMigrations(db);
    expect(new MessageStore(db).getMessages("a")).toHaveLength(3);
    db.exec("ROLLBACK");
    expect(db.prepare("PRAGMA table_info(messages)").all().map(row => row.name)).not.toContain("turn_seq");
  } finally {
    db.close();
  }
});

it("removes a partially installed FTS index so a later startup can retry", () => {
  const db = new DatabaseSync(":memory:");
  try {
    legacyArchive(db);
    // A trigger name collision fails after the virtual table and insert trigger
    // have been created, like an interrupted multi-statement installation.
    db.exec("CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN SELECT 1; END");
    runMigrations(db);
    expect(new MessageStore(db).getMessages("a").map(row => row.turnSeq)).toEqual([1, 2, 2]);
    expect(db.prepare("SELECT name FROM sqlite_master WHERE name='messages_fts'").get()).toBeUndefined();
    db.exec("DROP TRIGGER messages_ad");
    runMigrations(db);
    const store = new MessageStore(db);
    expect(store.searchMessages("a", "question", 5)).toHaveLength(1);
    store.createMessage({ sessionId: "a", seq: 4, turnSeq: 3, role: "user", content: "recovered", tokenCount: 1 });
    expect(store.searchMessages("a", "recovered", 5)).toHaveLength(1);
    db.exec("DELETE FROM messages WHERE seq=4");
    expect(store.searchMessages("a", "recovered", 5)).toEqual([]);
  } finally {
    db.close();
  }
});
