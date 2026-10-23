import type { AgentMessage } from "./openclaw-bridge.js";
import { estimateTokens, estimateTokenWeight } from "./estimate-tokens.js";
import { TokenBudgetExceededError } from "./budget-error.js";

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
    /** Admitted fallback previews whose turns have no summary. */
    droppedTurns: number;
    /** Oldest prefix omitted because it could not fit the budget. */
    omittedTurns: number;
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
      turnSeq = 1;
      current = { turnSeq, messages: [], tokenCount: 0 };
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
    if (!Number.isFinite(input.tokenBudget) || input.tokenBudget <= 0) {
      throw new TokenBudgetExceededError(0, input.tokenBudget, 0, 0);
    }
    const freshTailTurns = input.freshTailTurns ?? this.defaultFreshTailTurns;
    const turns = deriveTurns(input.messages);

    if (turns.length === 0) {
      return {
        messages: [],
        estimatedTokens: 0,
        stats: {
          totalTurns: 0,
          freshTailTurnCount: 0,
          freshTailMessageCount: 0,
          summaryCount: 0,
          droppedTurns: 0,
          omittedTurns: 0,
        },
      };
    }

    if (turns.length <= freshTailTurns) {
      const allTokens = turns.reduce((s, t) => s + t.tokenCount, 0);
      assertTailFits(
        allTokens,
        input.tokenBudget,
        turns.length,
        input.messages.length,
      );
      return {
        messages: input.messages,
        estimatedTokens: allTokens,
        stats: {
          totalTurns: turns.length,
          freshTailTurnCount: turns.length,
          freshTailMessageCount: input.messages.length,
          summaryCount: 0,
          droppedTurns: 0,
          omittedTurns: 0,
        },
      };
    }

    const tail = turns.slice(-freshTailTurns);
    const older = turns.slice(0, -freshTailTurns);

    const tailTokens = tail.reduce((sum, turn) => sum + turn.tokenCount, 0);
    assertTailFits(
      tailTokens,
      input.tokenBudget,
      tail.length,
      tail.reduce((sum, turn) => sum + turn.messages.length, 0),
    );
    const summaryLines: string[] = [];
    let droppedTurns = 0;
    const header = "[summary] Earlier conversation context:";
    let summaryWeight = estimateTokenWeight(
      JSON.stringify({ role: "user", content: header }),
    );
    for (const turn of [...older].reverse()) {
      const summary = input.summaries.get(turn.turnSeq);
      const preview = summary
        ? ""
        : turn.messages
            .map((message) => extractText(message).slice(0, 60))
            .join(" | ");
      const line = `Turn ${turn.turnSeq}: ${summary || `[no summary] ${preview}...`}`;
      const lineWeight = escapedContentWeight("\n" + line);
      if (
        tailTokens + Math.ceil(summaryWeight + lineWeight) >
        input.tokenBudget
      ) {
        // Preserve a contiguous newest suffix rather than hiding a gap in history.
        break;
      }
      summaryLines.push(line);
      summaryWeight += lineWeight;
      if (!summary) droppedTurns++;
    }
    const omittedTurns = older.length - summaryLines.length;
    const marker = `Turns 1-${omittedTurns}: [omitted to fit token budget; use expand_turn]`;
    // Never displace an admitted summary or exceed the budget just to annotate a gap.
    const includeMarker =
      omittedTurns > 0 &&
      tailTokens +
        Math.ceil(summaryWeight + escapedContentWeight("\n" + marker)) <=
        input.tokenBudget;

    summaryLines.reverse();
    const result: AgentMessage[] = [];

    if (summaryLines.length > 0 || includeMarker) {
      result.push({
        role: "user",
        content: [
          header,
          ...(includeMarker ? [marker] : []),
          ...summaryLines,
        ].join("\n"),
      });
    }

    for (const turn of tail) {
      result.push(...turn.messages);
    }

    const estimatedTokens = result.reduce(
      (total, message) => total + estimateTokens(JSON.stringify(message)),
      0,
    );

    return {
      messages: result,
      estimatedTokens,
      stats: {
        totalTurns: turns.length,
        freshTailTurnCount: tail.length,
        freshTailMessageCount: tail.reduce((s, t) => s + t.messages.length, 0),
        summaryCount: summaryLines.length,
        droppedTurns,
        omittedTurns,
      },
    };
  }
}

function assertTailFits(
  tokens: number,
  budget: number,
  turns: number,
  messages: number,
): void {
  if (tokens > budget) {
    throw new TokenBudgetExceededError(tokens, budget, turns, messages);
  }
}

function escapedContentWeight(text: string): number {
  // Match the final JSON string's escape rules, excluding only its enclosing quotes.
  return estimateTokenWeight(JSON.stringify(text).slice(1, -1));
}

function extractText(msg: AgentMessage): string {
  if (typeof msg.content === "string") return msg.content;
  if (Array.isArray(msg.content)) {
    return msg.content
      .map((block: any) => {
        if (typeof block === "string") return block;
        if (block?.type === "text" && typeof block.text === "string")
          return block.text;
        return "";
      })
      .join(" ");
  }
  return "";
}
