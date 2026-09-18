/**
 * register() lifecycle around a configuration the plugin refuses.
 *
 * A hot-reload can call register() again without the host firing
 * gateway_stop for the previous instance, so the plugin tears the stale
 * client down itself (see the `activeClient` note in src/index.ts). That
 * teardown has to run *before* `resolveConfig`, not after it: a config
 * error — the `profile: "expert"` rejection in particular — aborts
 * register(), the host keeps nothing from the failed registration, and the
 * stale subprocess would outlive the plugin that owned it, holding the
 * sqlite/git locks until the gateway exits. The reload that fixes the
 * config would then start a second child behind those locks.
 *
 * Nothing here spawns a subprocess: `McpStdioClient` starts lazily on the
 * first tool call and `stop()` on an unstarted client returns immediately,
 * so these tests drive the real register() against a mock host API and
 * observe the teardown through a `stop()` spy.
 */

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { McpStdioClient } from "../../src/mcp-client.js";

const plugin = (await import("../../src/index.js")).default;

// resolveConfig requires an existing, executable binaryPath. It is never
// spawned — no test below calls a tool — so a stub file is enough.
const binDir = fs.mkdtempSync(path.join(os.tmpdir(), "agent-memory-register-"));
const binaryPath = path.join(binDir, "agent-memory");
fs.writeFileSync(binaryPath, "#!/bin/sh\nexit 0\n");
fs.chmodSync(binaryPath, 0o755);

/** Valid config; `sessionId` doubles as the identity of the client that
 *  carried it, which is how the assertions below tell the stale client
 *  apart from the one a later register() creates. */
function config(sessionId: string, extra: Record<string, unknown> = {}) {
  return { binaryPath, userId: "1000", sessionId, profile: "advanced", ...extra };
}

type MockHost = ReturnType<typeof mockApi>;

function mockApi(pluginConfig: Record<string, unknown>) {
  const tools: string[] = [];
  const hooks: string[] = [];
  const warnings: string[] = [];
  const api = {
    pluginConfig,
    resolvePath: (p: string) => p,
    logger: {
      info: () => {},
      debug: () => {},
      error: () => {},
      warn: (message: string) => {
        warnings.push(message);
      },
    },
    on: (event: string) => {
      hooks.push(event);
    },
    registerTool: (spec: { name: string }) => {
      tools.push(spec.name);
    },
    registerMemoryCapability: () => {},
    registerMemoryCorpusSupplement: () => {},
  };
  return { api: api as never, tools, hooks, warnings };
}

function register(host: MockHost) {
  plugin.register(host.api);
  return host;
}

/** The sessionId of the config a stopped client was built from. */
function sessionIdOf(client: McpStdioClient): string | undefined {
  return (client as unknown as { config?: { sessionId?: string } }).config?.sessionId;
}

// `activeClient` is module-scoped, so the registrations below are one
// continuous sequence: each test asserts on the stops it added, not on an
// absolute count.
const stopped: McpStdioClient[] = [];
const realStop = McpStdioClient.prototype.stop;

before(() => {
  McpStdioClient.prototype.stop = async function (this: McpStdioClient) {
    stopped.push(this);
    return realStop.call(this);
  };
});

after(() => {
  McpStdioClient.prototype.stop = realStop;
  fs.rmSync(binDir, { recursive: true, force: true });
});

describe("register() stale-client teardown", () => {
  it("registers the memory contract and leaves nothing to tear down on first load", () => {
    const host = register(mockApi(config("ses_first")));

    assert.deepEqual(
      [...host.tools].sort(),
      ["memory_get", "memory_get_context", "memory_observe", "memory_search"].sort(),
    );
    assert.deepEqual(host.hooks, ["before_prompt_build", "gateway_stop", "agent_end"]);
    assert.deepEqual(stopped, []);
  });

  it("still stops the stale client when the reload's profile is rejected", () => {
    const reloaded = register(mockApi(config("ses_stale")));
    assert.deepEqual(
      stopped.map(sessionIdOf),
      ["ses_first"],
      "the successful reload tears down the client it replaced",
    );
    assert.match(
      reloaded.warnings.join("\n"),
      /previous client still active during register\(\)/,
    );

    const mark = stopped.length;
    const failed = mockApi(config("ses_rejected", { profile: "expert" }));

    assert.throws(
      () => plugin.register(failed.api),
      /profile 'expert' cannot run the OpenClaw adapter/,
    );

    // The point of the ordering: register() aborted, and the subprocess the
    // previous registration owned was stopped on the way out rather than
    // stranded with no hook left that could ever reach it.
    assert.deepEqual(
      stopped.slice(mark).map(sessionIdOf),
      ["ses_stale"],
      "a rejected reload must still stop the client it was replacing",
    );
    assert.deepEqual(failed.tools, [], "a rejected register() registers no tools");
    assert.deepEqual(failed.hooks, [], "a rejected register() registers no hooks");
  });

  it("does not tear the same stale client down a second time", () => {
    const mark = stopped.length;
    const recovered = register(mockApi(config("ses_recovered")));

    assert.deepEqual(
      stopped.slice(mark),
      [],
      "the client stopped by the failed reload is already gone; only a live one may be stopped",
    );
    assert.ok(
      !recovered.warnings.some((w) => /previous client still active/.test(w)),
      `unexpected teardown warning: ${recovered.warnings.join(" | ")}`,
    );
    assert.deepEqual(recovered.tools.length, 4);
  });

  it("keeps tearing the stale client down across a successful reload", () => {
    const mark = stopped.length;
    register(mockApi(config("ses_again")));

    assert.deepEqual(stopped.slice(mark).map(sessionIdOf), ["ses_recovered"]);
  });
});
