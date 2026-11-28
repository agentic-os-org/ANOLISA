import type { DatabaseSync } from "node:sqlite";

// The guard must cover every indexed column and the external-content row ID.
const FTS_UPDATE_TRIGGER_SQL = `CREATE TRIGGER messages_au AFTER UPDATE ON messages
  WHEN old.message_id IS NOT new.message_id OR old.content IS NOT new.content
  BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
      VALUES('delete', old.message_id, old.content);
    INSERT INTO messages_fts(rowid, content)
      VALUES (new.message_id, new.content);
  END`;

const LEGACY_FTS_UPDATE_TRIGGER_SQL = `CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.message_id, old.content);
  INSERT INTO messages_fts(rowid, content) VALUES (new.message_id, new.content);
END`.replace(/\s+/g, " ");

const SQLITE_BUSY = 5;

function isSqliteBusy(error: unknown): boolean {
  return (
    error instanceof Error &&
    "errcode" in error &&
    typeof error.errcode === "number" &&
    // SQLite puts the primary result code in the low byte.
    (error.errcode & 0xff) === SQLITE_BUSY
  );
}

function columnExists(db: DatabaseSync, table: string, column: string): boolean {
  const rows = db.prepare(`PRAGMA table_info(${table})`).all() as Array<{ name: string }>;
  return rows.some((r) => r.name === column);
}

function upgradeFtsUpdateTrigger(db: DatabaseSync): void {
  const trigger = db.prepare(
    "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'messages_au'",
  ).get() as { sql: string } | undefined;
  // Skip the schema write when the installed definition already matches.
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
      throw error;
    }
    // The old trigger is still correct after rollback. Defer the optimization
    // only for this known definition, so a missing or invalid trigger cannot
    // leave writes running against an unsynchronized index.
    if (
      isSqliteBusy(error) &&
      trigger?.sql.trim().replace(/\s+/g, " ") === LEGACY_FTS_UPDATE_TRIGGER_SQL
    ) {
      console.warn(
        "[selective-claw] FTS trigger upgrade deferred: archive is busy",
      );
      return;
    }
    throw error;
  }
}

export function runMigrations(db: DatabaseSync): void {
  // Schema presence is the migration marker, so it must commit with its data.
  // A savepoint also preserves transactions owned by the caller.
  db.exec("SAVEPOINT selective_claw_migrate");
  try {
    migrate(db);
    db.exec("RELEASE selective_claw_migrate");
  } catch (error) {
    try {
      db.exec("ROLLBACK TO selective_claw_migrate");
      db.exec("RELEASE selective_claw_migrate");
    } catch (cleanupError) {
      console.warn("[selective-claw] migration rollback failed", cleanupError);
    }
    throw error;
  }
}

function migrate(db: DatabaseSync): void {
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
    db.exec("SAVEPOINT selective_claw_fts");
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
      db.exec("RELEASE selective_claw_fts");
    } catch {
      // Optional FTS may be unavailable. Remove every partial installation so
      // table existence cannot prevent a complete retry on the next startup.
      db.exec("ROLLBACK TO selective_claw_fts");
      db.exec("RELEASE selective_claw_fts");
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
