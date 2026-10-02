import { describe, it, expect } from "vitest";
import { estimateTokens, truncateTextToEstimatedTokens } from "../src/estimate-tokens.js";

describe("estimateTokens", () => {
  it("estimates ASCII text at ~0.25 tokens per char", () => {
    const result = estimateTokens("hello world");
    expect(result).toBe(3);
  });

  it("estimates CJK text at ~1.5 tokens per char", () => {
    const result = estimateTokens("你好世界");
    expect(result).toBe(6);
  });

  it("returns 0 for empty string", () => {
    expect(estimateTokens("")).toBe(0);
  });
});

describe("truncateTextToEstimatedTokens", () => {
  it("truncates to fit within token limit", () => {
    const text = "a".repeat(100);
    const truncated = truncateTextToEstimatedTokens(text, 10);
    expect(estimateTokens(truncated)).toBeLessThanOrEqual(10);
    expect(truncated.length).toBe(40);
  });

  it("returns full text when under limit", () => {
    const text = "short";
    expect(truncateTextToEstimatedTokens(text, 100)).toBe(text);
  });
});
