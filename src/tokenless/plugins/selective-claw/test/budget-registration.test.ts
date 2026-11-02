import { afterEach, describe, expect, it, vi } from "vitest";
import activate from "../index.js";
import { SelectiveContextEngine } from "../src/engine.js";
import type { AgentMessage } from "../src/openclaw-bridge.js";
import { estimateTokens } from "../src/estimate-tokens.js";
import * as connection from "../src/db/connection.js";
import type { DatabaseSync } from "node:sqlite";

const databases: DatabaseSync[] = [];
afterEach(() => {
  for (const db of databases.splice(0)) db.close();
  vi.restoreAllMocks();
});

function register(summary = "short summary") {
  const open = connection.createConnection;
  vi.spyOn(connection, "createConnection").mockImplementation((path) => {
    const db = open(path);
    databases.push(db);
    return db;
  });
  let engine: SelectiveContextEngine | undefined;
  let toolFactory: any;
  const complete = vi.fn(async () => ({ text: summary }));
  activate({
    pluginConfig: { dbPath: ":memory:", freshTailTurns: 1 },
    runtime: { llm: { complete } },
    registerContextEngine: (_id, factory) => {
      engine = factory() as SelectiveContextEngine;
    },
    registerTool: (factory: any) => {
      toolFactory = factory;
    },
  });
  expect(engine).toBeDefined();
  return {
    engine: engine!,
    complete,
    tool: toolFactory({ sessionId: "budget" }),
  };
}

describe("registered context engine budget", () => {
  it("bounds summaries while keeping tool envelopes and archived recall intact", async () => {
    const { engine, complete, tool } = register(
      "Detailed summary ".repeat(300),
    );
    const recent: AgentMessage[] = [
      { role: "user", content: "inspect the fixture" },
      {
        role: "assistant",
        content: [
          {
            type: "toolCall",
            id: "c1",
            name: "read",
            arguments: { path: "fixture" },
          },
        ],
      },
      {
        role: "toolResult",
        toolCallId: "c1",
        content: [{ type: "text", text: "完整结果" }],
        isError: false,
      },
    ];
    const messages = [
      { role: "user", content: "archived question" },
      { role: "assistant", content: "archived answer" },
      ...recent,
    ];
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const result = await engine.assemble({
      sessionId: "budget",
      messages,
      tokenBudget: 180,
    });
    expect(result.estimatedTokens).toBeLessThanOrEqual(180);
    expect(result.estimatedTokens).toBe(
      result.messages.reduce(
        (sum, message) => sum + estimateTokens(JSON.stringify(message)),
        0,
      ),
    );
    for (let i = 0; i < recent.length; i++)
      expect(result.messages.at(i - recent.length)).toBe(recent[i]);
    expect(complete).toHaveBeenCalledOnce();
    const recalled = await tool.execute("expand", { turn_ids: [1] });
    expect(recalled.details.found).toBe(1);
    expect(
      recalled.details.turns[0].messages.map((message: any) => message.content),
    ).toEqual(["archived question", "archived answer"]);
  });

  it.each([false, true])(
    "rejects an oversized protected tail (compressed=%s) and permits a larger retry",
    async (compressed) => {
      const { engine, complete } = register();
      const prefix = compressed
        ? [
            { role: "user", content: "old" },
            { role: "assistant", content: "answer" },
          ]
        : [];
      const recent = { role: "user", content: "recent ".repeat(400) };
      const messages = [...prefix, recent];
      vi.spyOn(console, "warn").mockImplementation(() => {});
      await expect(
        engine.assemble({ sessionId: "budget", messages, tokenBudget: 100 }),
      ).rejects.toThrow(RangeError);
      expect(engine.getStore().getMessageCount("budget")).toBe(messages.length);
      const calls = complete.mock.calls.length;
      const retried = await engine.assemble({
        sessionId: "budget",
        messages,
        tokenBudget: 2000,
      });
      expect(retried.estimatedTokens).toBeLessThanOrEqual(2000);
      expect(retried.messages.at(-1)).toBe(recent);
      expect(complete.mock.calls.length).toBe(calls);
    },
  );

  it("preserves ordinary assembly when the whole context fits", async () => {
    const { engine } = register();
    const messages = [
      { role: "user", content: "one" },
      { role: "assistant", content: "two" },
    ];
    const result = await engine.assemble({
      sessionId: "budget",
      messages,
      tokenBudget: 100,
    });
    expect(result.messages).toEqual(messages);
    expect(result.messages[0]).toBe(messages[0]);
    expect(result.messages[1]).toBe(messages[1]);
  });
});

it.each([undefined, NaN, Infinity, -Infinity, 0, -1])(
  "uses the documented 128000 fallback for host budget %s",
  async (tokenBudget) => {
    const { engine } = register();
    vi.spyOn(console, "warn").mockImplementation(() => {});
    // This one-message estimate is exactly the fallback, including its JSON envelope.
    const message = { role: "user", content: "x".repeat(511972) };
    expect(estimateTokens(JSON.stringify(message))).toBe(128000);
    const result = await engine.assemble({
      sessionId: "budget",
      messages: [message],
      tokenBudget,
    });
    expect(result.estimatedTokens).toBe(128000);
    const tooLarge = { ...message, content: message.content + "xxxx" };
    await expect(
      engine.assemble({
        sessionId: "budget",
        messages: [tooLarge],
        tokenBudget,
      }),
    ).rejects.toMatchObject({
      name: "TokenBudgetExceededError",
      tokenBudget: 128000,
      requiredTokens: 128001,
    });
  },
);

it("logs omissions and preserves the archive when the prompt cannot fit a marker", async () => {
  const { engine, tool } = register("large summary ".repeat(100));
  const messages = [
    { role: "user", content: "old" },
    { role: "assistant", content: "answer" },
    { role: "user", content: "tail" },
  ];
  const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
  const tokenBudget = estimateTokens(JSON.stringify(messages[2]));
  const result = await engine.assemble({
    sessionId: "budget",
    messages,
    tokenBudget,
  });
  expect(result.messages).toEqual([messages[2]]);
  expect(result.estimatedTokens).toBe(tokenBudget);
  expect(warning).toHaveBeenCalledWith(
    expect.stringContaining("omitted turns 1-1"),
  );
  expect(engine.getStore().getMessageCount("budget")).toBe(3);
  const recalled = await tool.execute("expand", { turn_ids: [1] });
  expect(recalled.details.turns[0].messages.map((m: any) => m.content)).toEqual(
    ["old", "answer"],
  );
});
