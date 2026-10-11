import type { MessageStore } from "./store/message-store.js";

const DEFAULT_LIMIT = 5;
const MAX_LIMIT = 20;
const PREVIEW_CHARACTERS = 300;

export type TurnSearchResult = {
  found: number;
  matches: Array<{ turnSeq: number; seq: number; role: string; preview: string }>;
};

function preview(content: string): string {
  let result = "";
  let characters = 0;
  for (const character of content) {
    if (characters++ === PREVIEW_CHARACTERS) return `${result}…`;
    result += character;
  }
  return result;
}

export function executeSearchTurns(
  store: MessageStore,
  sessionId: string,
  query: string,
  limit?: number,
): TurnSearchResult {
  if (!query.trim()) return { found: 0, matches: [] };
  const boundedLimit = typeof limit === "number" && Number.isFinite(limit)
    ? Math.max(1, Math.min(MAX_LIMIT, Math.trunc(limit)))
    : DEFAULT_LIMIT;
  const matches = store.searchMessages(sessionId, query, boundedLimit).map((message) => ({
    turnSeq: message.turnSeq,
    seq: message.seq,
    role: message.role,
    preview: preview(message.content),
  }));
  return { found: matches.length, matches };
}
