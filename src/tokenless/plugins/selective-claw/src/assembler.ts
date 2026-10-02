import type { AgentMessage } from "./openclaw-bridge.js";
import { estimateTokens } from "./estimate-tokens.js";

export interface AssembleInput {
  messages: AgentMessage[];
  summaries: Map<number, string>;
  tokenBudget: number;
  freshTailTurns: number;
}

export interface AssembleResult {
  messages: AgentMessage[];
  estimatedTokens: number;
  stats: {
    totalTurns: number;
    freshTailTurnCount: number;
    freshTailMessageCount: number;
    summaryCount: number;
    droppedTurns: number;
  };
}

interface Turn {
  turnSeq: number;
  messages: AgentMessage[];
  tokenCount: number;
}

function deriveTurns(messages: AgentMessage[]): Turn[] {
  const turns: Turn[] = [];
  let turnSeq = 0;
  let current: Turn | null = null;

  for (const msg of messages) {
    const role = normalizeRole(msg.role);
    if (role === "user" || role === "system") {
      turnSeq++;
      current = { turnSeq, messages: [], tokenCount: 0 };
      turns.push(current);
    }
    if (!current) {
      current = { turnSeq: 1, messages: [], tokenCount: 0 };
      turns.push(current);
    }
    const tokens = estimateTokens(JSON.stringify(msg));
    current.messages.push(msg);
    current.tokenCount += tokens;
  }

  return turns;
}

function normalizeRole(role: string): string {
  if (role === "toolResult" || role === "tool_result") return "tool";
  return role;
}

export class Assembler {
  constructor(private defaultFreshTailTurns: number = 3) {}

  assemble(input: AssembleInput): AssembleResult {
    const freshTailTurns = input.freshTailTurns ?? this.defaultFreshTailTurns;
    const turns = deriveTurns(input.messages);

    if (turns.length === 0) {
      return {
        messages: [],
        estimatedTokens: 0,
        stats: { totalTurns: 0, freshTailTurnCount: 0, freshTailMessageCount: 0, summaryCount: 0, droppedTurns: 0 },
      };
    }

    if (turns.length <= freshTailTurns) {
      const allTokens = turns.reduce((s, t) => s + t.tokenCount, 0);
      return {
        messages: input.messages,
        estimatedTokens: allTokens,
        stats: {
          totalTurns: turns.length,
          freshTailTurnCount: turns.length,
          freshTailMessageCount: input.messages.length,
          summaryCount: 0,
          droppedTurns: 0,
        },
      };
    }

    const tail = turns.slice(-freshTailTurns);
    const older = turns.slice(0, -freshTailTurns);

    const summaryLines: string[] = [];
    let droppedTurns = 0;
    for (const turn of older) {
      const summary = input.summaries.get(turn.turnSeq);
      if (summary) {
        summaryLines.push(`Turn ${turn.turnSeq}: ${summary}`);
      } else {
        droppedTurns++;
        const preview = turn.messages
          .map((m) => extractText(m).slice(0, 60))
          .join(" | ");
        summaryLines.push(`Turn ${turn.turnSeq}: [no summary] ${preview}...`);
      }
    }

    const result: AgentMessage[] = [];

    if (summaryLines.length > 0) {
      result.push({
        role: "user",
        content: ["[summary] Earlier conversation context:", ...summaryLines].join("\n"),
      });
    }

    for (const turn of tail) {
      result.push(...turn.messages);
    }

    const tailTokens = tail.reduce((s, t) => s + t.tokenCount, 0);

    return {
      messages: result,
      estimatedTokens: tailTokens,
      stats: {
        totalTurns: turns.length,
        freshTailTurnCount: tail.length,
        freshTailMessageCount: tail.reduce((s, t) => s + t.messages.length, 0),
        summaryCount: summaryLines.length,
        droppedTurns,
      },
    };
  }
}

function extractText(msg: AgentMessage): string {
  if (typeof msg.content === "string") return msg.content;
  if (Array.isArray(msg.content)) {
    return msg.content
      .map((block: any) => {
        if (typeof block === "string") return block;
        if (block?.type === "text" && typeof block.text === "string") return block.text;
        return "";
      })
      .join(" ");
  }
  return "";
}
