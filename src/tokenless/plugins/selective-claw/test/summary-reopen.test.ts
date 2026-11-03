import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it, vi } from "vitest";
import { createConnection, closeConnection } from "../src/db/connection.js";
import { SelectiveContextEngine } from "../src/engine.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";

it.each(["system", "assistant", "tool", "toolResult", "tool_result"])(
  "keeps a leading %s summary distinct from the first user turn after archive reopen",
  async (role) => {
    const directory = mkdtempSync(join(tmpdir(), "selective-claw-summary-"));
    const config = { freshTailTurns: 3, dbPath: join(directory, "archive.db"), enabled: true };
    const sessionId = "summary-reopen";
    let db = createConnection(config.dbPath);
    try {
      const engine = new SelectiveContextEngine(db, config);
      const summarize = vi.fn(async (text: string) => `Summary: ${text}`);
      engine.setSummarizeFn(summarize);
      const messages: AgentMessage[] = [{ role, content: "leading context" }];
      for (let turn = 1; turn <= 3; turn++) {
        messages.push(
          { role: "user", content: `question ${turn}` },
          { role: "assistant", content: `answer ${turn}` },
        );
      }
      await engine.afterTurn({ sessionId, messages });
      expect(summarize).toHaveBeenCalledTimes(1);

      messages.push(
        { role: "user", content: "question 4" },
        { role: "assistant", content: "answer 4" },
      );
      await engine.afterTurn({ sessionId, messages });
      expect(summarize).toHaveBeenCalledTimes(2);
      const summaries = [
        { turnSeq: 1, summary: `Summary: ${role}: leading context` },
        { turnSeq: 2, summary: "Summary: user: question 1\nassistant: answer 1" },
      ];
      expect(engine.getStore().getTurnSummaries(sessionId)).toEqual(summaries);

      closeConnection(db);
      db = createConnection(config.dbPath);
      const restored = new SelectiveContextEngine(db, config);
      const regenerate = vi.fn(async () => "unexpected replacement");
      restored.setSummarizeFn(regenerate);
      expect(restored.getStore().getTurnSummaries(sessionId)).toEqual(summaries);
      await restored.afterTurn({ sessionId, messages: [] });

      for (const input of [[], messages]) {
        const result = await restored.assemble({ sessionId, messages: input, tokenBudget: 100_000 });
        expect(result.messages[0].content).toBe([
          "[summary] Earlier conversation context:",
          ...summaries.map(({ turnSeq, summary }) => `Turn ${turnSeq}: ${summary}`),
        ].join("\n"));
        expect(result.messages.slice(1)).toEqual(messages.slice(3));
      }
      expect(regenerate).not.toHaveBeenCalled();
    } finally {
      closeConnection(db);
      rmSync(directory, { recursive: true, force: true });
    }
  },
);
