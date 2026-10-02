import type { MessageStore, MessageRecord } from "./store/message-store.js";

export type RecallResult = {
  found: number;
  turns: Array<{
    turnSeq: number;
    messages: Array<{ seq: number; role: string; content: string }>;
  }>;
};

export function executeExpandTurn(
  store: MessageStore,
  sessionId: string,
  turnSeqs: number[],
): RecallResult {
  if (turnSeqs.length === 0) return { found: 0, turns: [] };

  const messages = store.getMessagesByTurnSeqs(sessionId, turnSeqs);
  const turnMap = new Map<number, MessageRecord[]>();
  for (const m of messages) {
    const arr = turnMap.get(m.turnSeq) ?? [];
    arr.push(m);
    turnMap.set(m.turnSeq, arr);
  }

  const turns = turnSeqs
    .filter((ts) => turnMap.has(ts))
    .map((ts) => ({
      turnSeq: ts,
      messages: (turnMap.get(ts) ?? []).map((m) => ({
        seq: m.seq,
        role: m.role,
        content: m.content,
      })),
    }));

  return { found: turns.length, turns };
}
