import type { DatabaseSync } from "node:sqlite";
import { sanitizeFts5Query } from "../fts5-sanitize.js";

export type MessageRole = "system" | "user" | "assistant" | "tool";

export type CreateMessageInput = {
  sessionId: string;
  seq: number;
  turnSeq: number;
  role: MessageRole;
  content: string;
  tokenCount: number;
  rawMessage?: string;
};

export type MessageRecord = {
  messageId: number;
  sessionId: string;
  seq: number;
  turnSeq: number;
  role: MessageRole;
  content: string;
  tokenCount: number;
  rawMessage: string | null;
  createdAt: string;
};

type RawMessageRow = {
  message_id: number;
  session_id: string;
  seq: number;
  turn_seq: number;
  role: string;
  content: string;
  token_count: number;
  raw_message: string | null;
  created_at: string;
};

type RawCountRow = {
  count: number;
};

type RawMaxSeqRow = {
  max_seq: number | null;
};

function toMessageRecord(row: RawMessageRow): MessageRecord {
  return {
    messageId: row.message_id,
    sessionId: row.session_id,
    seq: row.seq,
    turnSeq: row.turn_seq,
    role: row.role as MessageRole,
    content: row.content,
    tokenCount: row.token_count,
    rawMessage: row.raw_message,
    createdAt: row.created_at,
  };
}

export class MessageStore {
  constructor(private db: DatabaseSync) {}

  createMessage(input: CreateMessageInput): MessageRecord {
    const row = this.db.prepare(`
      INSERT INTO messages (session_id, seq, turn_seq, role, content, token_count, raw_message)
      VALUES (?, ?, ?, ?, ?, ?, ?)
      RETURNING message_id, session_id, seq, turn_seq, role, content, token_count, raw_message, created_at
    `).get(
      input.sessionId,
      input.seq,
      input.turnSeq,
      input.role,
      input.content,
      input.tokenCount,
      input.rawMessage ?? null,
    ) as RawMessageRow;

    return toMessageRecord(row);
  }

  getMessages(sessionId: string): MessageRecord[] {
    const rows = this.db.prepare(
      `SELECT message_id, session_id, seq, turn_seq, role, content, token_count, raw_message, created_at
       FROM messages WHERE session_id = ? ORDER BY seq ASC`
    ).all(sessionId) as RawMessageRow[];

    return rows.map(toMessageRecord);
  }

  getMessageCount(sessionId: string): number {
    const row = this.db.prepare(
      `SELECT COUNT(*) as count FROM messages WHERE session_id = ?`
    ).get(sessionId) as RawCountRow;
    return row.count;
  }

  getNextSeq(sessionId: string): number {
    const row = this.db.prepare(
      `SELECT MAX(seq) as max_seq FROM messages WHERE session_id = ?`
    ).get(sessionId) as RawMaxSeqRow;
    return (row.max_seq ?? 0) + 1;
  }

  getLastUserSeq(sessionId: string): number | null {
    const row = this.db.prepare(
      `SELECT MAX(seq) as max_seq FROM messages WHERE session_id = ? AND role = 'user'`
    ).get(sessionId) as RawMaxSeqRow;
    return row.max_seq ?? null;
  }

  getMaxTurnSeq(sessionId: string): number {
    const row = this.db.prepare(
      `SELECT MAX(turn_seq) as max_seq FROM messages WHERE session_id = ?`
    ).get(sessionId) as RawMaxSeqRow;
    return row.max_seq ?? 0;
  }

  getLastMessage(sessionId: string): MessageRecord | null {
    const row = this.db.prepare(
      `SELECT message_id, session_id, seq, turn_seq, role, content, token_count, raw_message, created_at
       FROM messages WHERE session_id = ? ORDER BY seq DESC LIMIT 1`
    ).get(sessionId) as RawMessageRow | undefined;
    return row ? toMessageRecord(row) : null;
  }

  searchMessages(sessionId: string, query: string, limit: number): MessageRecord[] {
    const sanitized = sanitizeFts5Query(query);
    try {
      const rows = this.db.prepare(`
        SELECT m.message_id, m.session_id, m.seq, m.turn_seq, m.role, m.content, m.token_count, m.raw_message, m.created_at
        FROM messages_fts fts
        JOIN messages m ON m.message_id = fts.rowid
        WHERE messages_fts MATCH ? AND m.session_id = ?
        ORDER BY rank
        LIMIT ?
      `).all(sanitized, sessionId, limit) as RawMessageRow[];

      return rows.map(toMessageRecord);
    } catch {
      return [];
    }
  }

  getMessagesByTurnSeqs(sessionId: string, turnSeqs: number[]): MessageRecord[] {
    if (turnSeqs.length === 0) return [];
    const placeholders = turnSeqs.map(() => "?").join(",");
    const rows = this.db.prepare(`
      SELECT message_id, session_id, seq, turn_seq, role, content, token_count, raw_message, created_at
      FROM messages
      WHERE session_id = ? AND turn_seq IN (${placeholders})
      ORDER BY seq ASC
    `).all(sessionId, ...turnSeqs) as RawMessageRow[];

    return rows.map(toMessageRecord);
  }

  setTurnSummary(sessionId: string, turnSeq: number, summary: string): void {
    this.db.prepare(`
      UPDATE messages SET summary = ?
      WHERE session_id = ? AND turn_seq = ? AND role = 'user'
    `).run(summary, sessionId, turnSeq);
  }

  getTurnSummaries(sessionId: string): { turnSeq: number; summary: string }[] {
    const rows = this.db.prepare(`
      SELECT DISTINCT turn_seq, summary FROM messages
      WHERE session_id = ? AND summary IS NOT NULL
      ORDER BY turn_seq ASC
    `).all(sessionId) as Array<{ turn_seq: number; summary: string }>;

    return rows.map((r) => ({ turnSeq: r.turn_seq, summary: r.summary }));
  }

  getDistinctTurnSeqs(sessionId: string): number[] {
    const rows = this.db.prepare(`
      SELECT DISTINCT turn_seq FROM messages
      WHERE session_id = ?
      ORDER BY turn_seq ASC
    `).all(sessionId) as Array<{ turn_seq: number }>;

    return rows.map((r) => r.turn_seq);
  }

}
