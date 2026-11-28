import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { DatabaseSync } from "node:sqlite";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import { SelectiveContextEngine } from "../src/engine.js";
import { closeConnection, createConnection } from "../src/db/connection.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";

describe("message import batch integrity", () => {
  let directory: string;
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;
  let dbPath: string;
  const batch: AgentMessage[] = [
    { role: "user", content: "new question" },
    { role: "toolResult", toolCallId: "call-1", toolName: "read", content: [{ type: "text", text: "reject batch" }] },
    { role: "assistant", content: "new answer" },
  ];

  function open() {
    db = createConnection(dbPath);
    engine = new SelectiveContextEngine(db, { dbPath, freshTailTurns: 3, enabled: true });
  }

  function failSecondInsert(action: "ABORT" | "ROLLBACK" = "ABORT") {
    db.exec(`CREATE TRIGGER reject_batch BEFORE INSERT ON messages
      WHEN new.role = 'tool'
      BEGIN SELECT RAISE(${action}, 'controlled batch failure'); END`);
  }

  beforeEach(() => {
    directory = mkdtempSync(join(tmpdir(), "selective-batch-"));
    dbPath = join(directory, "archive.db");
    open();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    closeConnection(db);
    rmSync(directory, { recursive: true, force: true });
  });

  it("retries the context engine obtained through actual plugin registration", async () => {
    const original = connection.createConnection;
    const opened: DatabaseSync[] = [];
    vi.spyOn(connection, "createConnection").mockImplementation((path) => {
      const actual = original(path);
      opened.push(actual);
      return actual;
    });
    let registered: SelectiveContextEngine | undefined;
    try {
      activate({
        pluginConfig: { dbPath },
        registerContextEngine(_id, factory) {
          registered = factory() as SelectiveContextEngine;
        },
      });
      expect(registered).toBeDefined();
      failSecondInsert();
      await expect(registered!.bootstrap({ sessionId: "registered", messages: batch })).rejects.toThrow("controlled batch failure");
      expect(engine.getStore().getMessageCount("registered")).toBe(0);
      db.exec("DROP TRIGGER reject_batch");
      expect(await registered!.bootstrap({ sessionId: "registered", messages: batch })).toMatchObject({ importedMessages: batch.length });
      expect(engine.getStore().getMessages("registered").map((r) => JSON.parse(r.rawMessage!))).toEqual(batch);
    } finally {
      for (const actual of opened) closeConnection(actual);
    }
  });

  it("retries an empty bootstrap after a real SQLite page-limit failure", async () => {
    const pages = (db.prepare("PRAGMA page_count").get() as { page_count: number }).page_count;
    db.exec(`PRAGMA max_page_count = ${pages + 12}`);
    const messages: AgentMessage[] = [
      { role: "user", content: "fits in the remaining pages" },
      { role: "assistant", content: "large answer ".repeat(100_000) },
    ];
    await expect(engine.bootstrap({ sessionId: "s1", messages })).rejects.toMatchObject({ errcode: 13 });
    expect(engine.getStore().getMessageCount("s1")).toBe(0);

    db.exec("PRAGMA max_page_count = 1073741823");
    closeConnection(db);
    open();
    expect(await engine.bootstrap({ sessionId: "s1", messages })).toMatchObject({ importedMessages: 2 });
    expect(engine.getStore().getMessages("s1").map((r) => JSON.parse(r.rawMessage!))).toEqual(messages);
  });

  it("leaves a failed initial import empty and retryable after reopen", async () => {
    await engine.ingest({ sessionId: "other", message: { role: "user", content: "keep me" } });
    failSecondInsert();
    await expect(engine.bootstrap({ sessionId: "s1", messages: batch })).rejects.toThrow("controlled batch failure");
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(engine.getStore().getMessageCount("other")).toBe(1);
    db.exec("DROP TRIGGER reject_batch");
    closeConnection(db);
    open();
    expect(await engine.bootstrap({ sessionId: "s1", messages: batch })).toMatchObject({ importedMessages: batch.length });
    expect(engine.getStore().getMessages("s1").map((r) => JSON.parse(r.rawMessage!))).toEqual(batch);
  });

  it.each(["assemble", "afterTurn"] as const)("preserves the existing archive when %s reconciliation fails", async (method) => {
    const original: AgentMessage[] = [
      { role: "user", content: "old question" },
      { role: "assistant", content: "old answer" },
    ];
    await engine.bootstrap({ sessionId: "s1", messages: original });
    engine.getStore().setTurnSummary("s1", 1, "original summary");
    const before = engine.getStore().getMessages("s1");
    failSecondInsert();
    await expect(engine[method]({ sessionId: "s1", messages: [...original, ...batch] })).rejects.toThrow("controlled batch failure");
    expect(engine.getStore().getMessages("s1")).toEqual(before);
    expect(engine.getStore().getTurnSummaries("s1")).toEqual([{ turnSeq: 1, summary: "original summary" }]);
    expect(engine.getStore().searchMessages("s1", "new", 10)).toEqual([]);

    db.exec("DROP TRIGGER reject_batch");
    await engine[method]({ sessionId: "s1", messages: [...original, ...batch] });
    // Replaying the same window must not import it a second time.
    await engine[method]({ sessionId: "s1", messages: [...original, ...batch] });
    expect(engine.getStore().getMessages("s1").map((r) => [r.seq, r.turnSeq])).toEqual([[1, 1], [2, 1], [3, 2], [4, 2], [5, 2]]);
  });

  it("preserves earlier caller writes when a batch statement aborts", async () => {
    failSecondInsert();
    db.exec("BEGIN");
    await engine.ingest({ sessionId: "caller", message: { role: "user", content: "caller-owned write" } });
    await expect(engine.bootstrap({ sessionId: "s1", messages: batch })).rejects.toThrow("controlled batch failure");
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(engine.getStore().getMessageCount("caller")).toBe(1);
    db.exec("COMMIT");
    closeConnection(db);
    open();
    expect(engine.getStore().getMessageCount("caller")).toBe(1);
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
  });

  it("leaves successful nested imports under the caller's commit control", async () => {
    db.exec("BEGIN");
    await engine.bootstrap({ sessionId: "s1", messages: batch });
    expect(engine.getStore().getMessageCount("s1")).toBe(batch.length);
    db.exec("ROLLBACK");
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(await engine.bootstrap({ sessionId: "s1", messages: batch })).toMatchObject({ importedMessages: batch.length });
  });

  it("preserves the original SQLite error when SQLite aborts the whole transaction", async () => {
    await engine.ingest({ sessionId: "other", message: { role: "user", content: "committed earlier" } });
    failSecondInsert("ROLLBACK");
    await expect(engine.bootstrap({ sessionId: "s1", messages: batch })).rejects.toMatchObject({
      code: "ERR_SQLITE_ERROR", errcode: 1811, message: "controlled batch failure", // SQLITE_CONSTRAINT_TRIGGER
    });
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(engine.getStore().getMessageCount("other")).toBe(1);
    db.exec("DROP TRIGGER reject_batch");
    expect(await engine.bootstrap({ sessionId: "s1", messages: batch })).toMatchObject({ importedMessages: batch.length });
  });


  it.each(["assemble", "afterTurn"] as const)("does not open an import transaction for an unchanged %s window", async (method) => {
    await engine.bootstrap({ sessionId: "s1", messages: batch });
    const exec = vi.spyOn(db, "exec");
    await engine[method]({ sessionId: "s1", messages: batch });
    const transactions = exec.mock.calls.map(([sql]) => sql).filter((sql) =>
      /^\s*(SAVEPOINT|RELEASE|ROLLBACK|BEGIN|COMMIT)\b/i.test(sql),
    );
    // Successful assembly still atomically publishes its recall mapping.
    // An unchanged import must add no transaction-control statements.
    expect(transactions).toEqual(method === "assemble"
      ? ["SAVEPOINT recall_aliases", "RELEASE recall_aliases"]
      : []);
    expect(engine.getStore().getMessageCount("s1")).toBe(batch.length);
    // First assembly must publish missing aliases; replaying that published window is read-only.
    exec.mockClear();
    await engine[method]({ sessionId: "s1", messages: batch });
    expect(exec.mock.calls.map(([sql]) => sql).filter((sql) =>
      /^\s*(SAVEPOINT|RELEASE|ROLLBACK|BEGIN|COMMIT)\b/i.test(sql),
    )).toEqual([]);
  });

  it("rolls back the batch when the final release fails a deferred constraint", async () => {
    await engine.ingest({ sessionId: "other", message: { role: "user", content: "committed earlier" } });
    db.exec(`CREATE TABLE import_parent (id INTEGER PRIMARY KEY);
      CREATE TABLE import_effect (
        parent_id INTEGER REFERENCES import_parent(id) DEFERRABLE INITIALLY DEFERRED
      );
      CREATE TRIGGER defer_import AFTER INSERT ON messages
      WHEN new.session_id = 's1'
      BEGIN INSERT INTO import_effect VALUES (1); END`);
    await expect(engine.bootstrap({ sessionId: "s1", messages: batch })).rejects.toMatchObject({
      errcode: 787, // SQLITE_CONSTRAINT_FOREIGNKEY, checked when the outermost savepoint is released.
    });
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(engine.getStore().getMessageCount("other")).toBe(1);
    expect(db.prepare("SELECT COUNT(*) AS count FROM import_effect").get()).toEqual({ count: 0 });

    closeConnection(db);
    open();
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    expect(engine.getStore().getMessageCount("other")).toBe(1);
    expect(db.prepare("SELECT COUNT(*) AS count FROM import_effect").get()).toEqual({ count: 0 });
    db.exec("INSERT INTO import_parent VALUES (1)");
    expect(await engine.bootstrap({ sessionId: "s1", messages: batch })).toMatchObject({ importedMessages: batch.length });
  });

  it.each(["ROLLBACK TO", "RELEASE"])("reports a secondary %s failure without replacing the import error", async (statement) => {
    failSecondInsert();
    let importError: unknown;
    const store = engine.getStore();
    const create = store.createMessage.bind(store);
    vi.spyOn(store, "createMessage").mockImplementation((message) => {
      try {
        return create(message);
      } catch (error) {
        importError = error;
        throw error;
      }
    });
    const cleanupError = new Error("controlled cleanup failure");
    const exec = db.exec.bind(db);
    const execSpy = vi.spyOn(db, "exec").mockImplementation((sql) => {
      if (sql.startsWith(statement)) throw cleanupError;
      exec(sql);
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const failure = await engine.bootstrap({ sessionId: "s1", messages: batch }).catch((error: unknown) => error);
    expect(failure).toBe(importError);
    expect(failure).toMatchObject({ message: "controlled batch failure", errcode: 1811 }); // SQLITE_CONSTRAINT_TRIGGER
    expect(warn).toHaveBeenCalledWith(
      "[selective-claw] message import failed; savepoint cleanup also failed",
      cleanupError,
    );

    // Only cleanup is injected: real SQLite must not accidentally commit the prefix.
    const observer = createConnection(dbPath);
    try {
      expect(observer.prepare("SELECT COUNT(*) AS count FROM messages WHERE session_id = 's1'").get()).toEqual({ count: 0 });
    } finally {
      closeConnection(observer);
      execSpy.mockRestore();
      db.exec("ROLLBACK");
    }
    expect(engine.getStore().getMessageCount("s1")).toBe(0);
    db.exec("DROP TRIGGER reject_batch");
    expect(await engine.bootstrap({ sessionId: "s1", messages: batch })).toMatchObject({ importedMessages: batch.length });
  });
});
