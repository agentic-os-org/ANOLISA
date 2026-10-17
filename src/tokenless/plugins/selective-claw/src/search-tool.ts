import type { MessageStore } from "./store/message-store.js";

const DEFAULT_LIMIT = 5;
const MAX_LIMIT = 20;
const PREVIEW_CHARACTERS = 300;

export type TurnSearchResult = {
  status: "ok" | "invalid_arguments" | "session_unresolved" | "no_stored_messages" | "search_unavailable";
  found: number;
  matches: Array<{ turnSeq: number; seq: number; role: string; preview: string }>;
  truncated: boolean;
  nextOffset: number | null;
};

export function emptySearchResult(status: TurnSearchResult["status"]): TurnSearchResult {
  return { status, found: 0, matches: [], truncated: false, nextOffset: null };
}

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
  query: unknown,
  limit: unknown = DEFAULT_LIMIT,
  offset: unknown = 0,
): TurnSearchResult {
  if (typeof query !== "string" || !query.trim()
    || typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > MAX_LIMIT
    || typeof offset !== "number" || !Number.isSafeInteger(offset) || offset < 0) {
    return emptySearchResult("invalid_arguments");
  }
  if (store.getMessageCount(sessionId) === 0) return emptySearchResult("no_stored_messages");
  const rows = store.searchMessagesPage(sessionId, query, limit + 1, offset);
  const truncated = rows.length > limit;
  const matches = rows.slice(0, limit).map((message) => ({
    turnSeq: message.turnSeq,
    seq: message.seq,
    role: message.role,
    preview: preview(message.content),
  }));
  return { status: "ok", found: matches.length, matches, truncated, nextOffset: truncated ? offset + matches.length : null };
}
