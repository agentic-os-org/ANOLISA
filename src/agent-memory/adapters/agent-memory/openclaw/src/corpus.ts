/**
 * Corpus identity, read handles and line-window slicing for the memory corpus
 * supplement.
 *
 * OpenClaw's `MemoryCorpusGetResult` (plugin-sdk `plugins/memory-state`)
 * requires a supplement to report the window it actually returned: `fromLine`
 * is the 1-based first line and `lineCount` the number of lines in `content`.
 * The host spreads that result straight into the `memory_read` response, so
 * these two fields are what the model sees — and the host's builtin reader
 * (`buildMemoryReadResult`) clamps the same two inputs, which means a
 * supplement that echoes the *requested* window instead of the *returned* one
 * makes the two corpora disagree about what "from line 0, 10 lines" produced.
 *
 * The path a search hit advertises is the other half of the same contract: the
 * model echoes it back to `memory_get`, and whether that read ever reaches this
 * supplement is a property of the path's *shape*, not of its corpus — see
 * `toCorpusReadHandle`.
 *
 * Pure and standalone on purpose: the supplement itself needs a live plugin
 * API and MCP client, this does not, so the window arithmetic and the path
 * mapping are testable.
 */

export type CorpusWindow = {
  /** The selected lines, joined with "\n". */
  content: string;
  /** 1-based first line of `content`; always >= 1. */
  fromLine: number;
  /** Number of lines in `content`; 0 when the window starts past the end. */
  lineCount: number;
};

/** 1-based start line, clamped exactly like the host's builtin reader.
 *  Without the clamp a 0 or negative request reaches `Array.slice` as a
 *  negative index and silently returns the document's *tail*, and a non-finite
 *  value propagates NaN into the reported window. */
function normalizeFromLine(fromLine?: number): number {
  if (fromLine === undefined || !Number.isFinite(fromLine)) return 1;
  return Math.max(1, Math.trunc(fromLine));
}

/** Requested line count, or undefined for "to the end of the document".
 *  Clamped to >= 1 for the same reason: to the host's builtin reader a
 *  non-positive count means one line, not the whole file — and this plugin's
 *  entire job is keeping payloads out of the context window. */
function normalizeLineCount(lineCount?: number): number | undefined {
  if (lineCount === undefined || !Number.isFinite(lineCount)) return undefined;
  return Math.max(1, Math.trunc(lineCount));
}

/**
 * Select a line window from a document body.
 *
 * `text` is split on "\n" and the selected lines are re-joined, so a document
 * ending in a newline carries one trailing empty line — the same accounting the
 * host's builtin reader uses, which keeps `fromLine + lineCount` pagination
 * consistent across corpora.
 */
export function sliceCorpusWindow(
  text: string,
  fromLine?: number,
  lineCount?: number,
): CorpusWindow {
  const lines = text.split("\n");
  const start = normalizeFromLine(fromLine);
  const requested = normalizeLineCount(lineCount);
  const end = requested === undefined ? lines.length : start - 1 + requested;
  const selected = lines.slice(start - 1, end);
  return {
    content: selected.join("\n"),
    fromLine: start,
    lineCount: selected.length,
  };
}

/**
 * Corpus id this plugin's supplement serves.
 *
 * The host stamps its own builtin hits with `corpus: "memory"` (memory-core
 * `createMemorySearchTool`: `surfacedMemoryResults = memoryResults.map((r) =>
 * ({ ...r, corpus: "memory" }))`) and its compiled-wiki supplement labels its
 * own hits `corpus: "wiki"`, so the field is how the model tells the corpora in
 * one merged `memory_search` response apart — and how it chooses the `corpus`
 * argument to echo back to `memory_get`.
 *
 * Claiming `"memory"` did two kinds of damage at once: agent-memory hits were
 * indistinguishable from the host's `MEMORY.md` hits, and a model echoing the
 * label back was routed to the builtin reader (`readAgentMemoryFile`, over the
 * workspace `MEMORY.md` + `memory/*.md`), which cannot see this store.
 * `memory_get` consults supplements only for `corpus=wiki` and, after a failed
 * builtin read, for `corpus=all` — so every path this supplement surfaced was
 * unreadable through the shared tool flow.
 *
 * The id is ours rather than the wiki's: `memory_get` walks the registered
 * supplements and returns the first non-null answer, so borrowing `corpus:
 * "wiki"` would make the two corpora interchangeable to the model and let
 * whichever registered first shadow the other.
 */
export const AGENT_MEMORY_CORPUS = "agent-memory";

/**
 * Read route carried on search hits.
 *
 * `memory_get`'s `corpus` enum is `memory | wiki | all`, so the model cannot
 * echo `agent-memory` back; with no route it retries the default builtin read
 * and lands in the dead end described above. `corpus=all` is the value that
 * reaches a supplement once the builtin read has failed. The hint costs a few
 * tokens per hit and replaces a failed read plus its retry — a net saving for a
 * component whose job is token economy. Only search hits carry it: by the time
 * `get()` answers, the route is already chosen.
 *
 * The promise holds only while the builtin read genuinely fails, which is a
 * property of the path being read — so the paths advertised alongside this
 * label go through `toCorpusReadHandle`. Without that, every hit whose store
 * path landed in the host's own memory namespace advertised a route that did
 * not exist: the builtin read answered (with the workspace file, or with an
 * empty text when there was none) and this supplement was never called.
 */
export const AGENT_MEMORY_CORPUS_READ_HINT =
  "agent-memory (read via memory_get corpus=all)";

/**
 * Whether the host's builtin reader answers this path itself, so that no
 * supplement is ever consulted for it.
 *
 * `memory_get` reaches the registered supplements only *after* the builtin read
 * has thrown: memory-core's `executeMemoryReadResult` returns the builtin
 * result when `read()` resolves, and only its catch block calls
 * `resolveMemoryReadFailureResult`, which walks the supplements — and only for
 * `corpus=all`. The builtin reader (memory-host-sdk `readMemoryFile`) resolves
 * the requested path against the agent workspace and resolves successfully for
 * exactly the shapes its `isMemoryPath` admits, returning `{text: "", path}`
 * when the workspace has no such file and the workspace file's own content when
 * it does. Neither answer throws, so neither ever reaches a supplement. Every
 * other shape it rejects with "path required", and that throw is this plugin's
 * only door in.
 *
 * Mirrored rather than imported because no plugin-sdk entry point exports
 * `isMemoryPath`; `tests/unit/corpus-routing-test.ts` pins the mirror against
 * the host's real `readMemoryFile`. `memory_get` picks its reader by backend —
 * the builtin one, a manager that delegates to it, or qmd's, which rejects a
 * foreign path the same way — so the shape test is the same one whichever
 * serves the read.
 *
 * The mirror is deliberately over-eager, because that is the cheap direction:
 * namespacing a path the host would have rejected anyway costs one prefix and
 * still routes here, while missing one silently swallows the read. It cannot be
 * exact in any case — the host also answers paths an operator lists under
 * `extraPaths` or inside a qmd collection root, neither of which a plugin can
 * see.
 */
export function isHostBuiltinMemoryPath(storePath: string): boolean {
  // The host normalises before testing: trim, drop leading "." and "/"
  // characters, and accept "\" as a separator.
  const normalized = storePath.trim().replace(/^[./]+/, "").replace(/\\/g, "/");
  if (!normalized) return false;
  if (normalized === "MEMORY.md") return true;
  if (normalized.toLowerCase() === "dreams.md") return true;
  return normalized.startsWith("memory/");
}

/**
 * Namespace put in front of a store path the host would otherwise answer
 * itself.
 *
 * Any prefix outside the host's namespace works — `isMemoryPath` tests the
 * whole relative path, so `agent-memory/MEMORY.md` is not a memory path and the
 * builtin read throws, which is what routes `corpus=all` to a supplement. This
 * plugin's own name keeps the handle self-describing in a merged search
 * response, where a store path can sit next to the workspace file it shadows.
 */
export const AGENT_MEMORY_READ_NAMESPACE = "agent-memory/";

/**
 * The path a search hit advertises — the one the model echoes back to
 * `memory_get`.
 *
 * Store paths the host answers itself are namespaced so that the builtin read
 * fails and `corpus=all` falls through to this supplement. Every other path is
 * advertised unchanged: it already reaches the supplement, and re-labelling it
 * would spend tokens on a route that works.
 *
 * A store path that already begins with the namespace is namespaced as well.
 * Without that, the store's own `agent-memory/notes.md` and a namespaced
 * `notes.md` would advertise the same handle, and `fromCorpusReadHandle` could
 * not tell them apart — one would read the other's file.
 */
export function toCorpusReadHandle(storePath: string): string {
  if (isHostBuiltinMemoryPath(storePath)) {
    return AGENT_MEMORY_READ_NAMESPACE + storePath;
  }
  if (storePath.startsWith(AGENT_MEMORY_READ_NAMESPACE)) {
    return AGENT_MEMORY_READ_NAMESPACE + storePath;
  }
  return storePath;
}

/**
 * Inverse of `toCorpusReadHandle`: the store path a `memory_get` lookup names.
 *
 * Exact for every handle this plugin advertises and the identity for anything
 * else, so a lookup that arrives un-namespaced still resolves — a model quoting
 * a path it saw elsewhere, or a `corpus=wiki` read, which skips the builtin
 * reader entirely and reaches the supplements for any path shape.
 */
export function fromCorpusReadHandle(lookup: string): string {
  return lookup.startsWith(AGENT_MEMORY_READ_NAMESPACE)
    ? lookup.slice(AGENT_MEMORY_READ_NAMESPACE.length)
    : lookup;
}

/** A hit exactly as this plugin's MCP `memory_search` reports it. */
export type CorpusSearchHit = {
  path: string;
  snippet: string;
  score: number;
};

/**
 * Label one MCP search hit as belonging to this plugin's corpus, advertising
 * the path that actually routes back to it.
 */
export function toCorpusSearchResult(
  hit: CorpusSearchHit,
): {
  corpus: string;
  path: string;
  snippet: string;
  score: number;
  provenanceLabel: string;
} {
  return {
    corpus: AGENT_MEMORY_CORPUS,
    path: toCorpusReadHandle(hit.path),
    snippet: hit.snippet,
    score: hit.score,
    provenanceLabel: AGENT_MEMORY_CORPUS_READ_HINT,
  };
}
