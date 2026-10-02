/**
 * Routing tests for the corpus read handle: does the path a search hit
 * advertises actually reach this plugin's supplement through the host's
 * `memory_get`?
 *
 * `corpus-test.ts` checks the handle as a pure mapping. This file checks the
 * claim the handle exists to make, against the pinned host's own reader — a
 * hit is only readable if the model can follow `provenanceLabel` and land on
 * `get()`, and for a store path inside the host's builtin memory namespace
 * (`MEMORY.md`, `dreams.md`, `memory/**`) it could not: the builtin read
 * *succeeds* there, with the workspace file's content or with an empty text
 * when the workspace has no such file, and `memory_get` consults supplements
 * only after that read has thrown.
 */

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { readMemoryFile } from "openclaw/plugin-sdk/memory-core-host-engine-storage";
import {
  AGENT_MEMORY_CORPUS,
  fromCorpusReadHandle,
  sliceCorpusWindow,
  toCorpusReadHandle,
  toCorpusSearchResult,
} from "../../src/corpus.js";

/** What this plugin's MCP store holds, keyed by store-relative path. */
const STORE = new Map<string, string>([
  ["MEMORY.md", "store one\nstore two"],
  ["dreams.md", "store dream one\nstore dream two"],
  ["memory/sample.md", "store mem one\nstore mem two"],
  ["notes/observed/sample.md", "note one\nnote two"],
  ["agent-memory/notes.md", "namespaced one\nnamespaced two"],
]);

/** Paths the workspace's own builtin memory holds — same shapes, different
 *  content, so a read served by the wrong store is unmistakable. */
const WORKSPACE_FILES = new Map<string, string>([
  ["MEMORY.md", "builtin one\nbuiltin two"],
  ["memory/sample.md", "builtin mem one\nbuiltin mem two"],
]);

type ReadRoute =
  | { via: "builtin"; text: string }
  | { via: "supplement"; text: string }
  | { via: "none" };

/**
 * The host's `memory_get` read path (memory-core `tools.ts`, OpenClaw
 * 2026.5.7), reduced to the part that decides whether a supplement is
 * consulted:
 *
 *   executeMemoryReadResult        builtin read resolves → return it;
 *                                  builtin read throws → resolveMemoryReadFailureResult,
 *                                  which for `corpus=all` walks the registered
 *                                  supplements and returns the first non-null
 *                                  answer.
 *
 * The builtin read is the host's own exported `readMemoryFile` over a real
 * workspace directory, so what decides the route here is the host's real
 * behaviour rather than a restatement of it.
 */
async function hostMemoryGet(params: {
  workspaceDir: string;
  relPath: string;
  store: Map<string, string>;
}): Promise<ReadRoute> {
  // This plugin's supplement, as src/index.ts registers it.
  const supplementGet = async (lookup: string) => {
    const text = params.store.get(fromCorpusReadHandle(lookup));
    if (text === undefined) return null; // index.ts: MCP error → null
    const slice = sliceCorpusWindow(text);
    return { corpus: AGENT_MEMORY_CORPUS, path: lookup, content: slice.content };
  };

  try {
    const builtin = await readMemoryFile({
      workspaceDir: params.workspaceDir,
      relPath: params.relPath,
    });
    return { via: "builtin", text: builtin.text };
  } catch {
    const result = await supplementGet(params.relPath);
    return result ? { via: "supplement", text: result.content } : { via: "none" };
  }
}

/** Read the path a search hit advertises, the way a model following
 *  `provenanceLabel` would. */
async function readAdvertisedHit(params: {
  workspaceDir: string;
  storePath: string;
  store: Map<string, string>;
}): Promise<ReadRoute> {
  const hit = toCorpusSearchResult({
    path: params.storePath,
    snippet: "…",
    score: -1,
  });
  return await hostMemoryGet({
    workspaceDir: params.workspaceDir,
    relPath: hit.path,
    store: params.store,
  });
}

describe("memory_get routing through the pinned host", () => {
  let bareWorkspace = ""; // no builtin memory files at all
  let shadowingWorkspace = ""; // builtin MEMORY.md + memory/sample.md present

  before(async () => {
    bareWorkspace = await fs.mkdtemp(path.join(os.tmpdir(), "am-corpus-bare-"));
    shadowingWorkspace = await fs.mkdtemp(
      path.join(os.tmpdir(), "am-corpus-shadow-"),
    );
    for (const [relPath, content] of WORKSPACE_FILES) {
      const abs = path.join(shadowingWorkspace, relPath);
      await fs.mkdir(path.dirname(abs), { recursive: true });
      await fs.writeFile(abs, `${content}\n`, "utf-8");
    }
  });

  after(async () => {
    for (const dir of [bareWorkspace, shadowingWorkspace]) {
      await fs.rm(dir, { recursive: true, force: true });
    }
  });

  for (const workspace of ["bare", "shadowing"] as const) {
    const dir = () => (workspace === "bare" ? bareWorkspace : shadowingWorkspace);

    describe(`workspace ${workspace}`, () => {
      it("routes every advertised hit to this supplement", async () => {
        for (const storePath of STORE.keys()) {
          const route = await readAdvertisedHit({
            workspaceDir: dir(),
            storePath,
            store: STORE,
          });
          assert.equal(route.via, "supplement", storePath);
          assert.equal(route.text, STORE.get(storePath), storePath);
        }
      });

      it("reads the store, not the workspace file it shadows", async () => {
        // The reviewer's repro: with a workspace MEMORY.md present, a bare
        // read of the same shape returned "builtin two" and never called the
        // supplement.
        for (const storePath of ["MEMORY.md", "memory/sample.md", "dreams.md"]) {
          const route = await readAdvertisedHit({
            workspaceDir: dir(),
            storePath,
            store: STORE,
          });
          assert.equal(route.via, "supplement", storePath);
          assert.match(route.text, /^store /, storePath);
        }
      });

      it("advertises only handles the host's builtin reader rejects", async () => {
        // A handle the builtin reader answers is a handle that never reaches
        // get(), whatever the workspace holds.
        for (const storePath of STORE.keys()) {
          await assert.rejects(
            readMemoryFile({
              workspaceDir: dir(),
              relPath: toCorpusReadHandle(storePath),
            }),
            /path required/,
            storePath,
          );
        }
      });
    });
  }

  describe("the bare store path it replaces", () => {
    it("is answered by the workspace file when one exists", async () => {
      const route = await hostMemoryGet({
        workspaceDir: shadowingWorkspace,
        relPath: "MEMORY.md",
        store: STORE,
      });
      assert.deepEqual(route, {
        via: "builtin",
        text: "builtin one\nbuiltin two\n",
      });
    });

    it("is answered with empty text when no workspace file exists", async () => {
      // Neither answer throws, so neither ever reaches a supplement: this is
      // the dead end the handle exists to avoid.
      for (const relPath of ["MEMORY.md", "memory/sample.md", "dreams.md"]) {
        const route = await hostMemoryGet({
          workspaceDir: bareWorkspace,
          relPath,
          store: STORE,
        });
        assert.deepEqual(route, { via: "builtin", text: "" }, relPath);
      }
    });

    it("still reaches the supplement for a path outside the namespace", async () => {
      // Why only shadowed paths are namespaced: this one already worked, and
      // re-labelling it would spend tokens on a route that needs no repair.
      const route = await hostMemoryGet({
        workspaceDir: shadowingWorkspace,
        relPath: "notes/observed/sample.md",
        store: STORE,
      });
      assert.equal(route.via, "supplement");
      assert.equal(route.text, "note one\nnote two");
    });
  });
});
