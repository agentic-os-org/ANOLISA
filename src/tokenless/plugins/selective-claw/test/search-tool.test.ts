import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveContextEngine } from "../src/engine.js";
import type { TurnSearchResult } from "../src/search-tool.js";

type Tool = {
  parameters: { properties: Record<string, unknown> };
  execute(id: string, params: Record<string, unknown>): Promise<{
    content: Array<{ type: string; text: string }>;
    details: TurnSearchResult;
  }>;
};

describe("registered search_turns workflow", () => {
  let db: DatabaseSync;
  let engine: SelectiveContextEngine;
  let factories: Record<string, (context: { sessionId?: string }) => Tool>;

  beforeEach(() => {
    db = connection.createConnection(":memory:");
    vi.spyOn(connection, "createConnection").mockReturnValue(db);
    factories = {};
    activate({
      config: { dbPath: ":memory:" },
      runtime: { llm: { complete: vi.fn() } },
      registerContextEngine(_id, factory) { engine = factory() as SelectiveContextEngine; },
      registerTool(factory: (context: { sessionId?: string }) => Tool, options: { name: string }) {
        factories[options.name] = factory;
      },
    });
  });

  afterEach(() => {
    connection.closeConnection(db);
    vi.restoreAllMocks();
  });

  async function archive(sessionId: string, contents: string[]) {
    await engine.bootstrap({ sessionId, messages: contents.map((content) => ({ role: "user", content })) });
  }

  it("discovers an archived turn and expands its original message", async () => {
    await archive("session-a", ["Plan the release", "Docker compose deployment details", "Another topic"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    const result = await tool.execute("search", { query: "Docker" });
    expect(result.details).toEqual({ found: 1, matches: [
      { turnSeq: 2, seq: 2, role: "user", preview: "Docker compose deployment details" },
    ] });
    expect(JSON.parse(result.content[0].text)).toEqual(result.details);
    const expanded = await factories.expand_turn({ sessionId: "session-a" }).execute("expand", {
      turn_ids: result.details.matches.map((match) => match.turnSeq),
    });
    expect(JSON.parse(expanded.content[0].text).turns[0].messages[0].content).toBe("Docker compose deployment details");
  });

  it("searches the invoking session after a different session becomes active", async () => {
    await archive("session-a", ["Docker alpha"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    await archive("session-b", ["Docker beta"]);
    const result = await tool.execute("search", { query: "Docker" });
    expect(result.details.matches.map((match) => match.preview)).toEqual(["Docker alpha"]);
    expect((await factories.search_turns({ sessionId: "missing" }).execute("missing", { query: "Docker" })).details.found).toBe(0);
  });

  it("bounds result counts and orders equally ranked messages consistently", async () => {
    await archive("session-a", Array.from({ length: 25 }, () => "Docker"));
    const tool = factories.search_turns({ sessionId: "session-a" });
    for (const [limit, count] of [[undefined, 5], [2, 2], [999, 20], [0, 1], [NaN, 5]]) {
      const result = await tool.execute("limit", { query: "Docker", limit });
      expect(result.details.found).toBe(count);
      expect(result.details.matches.map((match) => match.seq)).toEqual(
        Array.from({ length: count! }, (_, index) => index + 1),
      );
    }
    expect(tool.parameters.properties.limit).toMatchObject({ minimum: 1, maximum: 20, default: 5 });
  });

  it("returns short Unicode previews instead of full archived content", async () => {
    const text = `Docker ${"🙂".repeat(400)}`;
    await archive("session-a", [text]);
    const result = await factories.search_turns({ sessionId: "session-a" }).execute("preview", { query: "Docker" });
    const preview = result.details.matches[0].preview;
    expect(Array.from(preview)).toHaveLength(301);
    expect(preview).toBe(`Docker ${"🙂".repeat(293)}…`);
    expect(engine.getStore().getMessages("session-a")[0].content).toBe(text);
  });

  it("returns no matches for empty queries, missing context and unavailable FTS", async () => {
    expect((await factories.search_turns({}).execute("no-context", { query: "Docker" })).details).toEqual({ found: 0, matches: [] });
    await archive("session-a", ["Docker"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    for (const query of ["", "  ", null]) {
      expect((await tool.execute("empty", { query })).details).toEqual({ found: 0, matches: [] });
    }
    db.exec("DROP TABLE messages_fts");
    expect((await tool.execute("unavailable", { query: "Docker" })).details).toEqual({ found: 0, matches: [] });
  });

  it("supports quoted phrases and literal FTS operator words", async () => {
    await archive("session-a", ["Docker compose release", "Docker release compose", "AND release"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    expect((await tool.execute("phrase", { query: '"Docker compose"' })).details.matches.map((match) => match.turnSeq)).toEqual([1]);
    expect((await tool.execute("operator", { query: "AND" })).details.matches.map((match) => match.turnSeq)).toEqual([3]);
  });
});
