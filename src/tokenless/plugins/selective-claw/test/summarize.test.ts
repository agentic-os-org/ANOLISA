import { describe, it, expect } from "vitest";
import { createSummarizer, fallbackSummary } from "../src/summarize.js";
import type { RuntimeLlmCompleteFn } from "../src/summarize.js";

describe("fallbackSummary", () => {
  it("returns short text as-is", () => {
    expect(fallbackSummary("hello world")).toBe("hello world");
  });

  it("truncates long text to 80 chars", () => {
    const long = "a".repeat(200);
    const result = fallbackSummary(long);
    expect(result).toBe("a".repeat(80) + "...");
  });

  it("strips role prefix", () => {
    expect(fallbackSummary("user: what is the plan?")).toBe("what is the plan?");
  });

  it("uses first non-empty line", () => {
    expect(fallbackSummary("\n\nuser: hello\nassistant: hi")).toBe("hello");
  });
});

describe("createSummarizer", () => {
  it("returns LLM result when successful", async () => {
    const mockLlm: RuntimeLlmCompleteFn = async () => ({
      text: "Discussed database selection and chose PostgreSQL.",
    });
    const summarize = createSummarizer(mockLlm);
    const result = await summarize("user: what database?\nassistant: PostgreSQL");
    expect(result).toBe("Discussed database selection and chose PostgreSQL.");
  });

  it("falls back when LLM returns empty", async () => {
    const mockLlm: RuntimeLlmCompleteFn = async () => ({ text: "" });
    const summarize = createSummarizer(mockLlm);
    const result = await summarize("user: hello world");
    expect(result).toBe("hello world");
  });

  it("falls back when LLM throws", async () => {
    const mockLlm: RuntimeLlmCompleteFn = async () => {
      throw new Error("LLM unavailable");
    };
    const summarize = createSummarizer(mockLlm);
    const result = await summarize("user: hello world");
    expect(result).toBe("hello world");
  });
});
