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
  let warn: ReturnType<typeof vi.fn>;
  let factories: Record<string, (context: { sessionId?: string; sessionKey?: string }) => Tool>;

  beforeEach(() => {
    db = connection.createConnection(":memory:");
    vi.spyOn(connection, "createConnection").mockReturnValue(db);
    factories = {};
    warn = vi.fn();
    activate({
      config: { dbPath: ":memory:" },
      logger: { warn },
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
    expect(result.details).toEqual({ status: "ok", found: 1, truncated: false, nextOffset: null, matches: [
      { turnSeq: 2, seq: 2, role: "user", preview: "Docker compose deployment details" },
    ] });
    expect(JSON.parse(result.content[0].text)).toEqual(result.details);
    expect(result.content[0].text).toBe(JSON.stringify(result.details));
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
    expect((await factories.search_turns({ sessionId: "missing" }).execute("missing", { query: "Docker" })).details).toMatchObject({ status: "no_stored_messages", found: 0 });
  });

  it.each(["bootstrap", "ingest", "assemble", "compact", "afterTurn"] as const)(
    "binds both tools through the session key observed by %s",
    async (hook) => {
      await archive("session-a", ["Docker alpha"]);
      await engine[hook]({
        sessionId: "session-a", sessionKey: "key-a", messages: [],
        message: { role: "assistant", content: "Acknowledged" },
      });
      const search = factories.search_turns({ sessionId: "", sessionKey: "key-a" });
      const expand = factories.expand_turn({ sessionKey: "key-a" });
      await archive("session-b", ["Docker beta"]);
      const result = await search.execute("search", { query: "Docker" });
      expect(result.details.matches.map((match) => match.preview)).toEqual(["Docker alpha"]);
      const expanded = await expand.execute("expand", { turn_ids: [1] });
      expect(JSON.parse(expanded.content[0].text).turns[0].messages[0].content).toBe("Docker alpha");
    },
  );

  it("never substitutes an active session for unresolved tool contexts", async () => {
    const contexts = [{}, { sessionKey: "unknown" }, { sessionId: "" }];
    const searches = contexts.map((context) => factories.search_turns(context));
    const expands = contexts.map((context) => factories.expand_turn(context));
    await archive("session-b", ["Docker beta"]);
    for (const tool of searches) {
      const result = await tool.execute("search", { query: "Docker" });
      expect(result.details).toMatchObject({ status: "session_unresolved", found: 0, matches: [] });
    }
    for (const tool of expands) {
      const result = await tool.execute("expand", { turn_ids: [1] });
      expect(JSON.parse(result.content[0].text)).toEqual({ status: "session_unresolved", found: 0, turns: [] });
    }
  });

  it("pins a resolved key and prioritizes an explicit ID when a key is reused", async () => {
    await engine.bootstrap({ sessionId: "session-a", sessionKey: "key", messages: [{ role: "user", content: "Docker alpha" }] });
    const search = factories.search_turns({ sessionKey: "key" });
    const expand = factories.expand_turn({ sessionKey: "key" });
    await engine.bootstrap({ sessionId: "session-b", sessionKey: "key", messages: [{ role: "user", content: "Docker beta" }] });
    for (const tool of [search, factories.search_turns({ sessionId: "session-a", sessionKey: "key" })]) {
      expect((await tool.execute("search", { query: "Docker" })).details.matches[0].preview).toBe("Docker alpha");
    }
    const result = await expand.execute("expand", { turn_ids: [1] });
    expect(JSON.parse(result.content[0].text).turns[0].messages[0].content).toBe("Docker alpha");
    expect(result.content[0].text).toBe(JSON.stringify(result.details));
  });

  it("bounds result counts and orders equally ranked messages consistently", async () => {
    await archive("session-a", Array.from({ length: 25 }, () => "Docker"));
    const tool = factories.search_turns({ sessionId: "session-a" });
    for (const [limit, count] of [[undefined, 5], [2, 2], [20, 20]]) {
      const result = await tool.execute("limit", { query: "Docker", limit });
      expect(result.details.found).toBe(count);
      expect(result.details.status).toBe("ok");
      expect(result.details.truncated).toBe(true);
      expect(result.details.nextOffset).toBe(count);
      expect(result.details.matches.map((match) => match.seq)).toEqual(
        Array.from({ length: count! }, (_, index) => index + 1),
      );
    }
    expect(tool.parameters.properties.limit).toMatchObject({ minimum: 1, maximum: 20, default: 5 });
    const lastPage = await tool.execute("last-page", { query: "Docker", limit: 20, offset: 20 });
    expect(lastPage.details).toMatchObject({ status: "ok", found: 5, truncated: false, nextOffset: null });
    expect(lastPage.details.matches.map((match) => match.seq)).toEqual([21, 22, 23, 24, 25]);
    expect((await tool.execute("past-end", { query: "Docker", offset: 25 })).details).toEqual({ status: "ok", found: 0, matches: [], truncated: false, nextOffset: null });
  });

  it.each([0, 21, 2.5, NaN, Infinity, "3", null])("rejects invalid limits instead of changing the requested count: %s", async (limit) => {
    await archive("session-a", ["Docker"]);
    const result = await factories.search_turns({ sessionId: "session-a" }).execute("limit", { query: "Docker", limit });
    expect(result.details).toMatchObject({ status: "invalid_arguments", found: 0, matches: [] });
  });

  it.each([-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, "2", null])("rejects invalid offsets: %s", async (offset) => {
    await archive("session-a", ["Docker"]);
    const result = await factories.search_turns({ sessionId: "session-a" }).execute("offset", { query: "Docker", offset });
    expect(result.details).toMatchObject({ status: "invalid_arguments", found: 0, matches: [] });
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

  it("distinguishes invalid queries, missing context, successful misses and unavailable FTS", async () => {
    expect((await factories.search_turns({}).execute("no-context", { query: "Docker" })).details.status).toBe("session_unresolved");
    await archive("session-a", ["Docker"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    for (const query of ["", "  ", null]) {
      expect((await tool.execute("empty", { query })).details).toMatchObject({ status: "invalid_arguments", found: 0, matches: [] });
    }
    expect((await tool.execute("miss", { query: "Kubernetes" })).details).toMatchObject({ status: "ok", found: 0, matches: [] });
    db.exec("DROP TABLE messages_fts");
    expect((await tool.execute("unavailable", { query: "Docker" })).details).toMatchObject({ status: "search_unavailable", found: 0, matches: [] });
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("messages_fts"));
  });

  it("supports quoted phrases and literal FTS operator words", async () => {
    await archive("session-a", ["Docker compose release", "Docker release compose", "AND release"]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    expect((await tool.execute("phrase", { query: '"Docker compose"' })).details.matches.map((match) => match.turnSeq)).toEqual([1]);
    expect((await tool.execute("operator", { query: "AND" })).details.matches.map((match) => match.turnSeq)).toEqual([3]);
  });

  it("matches all terms within one stored message with case folding and stemming", async () => {
    await archive("session-a", ["Configure the DATABASE", "Database migrations", "Configure the network"]);
    expect(engine.getStore().getTurnSummaries("session-a")).toEqual([]);
    const tool = factories.search_turns({ sessionId: "session-a" });
    expect((await tool.execute("and", { query: "configuring databases" })).details.matches.map((match) => match.turnSeq)).toEqual([1]);
    expect((await tool.execute("separate-messages", { query: "network migrations" })).details).toMatchObject({ status: "ok", found: 0 });
    expect((await tool.execute("adjacency", { query: '"configure database"' })).details).toMatchObject({ status: "ok", found: 0 });
    expect((await tool.execute("stem", { query: "migration" })).details.matches.map((match) => match.turnSeq)).toEqual([2]);
  });
});
