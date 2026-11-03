import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveContextEngine } from "../src/engine.js";
import type { RecallResult } from "../src/recall-tool.js";

type ToolFactory = (context: { sessionId?: string }) => {
  execute(id: string, params: { turn_ids: number[] }): Promise<{ details: RecallResult }>;
};

describe("registered expand_turn tool", () => {
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;
  let toolFactory: ToolFactory;

  beforeEach(() => {
    db = connection.createConnection(":memory:");
    vi.spyOn(connection, "createConnection").mockReturnValue(db);
    activate({
      config: { dbPath: ":memory:" },
      runtime: { llm: { complete: vi.fn() } },
      registerContextEngine(_id, factory) {
        engine = factory() as SelectiveContextEngine;
      },
      registerTool(factory: ToolFactory, options: { name: string }) {
        if (options.name === "expand_turn") toolFactory = factory;
      },
    });
  });

  afterEach(() => {
    connection.closeConnection(db);
    vi.restoreAllMocks();
  });

  async function activateSession(sessionId: string) {
    await engine.assemble({
      sessionId,
      messages: [
        { role: "user", content: `${sessionId} question` },
        { role: "assistant", content: `${sessionId} answer` },
      ],
    });
  }

  it("keeps a factory-bound session when another session becomes active", async () => {
    await activateSession("session-a");
    const toolA = toolFactory({ sessionId: "session-a" });
    await activateSession("session-b");
    const toolB = toolFactory({ sessionId: "session-b" });

    const resultA = await toolA.execute("call-a", { turn_ids: [1] });
    const resultB = await toolB.execute("call-b", { turn_ids: [1] });
    expect(resultA.details.turns[0].messages.map((message) => message.content)).toEqual([
      "session-a question", "session-a answer",
    ]);
    expect(resultB.details.turns[0].messages.map((message) => message.content)).toEqual([
      "session-b question", "session-b answer",
    ]);
  });

  it("does not substitute another active session for an unknown bound session", async () => {
    await activateSession("session-a");
    const result = await toolFactory({ sessionId: "not-imported" }).execute("missing", { turn_ids: [1] });
    expect(result.details).toEqual({ found: 0, turns: [] });
  });

  it("preserves active-session fallback when the host supplies no session ID", async () => {
    await activateSession("session-a");
    const result = await toolFactory({}).execute("legacy-host", { turn_ids: [1] });
    expect(result.details.found).toBe(1);
    expect(result.details.turns[0].messages[0].content).toBe("session-a question");
  });
});
