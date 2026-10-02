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
import { Assembler } from "./assembler.js";
import { estimateTokens } from "./estimate-tokens.js";

const MAX_CACHED_SESSIONS = 10;

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

    if (params.messages && params.messages.length > 0) {
      this.reconcileMessages(params.sessionId, params.messages);
    }

    let messages: AgentMessage[];
    if (params.messages && params.messages.length > 0) {
      messages = params.messages;
    } else {
      const stored = this.store.getMessages(params.sessionId);
      if (stored.length === 0) {
        return { messages: [], estimatedTokens: 0 };
      }
      messages = stored.map((m) => ({
        role: m.role as string,
        content: m.content,
      }));
    }

    const tokenBudget =
      typeof params.tokenBudget === "number" && params.tokenBudget > 0
        ? params.tokenBudget
        : 128_000;

    const turns = this.deriveTurns(messages);
    this.cacheTurnMessages(params.sessionId, turns);

    if (turns.length > this.config.freshTailTurns && this.summarizeFn) {
      await this.generateMissingSummaries(params.sessionId, turns);
    }

    const summaries = this.getSummariesForSession(params.sessionId);

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

    const messages: AgentMessage[] = stored.map((m) => ({
      role: m.role as string,
      content: m.content,
    }));
    const turns = this.deriveTurns(messages);
    this.cacheTurnMessages(params.sessionId, turns);

    if (turns.length > this.config.freshTailTurns && this.summarizeFn) {
      await this.generateMissingSummaries(params.sessionId, turns);
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
    if (cache) {
      const result: Array<{ turnSeq: number; messages: Array<{ role: string; content: string }> }> = [];
      for (const seq of turnSeqs) {
        const msgs = cache.get(seq);
        if (msgs) {
          result.push({
            turnSeq: seq,
            messages: msgs.map((m) => ({
              role: m.role,
              content: this.extractContent(m),
            })),
          });
        }
      }
      if (result.length > 0) return { found: result.length, turns: result };
    }

    const storeMessages = this.store.getMessagesByTurnSeqs(sessionId, turnSeqs);
    const turnMap = new Map<number, Array<{ role: string; content: string }>>();
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

  private reconcileMessages(sessionId: string, messages: AgentMessage[]): void {
    const stored = this.store.getMessages(sessionId);

    let matchLen = 0;
    const minLen = Math.min(stored.length, messages.length);
    for (let i = 0; i < minLen; i++) {
      const storedRole = stored[i].role;
      const incomingRole = this.normalizeRole(messages[i].role);
      const incomingContent = this.extractContent(messages[i]);
      if (storedRole === incomingRole && stored[i].content === incomingContent) {
        matchLen++;
      } else {
        break;
      }
    }

    if (matchLen < messages.length) {
      const toImport = messages.slice(matchLen);
      this.importMessages(sessionId, toImport);
    }
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
      });
      seq++;
    }
  }

  private async generateMissingSummaries(
    sessionId: string,
    turns: Array<{ turnSeq: number; messages: AgentMessage[] }>,
  ): Promise<void> {
    const olderTurns = turns.slice(0, -this.config.freshTailTurns);
    if (olderTurns.length === 0) return;

    const summaries = this.getSummariesForSession(sessionId);
    const needSummary = olderTurns.filter((t) => !summaries.has(t.turnSeq));
    if (needSummary.length === 0) return;

    const summarizeFn = this.summarizeFn!;

    const results = await Promise.allSettled(
      needSummary.map((turn) => {
        const text = turn.messages
          .map((m) => `${m.role}: ${this.extractContent(m)}`)
          .join("\n");
        return summarizeFn(text).then((summary) => ({ turnSeq: turn.turnSeq, summary }));
      }),
    );

    if (!this.turnSummaryCache.has(sessionId)) {
      this.turnSummaryCache.set(sessionId, new Map());
    }
    const cache = this.turnSummaryCache.get(sessionId)!;

    for (const r of results) {
      if (r.status === "fulfilled") {
        cache.set(r.value.turnSeq, r.value.summary);
        try {
          this.store.setTurnSummary(sessionId, r.value.turnSeq, r.value.summary);
        } catch {
          // best-effort persist
        }
      }
    }
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
        current = { turnSeq: 1, messages: [] };
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
