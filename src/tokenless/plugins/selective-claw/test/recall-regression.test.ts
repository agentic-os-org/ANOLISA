import { afterEach, beforeEach, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { SelectiveContextEngine } from "../src/engine.js";

const config = { freshTailTurns: 3, dbPath: ":memory:", enabled: true };
const history = Array.from({ length: 6 }, (_, index) => [
  { role: "user", content: `question-${index + 1}` },
  { role: "assistant", content: `answer-${index + 1}` },
]).flat();
let db: DatabaseSync;
let engine: SelectiveContextEngine;
beforeEach(() => {
  db = new DatabaseSync(":memory:");
  engine = new SelectiveContextEngine(db, config);
  engine.setSummarizeFn(async (text) => `SUMMARY(${text})`);
});
afterEach(() => db.close());

it("keeps suffix-window recall identities after cache eviction and engine restart", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  const result = await engine.assemble({ sessionId: "s", messages: history.slice(4) });
  expect(result.messages[0].content).toContain("Turn 1: SUMMARY(user: question-3");
  for (let index = 0; index < 10; index++) {
    await engine.bootstrap({ sessionId: `other-${index}` });
  }
  for (const reader of [engine, new SelectiveContextEngine(db, config)]) {
    const recalled = reader.expandTurns("s", [1]);
    expect(recalled.found).toBe(1);
    expect(recalled.turns[0].messages.map((message) => message.content)).toEqual(["question-3", "answer-3"]);
  }
});

it("never substitutes an unrelated archived turn for a partial live turn", async () => {
  await engine.bootstrap({ sessionId: "s", messages: history });
  await engine.assemble({ sessionId: "s", messages: history.slice(5) });
  expect(engine.expandTurns("s", [1]).turns[0].messages[0].content).toBe("answer-3");
  const restored = new SelectiveContextEngine(db, config);
  expect(restored.expandTurns("s", [1])).toEqual({ found: 0, turns: [] });
});
