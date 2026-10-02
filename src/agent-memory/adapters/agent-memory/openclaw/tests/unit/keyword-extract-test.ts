/**
 * Unit tests for BM25 auto-recall keyword extraction.
 *
 * Covers the review findings from PR #1574: keyword extraction with
 * non-ASCII stripping, CJK trigram extraction (>= 3 chars for BM25
 * trigram tokenizer compatibility), 2-char keyword allowance,
 * candidate deduplication, stride sampling, byte-length safety.
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";

const {
  extractKeywords,
  extractCjkTrigrams,
  buildRecallQueries,
  MAX_QUERY_BYTES,
  MAX_RESULTS,
  RRF_K,
} = await import("../../src/keyword-extract.js");

describe("extractKeywords", () => {
  it("extracts salient words from English prompts", () => {
    const kw = extractKeywords(
      "What is the secret codeword for this session?",
    );
    assert.ok(kw.includes("secret"));
    assert.ok(kw.includes("codeword"));
    assert.ok(kw.includes("session"));
  });

  it("filters stopwords", () => {
    const kw = extractKeywords("the cat is on the mat");
    assert.ok(!kw.includes("the"));
    assert.ok(!kw.includes("is"));
    assert.ok(!kw.includes("on"));
    assert.ok(kw.includes("cat"));
    assert.ok(kw.includes("mat"));
  });

  it("allows 2-char ASCII keywords (AI, ID, UK)", () => {
    const kw = extractKeywords(
      "What is the secret codeword for our AI session in the UK?",
    );
    assert.ok(kw.split(" ").includes("ai"), "2-char 'ai' should be kept");
    assert.ok(kw.split(" ").includes("uk"), "2-char 'uk' should be kept");
    assert.ok(kw.includes("secret"));
    assert.ok(kw.includes("codeword"));
    assert.ok(kw.includes("session"));
  });

  it("extracts keywords from English prompts containing emoji", () => {
    const kw = extractKeywords(
      "What is the secret codeword for this session? \ud83d\udd10",
    );
    assert.ok(kw.includes("secret"), "should extract 'secret' despite emoji");
    assert.ok(kw.includes("codeword"), "should extract 'codeword' despite emoji");
    assert.ok(kw.includes("session"), "should extract 'session' despite emoji");
  });

  it("extracts keywords from prompts with smart quotes and accented letters", () => {
    const kw = extractKeywords(
      "What is the \u201csecret\u201d codeword for caf\u00e9 session?",
    );
    assert.ok(kw.includes("secret"), "should extract 'secret' despite smart quotes");
    assert.ok(kw.includes("codeword"), "should extract 'codeword'");
    assert.ok(kw.includes("session"), "should extract 'session'");
  });

  it("returns empty for pure CJK text (no ASCII keywords available)", () => {
    assert.equal(extractKeywords("\u8bf7\u95ee\u6211\u4eec\u4e4b\u524d\u7ea6\u5b9a\u7684\u79d8\u5bc6\u4ee3\u53f7\u662f\u4ec0\u4e48"), "");
    assert.equal(extractKeywords("\u82b1\u540d\u306f\u5c0f\u4e91\u3067\u3059"), "");
    assert.equal(extractKeywords("\u041a\u0430\u043a \u0434\u0435\u043b\u0430?"), "");
  });

  it("extracts ASCII keywords from mixed CJK + ASCII text", () => {
    const kw = extractKeywords("hello\u4e16\u754c");
    assert.ok(kw.includes("hello"), "should extract 'hello' from mixed text");
    const kw2 = extractKeywords("test\u30c6\u30b9\u30c8");
    assert.ok(kw2.includes("test"), "should extract 'test' from mixed text");
  });

  it("deduplicates repeated words", () => {
    const kw = extractKeywords("test test test hello hello world");
    const words = kw.split(" ").filter(Boolean);
    assert.equal(new Set(words).size, words.length);
  });

  it("stride-samples endpoints-inclusively so the last keyword is always included", () => {
    const prompt =
      "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima codeword";
    const kw = extractKeywords(prompt);
    const words = kw.split(" ");
    assert.ok(words.length <= 8, `expected \u22648 keywords, got ${words.length}`);
    assert.ok(
      words.includes("alpha"),
      "stride sampling must include the first keyword",
    );
    assert.ok(
      words.includes("codeword"),
      "stride sampling must include the last keyword (tail topic)",
    );
  });

  it("returns all keywords when under the cap", () => {
    const kw = extractKeywords("rust python typescript");
    assert.equal(kw, "rust python typescript");
  });

  it("handles empty and very short input", () => {
    assert.equal(extractKeywords(""), "");
    assert.equal(extractKeywords("a"), "");
    assert.equal(extractKeywords("   "), "");
  });

  it("keeps single 2-char keyword", () => {
    const kw = extractKeywords("AI");
    assert.equal(kw, "ai");
  });
});

describe("extractCjkTrigrams", () => {
  it("extracts overlapping trigrams from Chinese text", () => {
    // 秘密代号是猎鹰 -> 秘密代, 密代号, 代号是, 号是猎, 是猎鹰
    const trigrams = extractCjkTrigrams("\u79d8\u5bc6\u4ee3\u53f7\u662f\u730e\u9e70");
    assert.ok(trigrams.includes("\u79d8\u5bc6\u4ee3"), "should include 秘密代");
    assert.ok(trigrams.includes("\u5bc6\u4ee3\u53f7"), "should include 密代号");
    assert.ok(trigrams.includes("\u4ee3\u53f7\u662f"), "should include 代号是");
    assert.ok(trigrams.includes("\u53f7\u662f\u730e"), "should include 号是猎");
    assert.ok(trigrams.includes("\u662f\u730e\u9e70"), "should include 是猎鹰");
    assert.equal(trigrams.length, 5);
  });

  it("extracts trigrams from long CJK sentences", () => {
    const text = "\u8bf7\u95ee\u6211\u4eec\u4e4b\u524d\u7ea6\u5b9a\u7684\u79d8\u5bc6\u4ee3\u53f7\u662f\u4ec0\u4e48\u8bf7\u53ea\u56de\u7b54\u4ee3\u53f7";
    const trigrams = extractCjkTrigrams(text);
    assert.ok(
      trigrams.some((t) => t.includes("\u79d8\u5bc6")),
      "should include trigram containing 秘密",
    );
    assert.ok(
      trigrams.some((t) => t.includes("\u4ee3\u53f7")),
      "should include trigram containing 代号",
    );
  });

  it("returns empty for pure ASCII text", () => {
    assert.deepEqual(extractCjkTrigrams("hello world"), []);
  });

  it("returns empty for text with fewer than 3 CJK characters", () => {
    assert.deepEqual(extractCjkTrigrams("\u4f60\u597d"), []);
    assert.deepEqual(extractCjkTrigrams("\u4f60"), []);
  });

  it("deduplicates repeated trigrams", () => {
    // 秘密代秘密代 -> 秘密代, 密代秘, 代秘密, 秘密代 (dup)
    const trigrams = extractCjkTrigrams("\u79d8\u5bc6\u4ee3\u79d8\u5bc6\u4ee3");
    const unique = new Set(trigrams);
    assert.equal(trigrams.length, unique.size, "trigrams should be deduplicated");
  });

  it("all trigrams are at least 3 characters", () => {
    const text = "\u8bf7\u95ee\u6211\u4eec\u4e4b\u524d\u7ea6\u5b9a\u7684\u79d8\u5bc6\u4ee3\u53f7";
    const trigrams = extractCjkTrigrams(text);
    for (const t of trigrams) {
      assert.ok(
        t.length >= 3,
        `trigram "${t}" has ${t.length} chars, must be >= 3 for BM25 trigram tokenizer`,
      );
    }
  });

  it("handles mixed CJK and ASCII text", () => {
    const trigrams = extractCjkTrigrams("hello\u79d8\u5bc6\u4ee3\u53f7test");
    assert.ok(trigrams.length > 0);
    assert.ok(trigrams.includes("\u79d8\u5bc6\u4ee3"));
  });
});

describe("buildRecallQueries", () => {
  it("returns deduplicated candidates", () => {
    const queries = buildRecallQueries("codeword");
    const unique = new Set(queries);
    assert.equal(queries.length, unique.size, "candidates must be deduplicated");
    assert.ok(queries.includes("codeword"));
  });

  it("includes keyword query and top-3 for multi-keyword prompts", () => {
    const queries = buildRecallQueries(
      "What is the secret codeword for our session today?",
    );
    assert.ok(queries[0].includes("secret"));
    assert.ok(queries[0].includes("codeword"));
    const top3 = queries[1].split(" ");
    assert.ok(top3.length <= 3);
  });

  it("includes truncated raw prompt as fallback", () => {
    const longPrompt = "a".repeat(300);
    const queries = buildRecallQueries(longPrompt);
    const hasTruncated = queries.some((q) => q.length === 200);
    assert.ok(hasTruncated, "should include 200-char truncated fallback");
  });

  it("includes full raw prompt for moderately long text", () => {
    const prompt = "word ".repeat(50).trim();
    const queries = buildRecallQueries(prompt);
    assert.ok(
      queries.includes(prompt),
      "full raw prompt should be present as final candidate",
    );
  });

  it("handles CJK text with trigram candidates (>= 3 chars each)", () => {
    const cjkPrompt = "\u8bf7\u95ee\u6211\u4eec\u4e4b\u524d\u7ea6\u5b9a\u7684\u79d8\u5bc6\u4ee3\u53f7\u662f\u4ec0\u4e48";
    const queries = buildRecallQueries(cjkPrompt);
    assert.ok(queries.length >= 2, "should have trigram + raw candidates");
    // All tokens in the trigram candidate should be >= 3 chars
    const trigramCandidate = queries.find((q) =>
      q.split(" ").every((t) => t.length >= 3 && /[\u4e00-\u9fff]/.test(t)),
    );
    assert.ok(trigramCandidate, "should have a CJK trigram candidate");
  });

  it("does not duplicate short CJK prompts", () => {
    const cjkPrompt = "\u79d8\u5bc6\u4ee3\u53f7";
    const queries = buildRecallQueries(cjkPrompt);
    assert.ok(queries.length >= 1);
    // 4 CJK chars -> 2 trigrams: 秘密代, 密代号
    const hasTrigram = queries.some((q) => q.includes("\u79d8\u5bc6\u4ee3"));
    assert.ok(hasTrigram, "should have trigram candidate");
  });

  it("handles mixed CJK and ASCII with keyword extraction and trigrams", () => {
    const queries = buildRecallQueries("hello\u4e16\u754c\u4f60\u597d test");
    assert.ok(queries.some((q) => q.includes("hello")));
  });

  it("does not produce more than 6 candidates", () => {
    const queries = buildRecallQueries(
      "What is the secret codeword for this very long session that we are having today?",
    );
    assert.ok(queries.length <= 6, `expected \u22646 candidates, got ${queries.length}`);
  });

  it("extracts keywords from emoji-containing prompts", () => {
    const queries = buildRecallQueries(
      "What is the secret codeword for this session? \ud83d\udd10",
    );
    assert.ok(queries.length >= 2, "should have keyword + raw candidates");
    assert.ok(
      queries[0].includes("secret"),
      "first candidate should be extracted keywords",
    );
  });

  it("truncates candidates exceeding MAX_QUERY_BYTES", () => {
    const longCjkPrompt = "\u4e00".repeat(350);
    const queries = buildRecallQueries(longCjkPrompt);
    const encoder = new TextEncoder();
    for (const q of queries) {
      const byteLen = encoder.encode(q).byteLength;
      assert.ok(
        byteLen <= MAX_QUERY_BYTES,
        `candidate exceeds byte limit: ${byteLen} > ${MAX_QUERY_BYTES}`,
      );
    }
  });

  it("truncates very long ASCII raw prompt to byte limit", () => {
    const longPrompt = "word ".repeat(400);
    const queries = buildRecallQueries(longPrompt);
    const encoder = new TextEncoder();
    for (const q of queries) {
      assert.ok(
        encoder.encode(q).byteLength <= MAX_QUERY_BYTES,
        "all candidates must be within byte limit",
      );
    }
  });

  it("includes CJK trigram candidates for long pure CJK prompts", () => {
    const cjkLong = "\u8bf7\u95ee\u6211\u4eec\u4e4b\u524d\u7ea6\u5b9a\u7684\u79d8\u5bc6\u4ee3\u53f7\u662f\u4ec0\u4e48\u8bf7\u53ea\u56de\u7b54\u4ee3\u53f7";
    const queries = buildRecallQueries(cjkLong);
    assert.ok(queries.length >= 3, `expected >=3 candidates for CJK, got ${queries.length}`);
    // All tokens in trigram candidates should be >= 3 chars
    const trigramCandidate = queries.find((q) =>
      q.split(" ").every((t) => t.length >= 3),
    );
    assert.ok(trigramCandidate, "should have trigram candidate with all tokens >= 3 chars");
  });

  it("includes 2-char keywords like AI and ID in candidates", () => {
    const queries = buildRecallQueries("What is the AI codeword for our UK session?");
    const keywordCandidate = queries[0];
    assert.ok(
      keywordCandidate.split(" ").includes("ai"),
      "keyword candidate should include 'ai'",
    );
    assert.ok(
      keywordCandidate.split(" ").includes("uk"),
      "keyword candidate should include 'uk'",
    );
  });

  it("exports MAX_RESULTS and RRF_K constants", () => {
    assert.equal(MAX_RESULTS, 5);
    assert.equal(RRF_K, 60);
    assert.equal(MAX_QUERY_BYTES, 1024);
  });
});
