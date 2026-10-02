/**
 * Unit tests for the corpus supplement contract helpers (src/corpus.ts):
 * the line window a `memory_read` reports, the corpus identity the
 * supplement's results carry, and the read handle each hit advertises.
 *
 * The window the memory corpus supplement reports back to OpenClaw has to
 * match the host's own builtin reader: `MemoryCorpusGetResult` requires
 * `fromLine` / `lineCount`, and the host clamps both inputs, so an unclamped
 * supplement would answer the same `memory_read` differently depending on
 * which corpus served it.
 *
 * The handle is checked here as a pure mapping; whether it actually routes a
 * read back to the supplement is `corpus-routing-test.ts`, which drives the
 * host's own reader.
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  AGENT_MEMORY_CORPUS,
  AGENT_MEMORY_CORPUS_READ_HINT,
  AGENT_MEMORY_READ_NAMESPACE,
  fromCorpusReadHandle,
  isHostBuiltinMemoryPath,
  sliceCorpusWindow,
  toCorpusReadHandle,
  toCorpusSearchResult,
} from "../../src/corpus.js";

type CorpusHit = Parameters<typeof toCorpusSearchResult>[0];

// Five lines, no trailing newline.
const DOC = "one\ntwo\nthree\nfour\nfive";

describe("sliceCorpusWindow", () => {
  it("returns the whole document when no window is requested", () => {
    assert.deepEqual(sliceCorpusWindow(DOC), {
      content: DOC,
      fromLine: 1,
      lineCount: 5,
    });
  });

  it("returns the requested window and reports it", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 2, 2), {
      content: "two\nthree",
      fromLine: 2,
      lineCount: 2,
    });
  });

  it("honours an open-ended window from a given line", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 4), {
      content: "four\nfive",
      fromLine: 4,
      lineCount: 2,
    });
  });

  it("clamps a window that runs past the end of the document", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 4, 10), {
      content: "four\nfive",
      fromLine: 4,
      lineCount: 2,
    });
  });

  it("reports an empty window when fromLine is past the end", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 6, 2), {
      content: "",
      fromLine: 6,
      lineCount: 0,
    });
  });

  it("clamps fromLine 0 to the first line instead of slicing from the end", () => {
    // Regression: (0 - 1) reached Array.slice as -1 and returned the tail.
    assert.deepEqual(sliceCorpusWindow(DOC, 0, 2), {
      content: "one\ntwo",
      fromLine: 1,
      lineCount: 2,
    });
  });

  it("clamps a negative fromLine the same way", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, -3, 1), {
      content: "one",
      fromLine: 1,
      lineCount: 1,
    });
  });

  it("clamps lineCount 0 to one line, matching the builtin reader", () => {
    // Regression: 0 is falsy, so it used to select the rest of the document.
    assert.deepEqual(sliceCorpusWindow(DOC, 2, 0), {
      content: "two",
      fromLine: 2,
      lineCount: 1,
    });
  });

  it("clamps a negative lineCount the same way", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 3, -5), {
      content: "three",
      fromLine: 3,
      lineCount: 1,
    });
  });

  it("truncates fractional bounds instead of reporting them", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, 2.7, 2.9), {
      content: "two\nthree",
      fromLine: 2,
      lineCount: 2,
    });
  });

  it("treats non-finite bounds as no window", () => {
    assert.deepEqual(sliceCorpusWindow(DOC, Number.NaN, Number.POSITIVE_INFINITY), {
      content: DOC,
      fromLine: 1,
      lineCount: 5,
    });
  });

  it("counts a trailing newline as one empty line", () => {
    assert.deepEqual(sliceCorpusWindow("a\nb\n"), {
      content: "a\nb\n",
      fromLine: 1,
      lineCount: 3,
    });
  });

  it("handles an empty document", () => {
    assert.deepEqual(sliceCorpusWindow(""), {
      content: "",
      fromLine: 1,
      lineCount: 1,
    });
  });
});

/** Store paths this plugin's MCP server can legitimately report: the ones the
 *  host's builtin namespace shadows, the ones it does not, and the one that
 *  collides with the namespace this plugin prefixes with. */
const STORE_PATHS = [
  "MEMORY.md",
  "dreams.md",
  "memory/sample.md",
  "memory/nested/deep.md",
  "notes/observed/01J.md",
  "observations.md",
  "strategies/rebase.md",
  "agent-memory/notes.md",
];

describe("host builtin namespace", () => {
  it("names the shapes the host's builtin reader answers itself", () => {
    // memory-core's memory_get only consults supplements after the builtin
    // read throws, and the host's isMemoryPath admits exactly these.
    for (const p of [
      "MEMORY.md",
      "dreams.md",
      "memory/sample.md",
      "memory/nested/deep.md",
    ]) {
      assert.equal(isHostBuiltinMemoryPath(p), true, p);
    }
  });

  it("normalises before testing, the way the host normalises", () => {
    for (const p of [
      "./MEMORY.md",
      "  MEMORY.md  ",
      "memory\\sample.md",
      ".//memory/sample.md",
    ]) {
      assert.equal(isHostBuiltinMemoryPath(p), true, p);
    }
  });

  it("matches dreams.md case-insensitively, like the host", () => {
    assert.equal(isHostBuiltinMemoryPath("Dreams.md"), true);
    assert.equal(isHostBuiltinMemoryPath("DREAMS.MD"), true);
    assert.equal(isHostBuiltinMemoryPath("MEMORY.MD"), false);
  });

  it("leaves every other store path alone", () => {
    // Near-misses matter as much as the hits: over-claiming here namespaces
    // paths that already reach the supplement and costs tokens for nothing.
    for (const p of [
      "notes/observed/01J.md",
      "observations.md",
      "memory",
      "memoryx/foo.md",
      "MEMORY.md.bak",
      "dreams.md.bak",
      "notes/memory/sample.md",
      "",
    ]) {
      assert.equal(isHostBuiltinMemoryPath(p), false, p);
    }
  });
});

describe("corpus read handle", () => {
  it("namespaces a store path the host would answer itself", () => {
    assert.equal(toCorpusReadHandle("MEMORY.md"), "agent-memory/MEMORY.md");
    assert.equal(
      toCorpusReadHandle("memory/sample.md"),
      "agent-memory/memory/sample.md",
    );
    assert.equal(toCorpusReadHandle("dreams.md"), "agent-memory/dreams.md");
  });

  it("leaves a path that already reaches the supplement unchanged", () => {
    assert.equal(toCorpusReadHandle("notes/observed/01J.md"), "notes/observed/01J.md");
    assert.equal(toCorpusReadHandle("observations.md"), "observations.md");
  });

  it("namespaces a store path that already begins with the namespace", () => {
    // Otherwise the store's own agent-memory/notes.md and a namespaced
    // notes.md would advertise the same handle, and one would read the
    // other's file.
    assert.equal(
      toCorpusReadHandle("agent-memory/notes.md"),
      "agent-memory/agent-memory/notes.md",
    );
  });

  it("never advertises a handle inside the host's builtin namespace", () => {
    for (const p of STORE_PATHS) {
      assert.equal(isHostBuiltinMemoryPath(toCorpusReadHandle(p)), false, p);
    }
  });

  it("round-trips every store path", () => {
    for (const p of STORE_PATHS) {
      assert.equal(fromCorpusReadHandle(toCorpusReadHandle(p)), p, p);
    }
  });

  it("resolves an un-namespaced lookup to itself", () => {
    // A model quoting a path it saw elsewhere, or a corpus=wiki read, which
    // skips the builtin reader and reaches supplements for any path shape.
    assert.equal(fromCorpusReadHandle("MEMORY.md"), "MEMORY.md");
    assert.equal(
      fromCorpusReadHandle("notes/observed/01J.md"),
      "notes/observed/01J.md",
    );
  });

  it("advertises the handle, not the store path, on a search hit", () => {
    const shadowed: CorpusHit = { path: "MEMORY.md", snippet: "…decided…", score: -1 };
    assert.equal(toCorpusSearchResult(shadowed).path, "agent-memory/MEMORY.md");
    const reachable: CorpusHit = {
      path: "notes/observed/01J.md",
      snippet: "…",
      score: -1,
    };
    assert.equal(toCorpusSearchResult(reachable).path, "notes/observed/01J.md");
  });

  it("namespaces with this plugin's own corpus id", () => {
    // The handle has to stay self-describing in a merged search response,
    // where a store path can sit next to the workspace file it shadows.
    assert.equal(AGENT_MEMORY_READ_NAMESPACE, `${AGENT_MEMORY_CORPUS}/`);
  });
});

describe("corpus identity", () => {
  const HIT: CorpusHit = {
    path: "notes/observed/01J.md",
    snippet: "…decided…",
    score: -3.25,
  };

  it("labels a search hit with this plugin's own corpus id", () => {
    assert.deepEqual(toCorpusSearchResult(HIT), {
      corpus: "agent-memory",
      path: HIT.path,
      snippet: HIT.snippet,
      score: HIT.score,
      provenanceLabel: "agent-memory (read via memory_get corpus=all)",
    });
  });

  it("never claims a corpus id the host resolves without a supplement", () => {
    // The host stamps its own builtin hits "memory" (and filters "sessions"
    // from the same index); memory_get's corpus enum is memory | wiki | all,
    // and only "wiki" and "all" ever reach a supplement. Claiming one of those
    // ids sends the model's read to a reader that cannot see this store.
    const hostResolved = ["memory", "sessions", "wiki", "all"];
    assert.equal(hostResolved.includes(AGENT_MEMORY_CORPUS), false);
  });

  it("carries the read route that does reach a supplement", () => {
    // memory_get falls back to the registered supplements only when the builtin
    // read failed *and* corpus=all was requested, so that is the route to name.
    assert.match(AGENT_MEMORY_CORPUS_READ_HINT, /corpus=all/);
    assert.equal(
      toCorpusSearchResult(HIT).provenanceLabel,
      AGENT_MEMORY_CORPUS_READ_HINT,
    );
  });

  it("passes the MCP score through unrescaled", () => {
    // Ranking across corpora is the host's merge step; this mapper only labels.
    const hits = [
      { path: "a.md", snippet: "a", score: -0.5 },
      { path: "b.md", snippet: "b", score: -4 },
    ];
    assert.deepEqual(
      hits.map(toCorpusSearchResult).map((r) => r.score),
      [-0.5, -4],
    );
  });
});
