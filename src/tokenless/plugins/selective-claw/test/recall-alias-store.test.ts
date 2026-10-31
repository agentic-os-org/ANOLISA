import { afterEach, beforeEach, expect, it } from "vitest";
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
afterEach(() => closeConnection(db));

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
