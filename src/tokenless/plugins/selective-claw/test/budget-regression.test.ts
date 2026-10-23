import { TokenBudgetExceededError } from "../index.js";
import { expect, it } from "vitest";
import { Assembler } from "../src/assembler.js";
import { estimateTokens } from "../src/estimate-tokens.js";

it("fits historical summaries without changing structured fresh-tail messages", () => {
  const recent = {
    role: "toolResult",
    toolCallId: "call-1",
    content: [{ type: "text", text: "result" }],
  };
  const messages = [
    { role: "user", content: "old" },
    { role: "user", content: "recent" },
    recent,
  ];
  const result = new Assembler(1).assemble({
    messages,
    summaries: new Map([[1, "long summary ".repeat(300)]]),
    tokenBudget: 100,
    freshTailTurns: 1,
  });
  expect(result.estimatedTokens).toBeLessThanOrEqual(100);
  expect(result.estimatedTokens).toBe(
    result.messages.reduce(
      (sum, message) => sum + estimateTokens(JSON.stringify(message)),
      0,
    ),
  );
  expect(result.messages.at(-1)).toBe(recent);
  expect(result.messages).toContain(messages[1]);
});

it("retains the most recent old turns before spending budget on older summaries", () => {
  const messages = ["oldest", "older", "latest"].map((content) => ({
    role: "user",
    content,
  }));
  const result = new Assembler(1).assemble({
    messages,
    summaries: new Map([
      [1, "oldest ".repeat(100)],
      [2, "recent decision"],
    ]),
    tokenBudget: 60,
    freshTailTurns: 1,
  });
  expect(result.estimatedTokens).toBeLessThanOrEqual(60);
  expect(result.messages[0].content).toContain("Turn 2: recent decision");
  expect(result.messages[0].content).not.toContain("oldest");
});

it.each([1, 3])(
  "rejects an oversized protected tail with freshTailTurns=%i",
  (freshTailTurns) => {
    const messages = [{ role: "user", content: "large input ".repeat(300) }];
    expect(() =>
      new Assembler(freshTailTurns).assemble({
        messages,
        summaries: new Map(),
        tokenBudget: 20,
        freshTailTurns,
      }),
    ).toThrow(/fresh.tail.*budget/i);
  },
);

it("rejects an oversized protected tail while compressing a longer history", () => {
  const messages = Array.from({ length: 5 }, (_, index) => ({
    role: "user",
    content: index === 4 ? "large input ".repeat(300) : `question ${index + 1}`,
  }));
  expect(() =>
    new Assembler(2).assemble({
      messages,
      summaries: new Map(),
      tokenBudget: 20,
      freshTailTurns: 2,
    }),
  ).toThrow(RangeError);
});

it("admits multiple summaries that exactly fit the final message estimate", () => {
  const messages = Array.from({ length: 6 }, (_, index) => ({
    role: "user",
    content: `question ${index + 1}`,
  }));
  const summaries = new Map([
    [1, "oldest entry ".repeat(30)],
    [2, "middle entry"],
    [3, "newest entry"],
  ]);
  const block = {
    role: "user",
    content:
      "[summary] Earlier conversation context:\nTurn 2: middle entry\nTurn 3: newest entry",
  };
  const tokenBudget =
    messages
      .slice(3)
      .reduce(
        (sum, message) => sum + estimateTokens(JSON.stringify(message)),
        0,
      ) + estimateTokens(JSON.stringify(block));
  const result = new Assembler(3).assemble({
    messages,
    summaries,
    tokenBudget,
    freshTailTurns: 3,
  });
  expect(result.messages[0]).toEqual(block);
  expect(result.estimatedTokens).toBe(tokenBudget);
});

it("does not fill a gap with older summaries after the newest entry is too large", () => {
  const messages = Array.from({ length: 6 }, (_, index) => ({
    role: "user",
    content: `question ${index + 1}`,
  }));
  const summaries = new Map([
    [1, "small oldest"],
    [2, "small middle"],
    [3, "large latest ".repeat(100)],
  ]);
  const result = new Assembler(3).assemble({
    messages,
    summaries,
    tokenBudget: 100,
    freshTailTurns: 3,
  });
  expect(
    result.messages.some(
      (message) =>
        typeof message.content === "string" &&
        /Turn [12]:/.test(message.content),
    ),
  ).toBe(false);
  expect(result.estimatedTokens).toBeLessThanOrEqual(100);
});

it.each([undefined, NaN, Infinity, 0, -1])(
  "rejects invalid direct-assembler budget %s",
  (tokenBudget) => {
    const messages = Array.from({ length: 5 }, (_, index) => ({
      role: "user",
      content: `question ${index + 1}`,
    }));
    expect(() =>
      new Assembler(1).assemble({
        messages,
        summaries: new Map(),
        tokenBudget: tokenBudget as number,
        freshTailTurns: 1,
      }),
    ).toThrow(RangeError);
  },
);

it("exposes structured oversized-tail details to hosts", () => {
  const messages = Array.from({ length: 5 }, (_, index) => ({
    role: "user",
    content: index === 4 ? "large input ".repeat(300) : `question ${index + 1}`,
  }));
  try {
    new Assembler(2).assemble({
      messages,
      summaries: new Map(),
      tokenBudget: 20,
      freshTailTurns: 2,
    });
    expect.unreachable("oversized compressed tail must be rejected");
  } catch (error) {
    expect(error).toBeInstanceOf(TokenBudgetExceededError);
    expect(error).toBeInstanceOf(RangeError);
    expect(error).toMatchObject({
      tokenBudget: 20,
      freshTailTurnCount: 2,
      freshTailMessageCount: 2,
    });
    expect((error as TokenBudgetExceededError).requiredTokens).toBe(
      messages
        .slice(-2)
        .reduce(
          (sum, message) => sum + estimateTokens(JSON.stringify(message)),
          0,
        ),
    );
  }
});

it.each(["中文😀", 'quote" and backslash\\ and newline\n', "isolated \ud800"])(
  "accounts for escaped summary content exactly: %s",
  (text) => {
    const messages = Array.from({ length: 6 }, (_, index) => ({
      role: "user",
      content: `question ${index + 1}`,
    }));
    const summaries = new Map([
      [1, "oldest entry ".repeat(100)],
      [2, text],
      [3, text],
    ]);
    const block = {
      role: "user",
      content: `[summary] Earlier conversation context:\nTurn 2: ${text}\nTurn 3: ${text}`,
    };
    const tokenBudget =
      messages
        .slice(3)
        .reduce(
          (sum, message) => sum + estimateTokens(JSON.stringify(message)),
          0,
        ) + estimateTokens(JSON.stringify(block));
    const result = new Assembler(3).assemble({
      messages,
      summaries,
      tokenBudget,
      freshTailTurns: 3,
    });
    expect(result.messages[0]).toEqual(block);
    expect(result.estimatedTokens).toBe(tokenBudget);
  },
);

it("separates admitted no-summary previews from budget omissions", () => {
  const messages = Array.from({ length: 8 }, (_, index) => ({
    role: "user",
    content: `question ${index + 1}`,
  }));
  const result = new Assembler(1).assemble({
    messages,
    summaries: new Map(),
    tokenBudget: 85,
    freshTailTurns: 1,
  });
  expect(result.stats.summaryCount).toBeGreaterThan(0);
  expect(result.stats.omittedTurns).toBeGreaterThan(0);
  expect(result.stats.droppedTurns).toBe(result.stats.summaryCount);
  expect(result.stats.omittedTurns + result.stats.summaryCount).toBe(7);
  expect(result.estimatedTokens).toBeLessThanOrEqual(85);
});

it("shows recoverable omitted turn numbers when their marker fits", () => {
  const messages = Array.from({ length: 5 }, (_, index) => ({
    role: "user",
    content: `question ${index + 1}`,
  }));
  const summaries = new Map(
    [1, 2, 3, 4].map((id) => [id, "large summary ".repeat(100)]),
  );
  const result = new Assembler(1).assemble({
    messages,
    summaries,
    tokenBudget: 100,
    freshTailTurns: 1,
  });
  expect(result.stats).toMatchObject({
    summaryCount: 0,
    droppedTurns: 0,
    omittedTurns: 4,
  });
  expect(result.messages[0].content).toContain("Turns 1-4:");
  expect(result.messages[0].content).toContain("expand_turn");
  expect(result.messages.at(-1)).toBe(messages.at(-1));
  expect(result.estimatedTokens).toBeLessThanOrEqual(100);
});

it("does not exceed an exactly full tail budget for an omission marker", () => {
  const messages = Array.from({ length: 5 }, (_, index) => ({
    role: "user",
    content: `question ${index + 1}`,
  }));
  const tokenBudget = estimateTokens(JSON.stringify(messages.at(-1)));
  const result = new Assembler(1).assemble({
    messages,
    summaries: new Map(),
    tokenBudget,
    freshTailTurns: 1,
  });
  expect(result.messages).toEqual(messages.slice(-1));
  expect(result.stats.omittedTurns).toBe(4);
  expect(result.estimatedTokens).toBe(tokenBudget);
});

it.each([27, 28])(
  "rounds fractional summary weight before admission at budget %i",
  (tokenBudget) => {
    const result = new Assembler(1).assemble({
      messages: [
        { role: "user", content: "old" },
        { role: "user", content: "tail" },
      ],
      summaries: new Map([[1, "x"]]),
      freshTailTurns: 1,
      tokenBudget,
    });
    expect(result.stats.summaryCount).toBe(tokenBudget === 28 ? 1 : 0);
    expect(result.estimatedTokens).toBe(tokenBudget === 28 ? 28 : 8);
    expect(result.estimatedTokens).toBeLessThanOrEqual(tokenBudget);
  },
);
