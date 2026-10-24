import { expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { SelectiveContextEngine } from "../src/engine.js";

it("bounds summary requests across simultaneous sessions and releases failed slots", async () => {
  const db = new DatabaseSync(":memory:");
  try {
    const engine = new SelectiveContextEngine(db, { freshTailTurns: 3, dbPath: ":memory:", enabled: true });
    let active = 0;
    let maximum = 0;
    let calls = 0;
    engine.setSummarizeFn(async (text) => {
      calls++;
      active++;
      maximum = Math.max(maximum, active);
      try {
        await new Promise((resolve) => setTimeout(resolve, 2));
        if (text.includes("question-0")) throw new Error("provider unavailable");
        return `SUMMARY(${text})`;
      } finally {
        active--;
      }
    });
    const messages = Array.from({ length: 20 }, (_, index) => ({ role: "user", content: `question-${index}` }));
    await Promise.all(["a", "b"].map((sessionId) => engine.assemble({ sessionId, messages })));
    expect(calls).toBe(34);
    expect(maximum).toBeGreaterThan(1);
    expect(maximum).toBeLessThanOrEqual(4);
    expect(active).toBe(0);
    expect(engine.getStore().getTurnSummaries("a")).toHaveLength(16);
    expect(engine.getStore().getTurnSummaries("b")).toHaveLength(16);
  } finally {
    db.close();
  }
});
