import type { DatabaseSync } from "node:sqlite";

const FTS_UPDATE_TRIGGER_SQL = `CREATE TRIGGER messages_au AFTER UPDATE ON messages
  WHEN old.message_id IS NOT new.message_id OR old.content IS NOT new.content
  BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
      VALUES('delete', old.message_id, old.content);
    INSERT INTO messages_fts(rowid, content)
      VALUES (new.message_id, new.content);
  END`;

function columnExists(db: DatabaseSync, table: string, column: string): boolean {
  const rows = db.prepare(`PRAGMA table_info(${table})`).all() as Array<{ name: string }>;
  return rows.some((r) => r.name === column);
}

function upgradeFtsUpdateTrigger(db: DatabaseSync): void {
  const trigger = db.prepare(
    "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'messages_au'",
  ).get() as { sql: string } | undefined;
  // Keep ordinary archive opens read-only with respect to the trigger schema.
  if (trigger?.sql === FTS_UPDATE_TRIGGER_SQL) return;

  db.exec("SAVEPOINT selective_claw_fts_update");
  try {
    db.exec("DROP TRIGGER IF EXISTS messages_au");
    db.exec(FTS_UPDATE_TRIGGER_SQL);
    db.exec("RELEASE selective_claw_fts_update");
  } catch (error) {
    try {
      db.exec("ROLLBACK TO selective_claw_fts_update");
      db.exec("RELEASE selective_claw_fts_update");
    } catch (cleanupError) {
      console.warn(
        "[selective-claw] FTS trigger upgrade failed; cleanup also failed",
        cleanupError,
      );
    }
    throw error;
  }
}

export function runMigrations(db: DatabaseSync): void {
  db.exec(`
    CREATE TABLE IF NOT EXISTS messages (
      message_id      INTEGER PRIMARY KEY AUTOINCREMENT,
      session_id      TEXT NOT NULL,
      seq             INTEGER NOT NULL,
      role            TEXT NOT NULL CHECK(role IN ('system','user','assistant','tool')),
      content         TEXT NOT NULL DEFAULT '',
      token_count     INTEGER NOT NULL DEFAULT 0,
      raw_message     TEXT,
      created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
      UNIQUE(session_id, seq)
    );

    CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
  `);

  const ftsExists = db.prepare(
    `SELECT name FROM sqlite_master WHERE type='table' AND name='messages_fts'`
  ).get();

  if (!ftsExists) {
    try {
      db.exec(`
        CREATE VIRTUAL TABLE messages_fts USING fts5(
          content,
          content=messages,
          content_rowid=message_id,
          tokenize='porter unicode61'
        );

        CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
          INSERT INTO messages_fts(rowid, content) VALUES (new.message_id, new.content);
        END;

        CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
          INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.message_id, old.content);
        END;

        ${FTS_UPDATE_TRIGGER_SQL};

        INSERT INTO messages_fts(messages_fts) VALUES('rebuild');
      `);
    } catch {
      // FTS5 not available in this SQLite build — full-text search will be unavailable
    }
  } else {
    upgradeFtsUpdateTrigger(db);
  }

  if (!columnExists(db, "messages", "turn_seq")) {
    db.exec(`ALTER TABLE messages ADD COLUMN turn_seq INTEGER`);

    db.exec(`
      WITH turn_ids AS (
        SELECT message_id,
          SUM(CASE WHEN role IN ('user', 'system') THEN 1 ELSE 0 END)
            OVER (PARTITION BY session_id ORDER BY seq)
          + CASE WHEN FIRST_VALUE(role)
            OVER (PARTITION BY session_id ORDER BY seq)
            IN ('user', 'system') THEN 0 ELSE 1 END AS turn_seq
        FROM messages
      )
      UPDATE messages SET turn_seq = (
        SELECT turn_ids.turn_seq FROM turn_ids
        WHERE turn_ids.message_id = messages.message_id
      )
    `);
  }

  if (!columnExists(db, "messages", "summary")) {
    db.exec(`ALTER TABLE messages ADD COLUMN summary TEXT`);
  }
}
