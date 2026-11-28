import { afterEach, expect, it } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { runMigrations } from "../src/db/migration.js";
import { MessageStore } from "../src/store/message-store.js";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

it.each([true, false])(
  "preserves legacy rows and search after a WAL reopen (existing FTS: %s)",
  (existingFts) => {
    const directory = mkdtempSync(join(tmpdir(), "selective-turn-backfill-"));
    directories.push(directory);
    const file = join(directory, "archive.db");
    let db = createConnection(file);
    const sessions = [
      {
        id: "user",
        roles: ["user", "assistant", "tool", "user", "assistant"],
        turns: [1, 1, 1, 2, 2],
      },
      {
        id: "system",
        roles: ["system", "assistant", "system", "user", "tool"],
        turns: [1, 1, 2, 3, 3],
      },
      {
        id: "assistant",
        roles: ["assistant", "tool", "user", "assistant", "system"],
        turns: [1, 1, 2, 2, 3],
      },
      {
        id: "tool",
        roles: ["tool", "assistant", "user", "tool", "assistant"],
        turns: [1, 1, 2, 2, 2],
      },
    ];
    try {
      runMigrations(db);
      const insert = db.prepare(`INSERT INTO messages
        (message_id, session_id, seq, role, content, token_count, raw_message, turn_seq, summary)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`);
      // Neither primary keys nor insertion order correspond to session seq.
      for (let i = 4; i >= 0; i--) {
        sessions.forEach((session, index) =>
          insert.run(
            1000 + (4 - i) * 31 + index * 7,
            session.id,
            3 + i * 11,
            session.roles[i],
            `archiveword ${session.id} ${i}`,
            12,
            JSON.stringify({ session: session.id, index: i }),
            99,
            i === 0 ? "retained summary" : null,
          ),
        );
      }
      db.exec("ALTER TABLE messages DROP COLUMN turn_seq");
      if (!existingFts) {
        db.exec(
          "DROP TRIGGER messages_ai; DROP TRIGGER messages_ad; DROP TRIGGER messages_au; DROP TABLE messages_fts",
        );
      }
      const fields =
        "message_id, session_id, seq, role, content, token_count, raw_message, created_at, summary";
      const before = db
        .prepare(`SELECT ${fields} FROM messages ORDER BY message_id`)
        .all();
      closeConnection(db);
      db = createConnection(file);
      expect(db.prepare("PRAGMA journal_mode").get()?.journal_mode).toBe("wal");
      runMigrations(db);
      closeConnection(db);
      db = createConnection(file);
      const store = new MessageStore(db);
      expect(
        db.prepare(`SELECT ${fields} FROM messages ORDER BY message_id`).all(),
      ).toEqual(before);
      for (const session of sessions) {
        expect(store.getMessages(session.id).map((row) => row.turnSeq)).toEqual(
          session.turns,
        );
        expect(
          store.searchMessages(session.id, "archiveword", 100),
        ).toHaveLength(5);
        expect(
          store.getMessagesByTurnSeqs(session.id, [2]).map((row) => row.seq),
        ).toEqual(
          session.turns.flatMap((turn, i) => (turn === 2 ? [3 + i * 11] : [])),
        );
      }
      const migrated = db
        .prepare("SELECT * FROM messages ORDER BY message_id")
        .all();
      runMigrations(db);
      expect(
        db.prepare("SELECT * FROM messages ORDER BY message_id").all(),
      ).toEqual(migrated);
      db.exec(
        "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
      );
      store.createMessage({
        sessionId: "probe",
        seq: 1,
        turnSeq: 1,
        role: "user",
        content: "insertprobe",
        tokenCount: 1,
      });
      expect(store.searchMessages("probe", "insertprobe", 5)).toHaveLength(1);
      db.exec(
        "UPDATE messages SET content = 'updateprobe' WHERE session_id = 'probe'",
      );
      expect(store.searchMessages("probe", "insertprobe", 5)).toEqual([]);
      expect(store.searchMessages("probe", "updateprobe", 5)).toHaveLength(1);
      db.exec("DELETE FROM messages WHERE session_id = 'probe'");
      expect(store.searchMessages("probe", "updateprobe", 5)).toEqual([]);
      db.exec(
        "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
      );
    } finally {
      closeConnection(db);
    }
  },
);
