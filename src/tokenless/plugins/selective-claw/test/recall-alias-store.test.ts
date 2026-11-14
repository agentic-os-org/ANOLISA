import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";

let db: DatabaseSync;
let store: MessageStore;
beforeEach(() => {
  db = createConnection(":memory:");
  runMigrations(db);
  store = new MessageStore(db);
});
afterEach(() => {
  vi.restoreAllMocks();
  closeConnection(db);
});

it("round-trips null anchors and replaces only the selected session's window", () => {
  store.replaceRecallAliases("s", new Map([[1, 3], [2, null]]));
  store.replaceRecallAliases("other", new Map([[1, 9]]));
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(new Map([[1, 3], [2, null]]));
  store.replaceRecallAliases("s", new Map([[1, 5]]));
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(new Map([[1, 5]]));
  expect(store.getRecallAliases("other", [1])).toEqual(new Map([[1, 9]]));
  store.replaceRecallAliases("s", new Map());
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6]).size).toBe(0);
  expect(store.getRecallAliases("other", [1])).toEqual(new Map([[1, 9]]));
});

it("rolls back a partially inserted window and permits retry after repair", () => {
  const accepted = new Map([[1, 3], [2, 4]]);
  store.replaceRecallAliases("s", accepted);
  db.exec(`CREATE TRIGGER reject_alias BEFORE INSERT ON recall_aliases
    WHEN new.live_turn_seq = 2
    BEGIN SELECT RAISE(FAIL, 'fixture alias write failure'); END`);
  expect(() => store.replaceRecallAliases("s", new Map([[1, 5], [2, 6]]))).toThrow("fixture alias write failure");
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(accepted);
  db.exec("DROP TRIGGER reject_alias");
  store.replaceRecallAliases("s", new Map([[1, 5], [2, 6]]));
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(new Map([[1, 5], [2, 6]]));
});

it("adds alias storage to an existing archive without rewriting its messages", () => {
  const original = store.createMessage({
    sessionId: "s", seq: 1, turnSeq: 1, role: "user", content: "existing", tokenCount: 2,
    rawMessage: JSON.stringify({ role: "user", content: "existing" }),
  });
  db.exec("DROP TABLE recall_aliases");
  runMigrations(db);
  store.replaceRecallAliases("s", new Map([[1, null]]));
  runMigrations(db);
  expect(store.getMessages("s")).toEqual([original]);
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(new Map([[1, null]]));
});

it("keeps the primary SQLite error when SQLite has already rolled back the savepoint", () => {
  const accepted = new Map([[1, 3], [2, 4]]);
  store.replaceRecallAliases("s", accepted);
  db.exec(`CREATE TRIGGER abort_alias BEFORE INSERT ON recall_aliases
    WHEN new.live_turn_seq = 2
    BEGIN SELECT RAISE(ROLLBACK, 'primary alias failure'); END`);
  expect(() => store.replaceRecallAliases("s", new Map([[1, 5], [2, 6]]))).toThrow("primary alias failure");
  expect(db.isTransaction).toBe(false);
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(accepted);
  db.exec("DROP TRIGGER abort_alias");
  store.replaceRecallAliases("s", new Map([[1, 5]]));
  expect(store.getRecallAliases("s", [1, 2, 3, 4, 5, 6])).toEqual(new Map([[1, 5]]));
});

it("returns only requested alias rows while distinguishing null from absent", () => {
  store.replaceRecallAliases("s", new Map([[1, 3], [2, null], [3, 5]]));
  expect(store.getRecallAliases("s", [2, 77])).toEqual(new Map([[2, null]]));
  expect(store.getRecallAliases("s", [])).toEqual(new Map());
});


it.each([
  { name: "empty", entries: [] },
  { name: "anchored and null", entries: [[1, 3], [2, null]] },
  { name: "different insertion order", entries: [[2, null], [1, 3]] },
] as Array<{ name: string; entries: Array<[number, number | null]> }>)(
  "keeps an identical $name mapping read-only with no savepoint",
  ({ entries }) => {
    const aliases = new Map(entries);
    store.replaceRecallAliases("s", aliases);
    store.replaceRecallAliases("other", new Map([[1, 9]]));
    db.exec("PRAGMA query_only = ON");
    const exec = vi.spyOn(db, "exec");
    expect(() => store.replaceRecallAliases("s", new Map([...aliases].reverse()))).not.toThrow();
    expect(exec).not.toHaveBeenCalled();
    expect(store.getRecallAliases("s", [1, 2, 3])).toEqual(aliases);
    expect(store.getRecallAliases("other", [1])).toEqual(new Map([[1, 9]]));
  },
);

it.each([
  { name: "removes extra old keys", entries: [[1, 3]] },
  { name: "adds a null key", entries: [[1, 3], [2, null], [3, null]] },
  { name: "changes a null anchor", entries: [[1, 3], [2, 6]] },
  { name: "distinguishes null from absent", entries: [[1, 3], [3, null]] },
  { name: "changes an anchor to null", entries: [[1, null], [2, null]] },
  { name: "clears a nonempty window", entries: [] },
] as Array<{ name: string; entries: Array<[number, number | null]> }>)(
  "$name instead of treating it as an unchanged mapping",
  ({ entries }) => {
    store.replaceRecallAliases("s", new Map([[1, 3], [2, null]]));
    store.replaceRecallAliases("other", new Map([[1, 9]]));
    store.replaceRecallAliases("s", new Map(entries));
    expect(store.getRecallAliases("s", [1, 2, 3])).toEqual(new Map(entries));
    expect(store.getRecallAliases("other", [1])).toEqual(new Map([[1, 9]]));
  },
);

it("repairs database changes even when the caller supplies its previous mapping", () => {
  const aliases = new Map<number, number | null>([[1, 3], [2, null]]);
  store.replaceRecallAliases("s", aliases);
  db.prepare("DELETE FROM recall_aliases WHERE session_id = ? AND live_turn_seq = ?").run("s", 2);
  store.replaceRecallAliases("s", aliases);
  expect(store.getRecallAliases("s", [1, 2, 3])).toEqual(aliases);
  db.prepare("INSERT INTO recall_aliases VALUES (?, ?, ?)").run("s", 3, 8);
  store.replaceRecallAliases("s", aliases);
  expect(store.getRecallAliases("s", [1, 2, 3])).toEqual(aliases);
});

it.each(["rollback", "release"] as const)(
  "reports a secondary %s failure while preserving the primary alias error",
  (stage) => {
    const aliases = new Map([[1, 3], [2, 4]]);
    store.replaceRecallAliases("s", aliases);
    db.exec(`CREATE TRIGGER fail_alias BEFORE INSERT ON recall_aliases
      WHEN new.live_turn_seq = 2
      BEGIN SELECT RAISE(FAIL, 'primary alias failure'); END`);
    const cleanupError = new Error("controlled " + stage + " failure");
    const originalExec = db.exec.bind(db);
    const statements: string[] = [];
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const exec = vi.spyOn(db, "exec").mockImplementation((sql) => {
      statements.push(sql);
      if (sql === (stage === "rollback" ? "ROLLBACK TO recall_aliases" : "RELEASE recall_aliases")) {
        throw cleanupError;
      }
      originalExec(sql);
    });
    try {
      expect(() => store.replaceRecallAliases("s", new Map([[1, 5], [2, 6]])))
        .toThrow("primary alias failure");
      expect(warn).toHaveBeenCalledExactlyOnceWith(
        expect.stringContaining("cleanup also failed"), cleanupError,
      );
      expect(db.isTransaction).toBe(true);
      if (stage === "rollback") expect(statements).not.toContain("RELEASE recall_aliases");
    } finally {
      exec.mockRestore();
      if (db.isTransaction) db.exec("ROLLBACK");
    }
    expect(store.getRecallAliases("s", [1, 2])).toEqual(aliases);
  },
);

it("reports cleanup failure after SQLite has already aborted the savepoint", () => {
  const aliases = new Map([[1, 3]]);
  store.replaceRecallAliases("s", aliases);
  db.exec(`CREATE TRIGGER abort_recall BEFORE INSERT ON recall_aliases
    BEGIN SELECT RAISE(ROLLBACK, 'original rollback error'); END`);
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  expect(() => store.replaceRecallAliases("s", new Map([[1, 7]]))).toThrow("original rollback error");
  expect(warn).toHaveBeenCalledExactlyOnceWith(
    expect.stringContaining("cleanup also failed"), expect.any(Error),
  );
  expect(db.isTransaction).toBe(false);
  expect(store.getRecallAliases("s", [1])).toEqual(aliases);
});
