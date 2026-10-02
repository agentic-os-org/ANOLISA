import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { SelectiveContextEngine } from "../src/engine.js";
import { executeExpandTurn } from "../src/recall-tool.js";
import { createConnection, closeConnection } from "../src/db/connection.js";
import type { DatabaseSync } from "node:sqlite";
import type { AgentMessage } from "../src/openclaw-bridge.js";

/**
 * Simulates the gateway lifecycle: only assemble() and afterTurn() are called.
 * No bootstrap() or ingest() — messages arrive via params.messages.
 */
function buildMessages(turnCount: number): AgentMessage[] {
  const msgs: AgentMessage[] = [];
  for (let t = 1; t <= turnCount; t++) {
    msgs.push({ role: "user", content: `question about topic ${t}` });
    msgs.push({ role: "assistant", content: `answer about topic ${t}` });
  }
  return msgs;
}

describe("Integration: gateway lifecycle simulation", () => {
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;
  const SESSION = "integration-test";
  const FRESH_TAIL = 3;

  beforeEach(() => {
    db = createConnection(":memory:");
    engine = new SelectiveContextEngine(db, {
      freshTailTurns: FRESH_TAIL,
      dbPath: ":memory:",
      enabled: true,
    });
    engine.setSummarizeFn(async (text: string) => {
      const firstLine = text.split("\n")[0] ?? "";
      return `Summary: ${firstLine.slice(0, 50)}`;
    });
  });

  afterEach(() => {
    closeConnection(db);
  });

  // ─── 1. 消息入库（reconcile） ───

  describe("1. message reconciliation", () => {
    it("imports messages via assemble params (no bootstrap/ingest)", async () => {
      const messages = buildMessages(3);
      await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });

      expect(engine.getStore().getMessageCount(SESSION)).toBe(6);
    });

    it("assigns continuous turn_seq (1, 2, 3...)", async () => {
      const messages = buildMessages(4);
      await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });

      const turns = engine.getStore().getDistinctTurnSeqs(SESSION);
      expect(turns).toEqual([1, 2, 3, 4]);
    });

    it("incremental import does not duplicate messages", async () => {
      const messages3 = buildMessages(3);
      await engine.assemble({ sessionId: SESSION, messages: messages3, tokenBudget: 100000 });

      const messages5 = buildMessages(5);
      await engine.assemble({ sessionId: SESSION, messages: messages5, tokenBudget: 100000 });

      expect(engine.getStore().getMessageCount(SESSION)).toBe(10);
      expect(engine.getStore().getDistinctTurnSeqs(SESSION)).toEqual([1, 2, 3, 4, 5]);
    });

    it("handles gateway replacing messages (thinking block swap)", async () => {
      const messages: AgentMessage[] = [
        { role: "user", content: "q1" },
        { role: "assistant", content: "thinking..." },
        { role: "user", content: "q2" },
        { role: "assistant", content: "a2" },
      ];
      await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });

      const replaced: AgentMessage[] = [
        { role: "user", content: "q1" },
        { role: "assistant", content: "actual answer 1" },
        { role: "user", content: "q2" },
        { role: "assistant", content: "a2" },
        { role: "user", content: "q3" },
        { role: "assistant", content: "a3" },
      ];
      await engine.assemble({ sessionId: SESSION, messages: replaced, tokenBudget: 100000 });

      const count = engine.getStore().getMessageCount(SESSION);
      expect(count).toBeGreaterThanOrEqual(6);
    });
  });

  // ─── 2. 上下文裁剪（assemble） ───

  describe("2. context trimming (assemble)", () => {
    it("returns all messages when turns <= freshTailTurns", async () => {
      const messages = buildMessages(3);
      const result = await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });
      expect(result.messages).toHaveLength(6);
    });

    it("trims to summary + fresh tail when turns > freshTailTurns", async () => {
      const messages = buildMessages(6);
      await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });
      await engine.afterTurn({ sessionId: SESSION, messages });

      const result = await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });

      const summaryMsg = result.messages.find(
        (m) => typeof m.content === "string" && m.content.includes("[summary]"),
      );
      expect(summaryMsg).toBeDefined();

      const nonSummaryMessages = result.messages.filter(
        (m) => !(typeof m.content === "string" && m.content.includes("[summary]")),
      );
      expect(nonSummaryMessages).toHaveLength(6);
    });

    it("output message count stays bounded as conversation grows", async () => {
      const counts: number[] = [];
      for (let turn = 1; turn <= 10; turn++) {
        const messages = buildMessages(turn);
        await engine.afterTurn({ sessionId: SESSION, messages });
        const result = await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });
        counts.push(result.messages.length);
      }

      // First 3 turns: 2, 4, 6 messages (all in fresh tail)
      // Turn 4+: 6 fresh tail messages + 1 summary = 7
      for (let i = FRESH_TAIL; i < counts.length; i++) {
        expect(counts[i]).toBeLessThanOrEqual(FRESH_TAIL * 2 + 1);
      }
    });
  });

  // ─── 3. 摘要生成（afterTurn） ───

  describe("3. summary generation (afterTurn)", () => {
    it("generates summaries for turns outside freshTailTurns", async () => {
      const messages = buildMessages(6);
      await engine.afterTurn({ sessionId: SESSION, messages });

      const summaries = engine.getStore().getTurnSummaries(SESSION);
      expect(summaries.length).toBe(3);
      expect(summaries.map((s) => s.turnSeq)).toEqual([1, 2, 3]);
    });

    it("does not re-summarize existing summaries", async () => {
      let callCount = 0;
      engine.setSummarizeFn(async (text: string) => {
        callCount++;
        return `summary ${callCount}`;
      });

      const messages = buildMessages(5);
      await engine.afterTurn({ sessionId: SESSION, messages });
      const firstCount = callCount;

      await engine.afterTurn({ sessionId: SESSION, messages });
      expect(callCount).toBe(firstCount);
    });

    it("summaries appear in assemble output", async () => {
      const messages = buildMessages(6);
      await engine.afterTurn({ sessionId: SESSION, messages });

      const result = await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });
      const summaryMsg = result.messages.find(
        (m) => typeof m.content === "string" && m.content.includes("[summary]"),
      );
      expect(summaryMsg).toBeDefined();
      const content = summaryMsg!.content as string;
      expect(content).toContain("Turn 1:");
      expect(content).toContain("Turn 2:");
      expect(content).toContain("Turn 3:");
      expect(content).not.toContain("Turn 4:");
    });
  });

  // ─── 4. 展开工具（expand_turn） ───

  describe("4. expand_turn tool", () => {
    it("expands a compressed turn by turn_id", async () => {
      const messages = buildMessages(6);
      await engine.afterTurn({ sessionId: SESSION, messages });
  
      const result = executeExpandTurn(engine.getStore(), SESSION, [1]);
  
      expect(result.found).toBe(1);
      expect(result.turns[0].turnSeq).toBe(1);
      expect(result.turns[0].messages).toHaveLength(2);
      expect(result.turns[0].messages[0].role).toBe("user");
      expect(result.turns[0].messages[0].content).toBe("question about topic 1");
      expect(result.turns[0].messages[1].role).toBe("assistant");
      expect(result.turns[0].messages[1].content).toBe("answer about topic 1");
    });
  
    it("expands multiple compressed turns", async () => {
      const messages = buildMessages(6);
      await engine.afterTurn({ sessionId: SESSION, messages });
  
      const result = executeExpandTurn(engine.getStore(), SESSION, [1, 2, 3]);
      expect(result.found).toBe(3);
    });
  
    it("returns empty for non-existent turn_id", async () => {
      const messages = buildMessages(3);
      await engine.assemble({ sessionId: SESSION, messages, tokenBudget: 100000 });
  
      const result = executeExpandTurn(engine.getStore(), SESSION, [999]);
      expect(result.found).toBe(0);
    });
  });

  // ─── 5. compact 不阻塞 ───

  describe("5. compact does not block", () => {
    it("returns compacted: true", async () => {
      const result = await engine.compact({ sessionId: SESSION });
      expect(result.ok).toBe(true);
      expect(result.compacted).toBe(true);
    });
  });

  // ─── 6. 端到端多轮对话 ───

  describe("6. end-to-end multi-turn conversation", () => {
    it("full 8-turn lifecycle: reconcile → summarize → trim → expand", async () => {
      const TOTAL_TURNS = 8;

      // Simulate gateway calling assemble + afterTurn each round
      for (let turn = 1; turn <= TOTAL_TURNS; turn++) {
        const messages = buildMessages(turn);
        const assembleResult = await engine.assemble({
          sessionId: SESSION,
          messages,
          tokenBudget: 100000,
        });

        // assemble should always return something
        expect(assembleResult.messages.length).toBeGreaterThan(0);

        await engine.afterTurn({ sessionId: SESSION, messages });
      }

      const store = engine.getStore();

      // All 16 messages should be in DB
      expect(store.getMessageCount(SESSION)).toBe(TOTAL_TURNS * 2);

      // turn_seq should be 1..8
      const turnSeqs = store.getDistinctTurnSeqs(SESSION);
      expect(turnSeqs).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);

      // Summaries for turns 1-5 (8 - 3 = 5)
      const summaries = store.getTurnSummaries(SESSION);
      expect(summaries.length).toBe(TOTAL_TURNS - FRESH_TAIL);
      expect(summaries.map((s) => s.turnSeq)).toEqual([1, 2, 3, 4, 5]);

      // Final assemble: summary + 3 fresh turns = 7 messages
      const finalResult = await engine.assemble({
        sessionId: SESSION,
        messages: buildMessages(TOTAL_TURNS),
        tokenBudget: 100000,
      });
      const hasSummary = finalResult.messages.some(
        (m) => typeof m.content === "string" && m.content.includes("[summary]"),
      );
      expect(hasSummary).toBe(true);
      expect(finalResult.messages.length).toBe(FRESH_TAIL * 2 + 1);

      // expand_turn can retrieve any old turn
      const expanded = executeExpandTurn(store, SESSION, [1, 3, 5]);
      expect(expanded.found).toBe(3);
      expect(expanded.turns[0].messages[0].content).toBe("question about topic 1");
      expect(expanded.turns[1].messages[0].content).toBe("question about topic 3");
      expect(expanded.turns[2].messages[0].content).toBe("question about topic 5");

      // compact does not block
      const compactResult = await engine.compact({ sessionId: SESSION });
      expect(compactResult.compacted).toBe(true);
    });

    it("LLM calls expand_turn mid-conversation, toolResult enters next reconcile", async () => {
      // Phase 1: build 6 turns, generate summaries
      const phase1Messages = buildMessages(6);
      await engine.assemble({ sessionId: SESSION, messages: phase1Messages, tokenBudget: 100000 });
      await engine.afterTurn({ sessionId: SESSION, messages: phase1Messages });

      const store = engine.getStore();

      // Verify turns 1-3 are summarized
      const summaries = store.getTurnSummaries(SESSION);
      expect(summaries.map((s) => s.turnSeq)).toEqual([1, 2, 3]);

      // Phase 2: assemble returns summary + fresh tail
      const assembleResult = await engine.assemble({
        sessionId: SESSION,
        messages: phase1Messages,
        tokenBudget: 100000,
      });
      const summaryMsg = assembleResult.messages.find(
        (m) => typeof m.content === "string" && m.content.includes("[summary]"),
      );
      expect(summaryMsg).toBeDefined();
      expect((summaryMsg!.content as string)).toContain("Turn 1:");

      // Phase 3: LLM sees summary, calls expand_turn({ turn_ids: [1] })
      const expandResult = executeExpandTurn(store, SESSION, [1]);
      expect(expandResult.found).toBe(1);
      expect(expandResult.turns[0].messages[0].content).toBe("question about topic 1");

      // Phase 4: gateway sends back the conversation with tool call + toolResult + assistant response
      const phase2Messages: AgentMessage[] = [
        ...phase1Messages,
        { role: "user", content: "Tell me what was discussed in turn 1" },
        { role: "assistant", content: JSON.stringify({ tool: "expand_turn", args: { turn_ids: [1] } }) },
        { role: "toolResult", content: JSON.stringify(expandResult) },
        { role: "assistant", content: "In turn 1, you asked about topic 1 and I answered about topic 1." },
      ];
      await engine.assemble({ sessionId: SESSION, messages: phase2Messages, tokenBudget: 100000 });
      await engine.afterTurn({ sessionId: SESSION, messages: phase2Messages });

      // Verify: all messages including tool call are in DB
      expect(store.getMessageCount(SESSION)).toBe(phase2Messages.length);

      // Verify: turn_seq is still continuous
      const allTurns = store.getDistinctTurnSeqs(SESSION);
      expect(allTurns).toEqual([1, 2, 3, 4, 5, 6, 7]);

      // Verify: new summaries generated for turn 4 (7 turns - 3 fresh = 4 summarized)
      const newSummaries = store.getTurnSummaries(SESSION);
      expect(newSummaries.map((s) => s.turnSeq)).toEqual([1, 2, 3, 4]);

      // Verify: final assemble still returns bounded messages
      const finalResult = await engine.assemble({
        sessionId: SESSION,
        messages: phase2Messages,
        tokenBudget: 100000,
      });
      expect(finalResult.messages.length).toBeLessThanOrEqual(FRESH_TAIL * 2 + 2 + 1);
    });
  });
});
