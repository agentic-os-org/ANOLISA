import type { DatabaseSync } from "node:sqlite";
import type {
  AgentMessage,
  AssembleResult,
  BootstrapResult,
  CompactResult,
  ContextEngine,
  ContextEngineInfo,
  IngestResult,
} from "./openclaw-bridge.js";
import type { SelectiveClawConfig } from "./types.js";
import type { SummarizeFn } from "./summarize.js";
import { runMigrations } from "./db/migration.js";
import { MessageStore } from "./store/message-store.js";
import type { MessageRecord } from "./store/message-store.js";
import { Assembler } from "./assembler.js";
import { estimateTokens } from "./estimate-tokens.js";

const MAX_CACHED_SESSIONS = 10;

type ReconcileMessage = { role: string; content: string };

function messagesMatch(left: ReconcileMessage, right: ReconcileMessage): boolean {
  return left.role === right.role && left.content === right.content;
}

function suffixOverlap(stored: ReconcileMessage[], incoming: ReconcileMessage[]): number {
  if (stored.length === 0 || incoming.length === 0) return 0;

  // Prefix fallback keeps matching linear even for repeated message patterns.
  const fallback = new Array<number>(incoming.length).fill(0);
  let matched = 0;
  for (let i = 1; i < incoming.length; i++) {
    while (matched > 0 && !messagesMatch(incoming[i], incoming[matched])) {
      matched = fallback[matched - 1];
    }
    if (messagesMatch(incoming[i], incoming[matched])) matched++;
    fallback[i] = matched;
  }

  matched = 0;
  for (const message of stored) {
    while (matched > 0 && (matched === incoming.length || !messagesMatch(message, incoming[matched]))) {
      matched = fallback[matched - 1];
    }
    if (messagesMatch(message, incoming[matched])) matched++;
  }
  return matched;
}

export class SelectiveContextEngine implements ContextEngine {
  readonly info: ContextEngineInfo = {
    id: "selective-claw",
    name: "Selective Context Injection",
    version: "0.5.0",
    ownsCompaction: true,
  };

  private activeSessionId: string | null = null;
  private turnSummaryCache = new Map<string, Map<number, string>>();
  private turnMessagesCache = new Map<string, Map<number, AgentMessage[]>>();
  private sessionAccessOrder: string[] = [];

  private store: MessageStore;
  private assembler: Assembler;
  private migrated = false;
  private config: SelectiveClawConfig;
  private summarizeFn: SummarizeFn | null = null;

  constructor(
    private db: DatabaseSync,
    config: SelectiveClawConfig,
  ) {
    this.config = config;
    this.ensureMigrated();
    this.store = new MessageStore(db);
    this.assembler = new Assembler(config.freshTailTurns);
  }

  private ensureMigrated(): void {
    if (this.migrated) return;
    runMigrations(this.db);
    this.migrated = true;
  }

  setSummarizeFn(fn: SummarizeFn): void {
    this.summarizeFn = fn;
  }

  getStore(): MessageStore {
    return this.store;
  }

  getActiveSessionId(): string | null {
    return this.activeSessionId;
  }

  async bootstrap(params: {
    sessionId: string;
    sessionKey?: string;
    messages?: AgentMessage[];
  }): Promise<BootstrapResult> {
    this.activeSessionId = params.sessionId;
    this.touchSession(params.sessionId);

    if (params.messages && params.messages.length > 0) {
      const existingCount = this.store.getMessageCount(params.sessionId);
      if (existingCount === 0) {
        this.importMessages(params.sessionId, params.messages);
        return { bootstrapped: true, importedMessages: params.messages.length };
      }
    }

    return { bootstrapped: true, importedMessages: 0 };
  }

  async ingest(params: {
    sessionId: string;
    sessionKey?: string;
    message: AgentMessage;
  }): Promise<IngestResult> {
    this.activeSessionId = params.sessionId;
    this.touchSession(params.sessionId);

    const { message, sessionId } = params;
    const role = this.normalizeRole(message.role);
    const content = this.extractContent(message);
    const seq = this.store.getNextSeq(sessionId);

    let turnSeq: number;
    if (role === "user" || role === "system") {
      turnSeq = this.store.getMaxTurnSeq(sessionId) + 1;
    } else {
      turnSeq = Math.max(this.store.getMaxTurnSeq(sessionId), 1);
    }

    this.store.createMessage({
      sessionId,
      seq,
      turnSeq,
      role,
      content,
      tokenCount: estimateTokens(content),
      rawMessage: JSON.stringify(message),
    });

    return { ingested: true };
  }

  async assemble(params: {
    sessionId: string;
    sessionKey?: string;
    messages: AgentMessage[];
    tokenBudget?: number;
    prompt?: string;
  }): Promise<AssembleResult> {
    this.activeSessionId = params.sessionId;
    this.touchSession(params.sessionId);

    let records: MessageRecord[];
    if (params.messages && params.messages.length > 0) {
      records = this.reconcileMessages(params.sessionId, params.messages);
    } else {
      records = this.store.getMessages(params.sessionId);
    }

    let messages: AgentMessage[];
    if (params.messages && params.messages.length > 0) {
      messages = params.messages;
    } else {
      const stored = records;
      if (stored.length === 0) {
        return { messages: [], estimatedTokens: 0 };
      }
      messages = stored.map((message) => this.restoreMessage(message));
    }

    const tokenBudget =
      typeof params.tokenBudget === "number" && params.tokenBudget > 0
        ? params.tokenBudget
        : 128_000;

    const turns = this.deriveTurns(messages);
    this.cacheTurnMessages(params.sessionId, turns);

    const bindings = this.bindSummaryTurns(params.sessionId, turns, records);
    const summaries = await this.generateMissingSummaries(params.sessionId, turns, bindings);

    const result = this.assembler.assemble({
      messages,
      summaries,
      tokenBudget,
      freshTailTurns: this.config.freshTailTurns,
    });

    if (result.estimatedTokens > tokenBudget) {
      console.warn(
        `[selective-claw] assembled context (${result.estimatedTokens} tokens) exceeds budget (${tokenBudget})`
      );
    }

    return {
      messages: result.messages,
      estimatedTokens: result.estimatedTokens,
    };
  }

  async afterTurn(params: {
    sessionId: string;
    sessionKey?: string;
    messages?: AgentMessage[];
  }): Promise<void> {
    this.activeSessionId = params.sessionId;
    this.touchSession(params.sessionId);

    if (params.messages && params.messages.length > 0) {
      this.reconcileMessages(params.sessionId, params.messages);
    }

    const stored = this.store.getMessages(params.sessionId);
    if (stored.length === 0) return;

    const messages = stored.map((message) => this.restoreMessage(message));
    const turns = this.deriveTurns(messages);
    this.cacheTurnMessages(params.sessionId, turns);

    if (turns.length > this.config.freshTailTurns && this.summarizeFn) {
      const bindings = this.bindSummaryTurns(params.sessionId, turns, stored);
      await this.generateMissingSummaries(params.sessionId, turns, bindings);
    }
  }

  async compact(params: {
    sessionId: string;
    sessionKey?: string;
    tokenBudget?: number;
    force?: boolean;
  }): Promise<CompactResult> {
    return {
      ok: true,
      compacted: true,
      reason: "context managed by assemble",
    };
  }

  expandTurns(sessionId: string, turnSeqs: number[]): {
    found: number;
    turns: Array<{ turnSeq: number; messages: Array<{ role: string; content: string }> }>;
  } {
    const cache = this.turnMessagesCache.get(sessionId);
    const turnMap = new Map<number, Array<{ role: string; content: string }>>();
    for (const seq of turnSeqs) {
      const messages = cache?.get(seq);
      if (messages) {
        turnMap.set(seq, messages.map((message) => ({
          role: message.role,
          content: this.extractContent(message),
        })));
      }
    }

    const missingTurns = turnSeqs.filter((seq) => !turnMap.has(seq));
    const storeMessages = this.store.getMessagesByTurnSeqs(sessionId, missingTurns);
    for (const m of storeMessages) {
      const arr = turnMap.get(m.turnSeq) ?? [];
      arr.push({ role: m.role, content: m.content });
      turnMap.set(m.turnSeq, arr);
    }

    const result = turnSeqs
      .filter((ts) => turnMap.has(ts))
      .map((ts) => ({ turnSeq: ts, messages: turnMap.get(ts)! }));

    return { found: result.length, turns: result };
  }

  private reconcileMessages(sessionId: string, messages: AgentMessage[]): MessageRecord[] {
    const records = this.store.getMessages(sessionId);
    const stored = records.map(({ role, content }) => ({ role, content }));
    const incoming = messages.map((message) => ({
      role: this.normalizeRole(message.role),
      content: this.extractContent(message),
    }));

    let matchLen = 0;
    const minLen = Math.min(stored.length, messages.length);
    for (let i = 0; i < minLen; i++) {
      if (messagesMatch(stored[i], incoming[i])) {
        matchLen++;
      } else {
        break;
      }
    }

    const overlap = suffixOverlap(stored.slice(matchLen), incoming.slice(matchLen));
    const toImport = messages.slice(matchLen + overlap);
    this.importMessages(sessionId, toImport);
    const imported = this.store.getMessages(sessionId).slice(stored.length);
    return [
      ...records.slice(0, matchLen),
      ...records.slice(records.length - overlap),
      ...imported,
    ];
  }

  private importMessages(sessionId: string, messages: AgentMessage[]): void {
    let seq = this.store.getNextSeq(sessionId);
    let turnSeq = this.store.getMaxTurnSeq(sessionId);

    for (const msg of messages) {
      const role = this.normalizeRole(msg.role);
      const content = this.extractContent(msg);

      if (role === "user" || role === "system") {
        turnSeq++;
      } else if (turnSeq === 0) {
        turnSeq = 1;
      }

      this.store.createMessage({
        sessionId,
        seq,
        turnSeq,
        role,
        content,
        tokenCount: estimateTokens(content),
        rawMessage: JSON.stringify(msg),
      });
      seq++;
    }
  }

  private bindSummaryTurns(
    sessionId: string,
    turns: Array<{ turnSeq: number; messages: AgentMessage[] }>,
    records: MessageRecord[],
  ): Map<number, number> {
    const archived = new Map<number, MessageRecord[]>();
    for (const record of this.store.getMessages(sessionId)) {
      const group = archived.get(record.turnSeq) ?? [];
      group.push(record);
      archived.set(record.turnSeq, group);
    }

    const bindings = new Map<number, number>();
    let offset = 0;
    for (const turn of turns) {
      const group = records.slice(offset, offset + turn.messages.length);
      offset += turn.messages.length;
      const archiveTurn = group[0]?.turnSeq;
      if (archiveTurn === undefined) continue;
      const complete = archived.get(archiveTurn);
      // Partial or rewritten turns keep a live summary without replacing an archive anchor.
      if (
        complete && group.length === complete.length &&
        group.every((record, index) => record.seq === complete[index].seq)
      ) {
        bindings.set(turn.turnSeq, archiveTurn);
      }
    }
    return bindings;
  }

  private async generateMissingSummaries(
    sessionId: string,
    turns: Array<{ turnSeq: number; messages: AgentMessage[] }>,
    bindings: Map<number, number>,
  ): Promise<Map<number, string>> {
    const archived = this.getSummariesForSession(sessionId);
    const summaries = new Map<number, string>();
    for (const [liveTurn, archiveTurn] of bindings) {
      const summary = archived.get(archiveTurn);
      if (summary !== undefined) summaries.set(liveTurn, summary);
    }

    const olderTurns = turns.slice(0, -this.config.freshTailTurns);
    const summarizeFn = this.summarizeFn;
    if (!summarizeFn || olderTurns.length === 0) return summaries;
    const needSummary = olderTurns.filter((turn) => !summaries.has(turn.turnSeq));

    const results = await Promise.allSettled(
      needSummary.map((turn) => {
        const text = turn.messages
          .map((message) => `${message.role}: ${this.extractContent(message)}`)
          .join("\n");
        return summarizeFn(text).then((summary) => ({ turnSeq: turn.turnSeq, summary }));
      }),
    );

    for (const result of results) {
      if (result.status !== "fulfilled") continue;
      const { turnSeq, summary } = result.value;
      summaries.set(turnSeq, summary);
      const archiveTurn = bindings.get(turnSeq);
      if (archiveTurn === undefined) continue;
      archived.set(archiveTurn, summary);
      try {
        this.store.setTurnSummary(sessionId, archiveTurn, summary);
      } catch {
        // best-effort persist
      }
    }
    return summaries;
  }

  private cacheTurnMessages(
    sessionId: string,
    turns: Array<{ turnSeq: number; messages: AgentMessage[] }>,
  ): void {
    if (!this.turnMessagesCache.has(sessionId)) {
      this.turnMessagesCache.set(sessionId, new Map());
    }
    const cache = this.turnMessagesCache.get(sessionId)!;
    for (const turn of turns) {
      cache.set(turn.turnSeq, turn.messages);
    }
  }

  private touchSession(sessionId: string): void {
    const idx = this.sessionAccessOrder.indexOf(sessionId);
    if (idx !== -1) this.sessionAccessOrder.splice(idx, 1);
    this.sessionAccessOrder.push(sessionId);

    while (this.sessionAccessOrder.length > MAX_CACHED_SESSIONS) {
      const evicted = this.sessionAccessOrder.shift()!;
      this.turnSummaryCache.delete(evicted);
      this.turnMessagesCache.delete(evicted);
    }
  }

  private deriveTurns(messages: AgentMessage[]): Array<{ turnSeq: number; messages: AgentMessage[] }> {
    const turns: Array<{ turnSeq: number; messages: AgentMessage[] }> = [];
    let turnSeq = 0;
    let current: { turnSeq: number; messages: AgentMessage[] } | null = null;

    for (const msg of messages) {
      const role = this.normalizeRole(msg.role);
      if (role === "user" || role === "system") {
        turnSeq++;
        current = { turnSeq, messages: [] };
        turns.push(current);
      }
      if (!current) {
        turnSeq = 1;
        current = { turnSeq, messages: [] };
        turns.push(current);
      }
      current.messages.push(msg);
    }

    return turns;
  }

  private getSummariesForSession(sessionId: string): Map<number, string> {
    if (this.turnSummaryCache.has(sessionId)) {
      return this.turnSummaryCache.get(sessionId)!;
    }

    const map = new Map<number, string>();
    try {
      const stored = this.store.getTurnSummaries(sessionId);
      for (const s of stored) {
        map.set(s.turnSeq, s.summary);
      }
    } catch {
      // best-effort
    }
    this.turnSummaryCache.set(sessionId, map);
    return map;
  }

  private restoreMessage(message: MessageRecord): AgentMessage {
    if (message.rawMessage) {
      try {
        const raw = JSON.parse(message.rawMessage);
        if (raw && typeof raw === "object" && !Array.isArray(raw) && typeof raw.role === "string") {
          return raw as AgentMessage;
        }
      } catch {
        // Older archives may contain incomplete raw payloads.
      }
    }
    return { role: message.role, content: message.content };
  }

  private extractContent(message: AgentMessage): string {
    if (typeof message.content === "string") return message.content;
    if (Array.isArray(message.content)) {
      return message.content
        .map((block: any) => {
          if (typeof block === "string") return block;
          if (block?.type === "text" && typeof block.text === "string") return block.text;
          if (block?.type === "tool_result" && typeof block.output === "string") return block.output;
          return JSON.stringify(block);
        })
        .join("\n");
    }
    if (message.content != null) return JSON.stringify(message.content);
    return "";
  }

  private normalizeRole(role: string): "system" | "user" | "assistant" | "tool" {
    if (role === "toolResult" || role === "tool_result") return "tool";
    if (role === "system" || role === "user" || role === "assistant" || role === "tool") {
      return role;
    }
    return "user";
  }
}
