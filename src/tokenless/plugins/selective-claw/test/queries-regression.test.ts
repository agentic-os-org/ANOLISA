import { expect, it, vi } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { SelectiveContextEngine } from "../src/engine.js";

it("reads the archive only once while assembling or completing a suffix window", async () => {
  const db = new DatabaseSync(":memory:");
  try {
    const engine = new SelectiveContextEngine(db, { freshTailTurns: 3, dbPath: ":memory:", enabled: true });
    const messages = Array.from({ length: 1000 }, (_, index) => ({ role: "user", content: `message-${index}` }));
    await engine.bootstrap({ sessionId: "s", messages });
    engine.setSummarizeFn(async (text) => `SUMMARY(${text})`);
    const reads = vi.spyOn(engine.getStore(), "getMessages");
    const result = await engine.assemble({ sessionId: "s", messages: messages.slice(-4) });
    expect(result.messages[0].content).toContain("Turn 1: SUMMARY(user: message-996)");
    expect(reads).toHaveBeenCalledTimes(1);
    reads.mockClear();
    await engine.afterTurn({ sessionId: "s", messages: messages.slice(-4) });
    expect(reads).toHaveBeenCalledTimes(1);
    expect(engine.getStore().getMessageCount("s")).toBe(1000);
  } finally {
    db.close();
  }
});
