import { describe, it, expect } from "vitest";
import { Assembler } from "../src/assembler.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";

function makeTurns(turnCount: number): AgentMessage[] {
  const msgs: AgentMessage[] = [];
  for (let t = 0; t < turnCount; t++) {
    msgs.push({ role: "user", content: `Turn ${t + 1} question about topic ${t + 1}` });
    msgs.push({ role: "assistant", content: `Turn ${t + 1} answer about topic ${t + 1}` });
  }
  return msgs;
}

describe("Assembler", () => {
  const assembler = new Assembler(3);

  it("returns empty messages for empty conversation", () => {
    const result = assembler.assemble({
      messages: [],
      summaries: new Map(),
      tokenBudget: 10000,
      freshTailTurns: 3,
    });
    expect(result.messages).toHaveLength(0);
    expect(result.estimatedTokens).toBe(0);
  });

  it("returns all messages when turns <= freshTailTurns", () => {
    const msgs = makeTurns(3);
    const result = assembler.assemble({
      messages: msgs,
      summaries: new Map(),
      tokenBudget: 100000,
      freshTailTurns: 3,
    });
    expect(result.messages).toHaveLength(6);
    expect(result.stats.totalTurns).toBe(3);
  });

  it("trims to fresh tail when turns > freshTailTurns", () => {
    const msgs = makeTurns(6);
    const result = assembler.assemble({
      messages: msgs,
      summaries: new Map(),
      tokenBudget: 100000,
      freshTailTurns: 3,
    });
    expect(result.stats.freshTailTurnCount).toBe(3);
    expect(result.stats.freshTailMessageCount).toBe(6);
    expect(result.stats.totalTurns).toBe(6);
  });

  it("inserts summary message when summaries exist", () => {
    const msgs = makeTurns(5);
    const summaries = new Map<number, string>([
      [1, "Discussed topic 1"],
      [2, "Discussed topic 2"],
    ]);

    const result = assembler.assemble({
      messages: msgs,
      summaries,
      tokenBudget: 100000,
      freshTailTurns: 3,
    });

    const summaryMsg = result.messages[0];
    expect(summaryMsg.role).toBe("user");
    expect(summaryMsg.content).toContain("[summary]");
    expect(summaryMsg.content).toContain("Turn 1: Discussed topic 1");
    expect(summaryMsg.content).toContain("Turn 2: Discussed topic 2");
    expect(result.stats.summaryCount).toBe(2);
  });

  it("does not include summary for recent turns in summary block", () => {
    const msgs = makeTurns(4);
    const summaries = new Map<number, string>([
      [1, "Old topic"],
      [4, "Recent topic"],
    ]);

    const result = assembler.assemble({
      messages: msgs,
      summaries,
      tokenBudget: 100000,
      freshTailTurns: 3,
    });

    const summaryMsg = result.messages[0];
    expect(summaryMsg.content).toContain("Turn 1: Old topic");
    expect(summaryMsg.content).not.toContain("Recent topic");
  });

  it("shows [no summary] for older turns without summaries", () => {
    const msgs = makeTurns(5);
    const result = assembler.assemble({
      messages: msgs,
      summaries: new Map(),
      tokenBudget: 100000,
      freshTailTurns: 3,
    });

    const hasFallback = result.messages.some((m) =>
      typeof m.content === "string" && m.content.includes("[no summary]")
    );
    expect(hasFallback).toBe(true);
    expect(result.stats.droppedTurns).toBe(2);
  });

  it("fresh tail messages preserve original order", () => {
    const msgs = makeTurns(5);
    const result = assembler.assemble({
      messages: msgs,
      summaries: new Map(),
      tokenBudget: 100000,
      freshTailTurns: 3,
    });

    const tailMsgs = result.messages.filter(
      (m) => typeof m.content !== "string" || !m.content.includes("[summary]")
    );
    expect(tailMsgs[0].content).toContain("Turn 3");
    expect(tailMsgs[tailMsgs.length - 1].content).toContain("Turn 5");
  });
});
