/**
 * Unit tests for the MCP stdio client.
 *
 * Covers:
 * - `resolveMcpToolName` (the real TOOL_NAME_MAP via its exported wrapper)
 * - `buildChildEnv` allowlist behaviour
 * - `McpStdioClient.stop()` safety on an unstarted client
 * - `callTool` rejection when the binary can't spawn
 * - Request-timer release on responses, write failures, exit and shutdown
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import type { ChildProcess } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { McpStdioClient, buildChildEnv, resolveMcpToolName } from "../../src/mcp-client.js";

describe("resolveMcpToolName", () => {
  it("maps namespaced OpenClaw tools to native MCP names", () => {
    assert.equal(resolveMcpToolName("anolisa_memory_get"), "mem_read");
    assert.equal(resolveMcpToolName("anolisa_memory_search"), "memory_search");
  });

  it("passes native MCP names and identity-mapped contracts through", () => {
    assert.equal(resolveMcpToolName("memory_search"), "memory_search");
    assert.equal(resolveMcpToolName("memory_observe"), "memory_observe");
    assert.equal(resolveMcpToolName("memory_get_context"), "memory_get_context");
  });

  it("passes unknown names through unchanged", () => {
    assert.equal(resolveMcpToolName("future_tool"), "future_tool");
  });
});

describe("buildChildEnv", () => {
  it("keeps allow-listed exact vars", () => {
    const env = buildChildEnv(
      { PATH: "/usr/bin", HOME: "/home/u", FOO: "secret" } as any,
      {},
    );
    assert.equal(env.PATH, "/usr/bin");
    assert.equal(env.HOME, "/home/u");
    assert.equal(env.FOO, undefined);
  });

  it("keeps prefix-matched vars (MEMORY_*, RUST_*) + exact USER_ID", () => {
    const env = buildChildEnv(
      {
        MEMORY_PROFILE: "expert",
        RUST_LOG: "debug",
        USER_ID: "alice",
        AWS_SECRET_KEY: "leak",
      } as any,
      {},
    );
    assert.equal(env.MEMORY_PROFILE, "expert");
    assert.equal(env.RUST_LOG, "debug");
    assert.equal(env.USER_ID, "alice");
    assert.equal(env.AWS_SECRET_KEY, undefined);
  });

  it("does NOT leak USER_ID-prefixed look-alikes (regression for R6-2)", () => {
    // Earlier USER_ID was in the prefix list, so a startsWith match
    // would have let USER_IDX / USER_ID_FOO through. Now USER_ID is
    // an exact-match entry and only the literal name passes.
    const env = buildChildEnv(
      { USER_IDX: "leak", USER_ID_FOO: "leak2", USER_ID: "alice" } as any,
      {},
    );
    assert.equal(env.USER_ID, "alice");
    assert.equal(env.USER_IDX, undefined);
    assert.equal(env.USER_ID_FOO, undefined);
  });

  it("does NOT leak MEMORY-prefixed look-alikes that miss the underscore", () => {
    // MEMORYCACHE has no underscore, so it should not match `MEMORY_`.
    const env = buildChildEnv({ MEMORYCACHE: "leak", MEMORY_PROFILE: "advanced" } as any, {});
    assert.equal(env.MEMORY_PROFILE, "advanced");
    assert.equal(env.MEMORYCACHE, undefined);
  });

  it("plugin env overrides allow-listed parent value", () => {
    const env = buildChildEnv(
      { MEMORY_PROFILE: "basic", PATH: "/usr/bin" } as any,
      { MEMORY_PROFILE: "advanced" },
    );
    assert.equal(env.MEMORY_PROFILE, "advanced");
    assert.equal(env.PATH, "/usr/bin");
  });
});

describe("McpStdioClient", () => {
  const cfg = {
    binaryPath: "/nonexistent/agent-memory-binary",
    userId: "0",
    profile: "advanced" as const,
    maxReadBytes: 1_048_576,
    maxWriteBytes: 16_777_216,
    // Inert here — the binary never spawns — but part of AgentMemoryConfig,
    // which the client forwards as MEMORY_SESSION_ID / MEMORY_SESSION_DIR.
    sessionId: "ses_test",
    sessionDir: "/nonexistent/agent-memory-session-dir",
  };

  it("stop() is safe when the process was never started", async () => {
    const client = new McpStdioClient(cfg);
    await client.stop();
  });

  for (const ignoreEof of [false, true]) {
    it(`stop() waits for ${ignoreEof ? "SIGKILL" : "stdin EOF"} before returning`, {
      timeout: 10_000,
    }, async (context) => {
      const directory = await mkdtemp(join(tmpdir(), "memory-stop-test-"));
      const binaryPath = join(directory, "mcp-server");
      const eofPath = join(directory, "eof");
      let childPid: number | undefined;
      context.after(async () => {
        if (childPid !== undefined) {
          try {
            process.kill(childPid, "SIGKILL");
          } catch (error) {
            assert.equal((error as NodeJS.ErrnoException).code, "ESRCH");
          }
        }
        await rm(directory, { recursive: true, force: true });
      });
      await writeFile(binaryPath, `#!${process.execPath}
const readline = require("node:readline");
const fs = require("node:fs");
setInterval(() => {}, 1000);
process.on("SIGTERM", () => {});
process.stdin.on("end", () => {
  if (${ignoreEof}) return;
  fs.writeFileSync(${JSON.stringify(eofPath)}, "eof");
  process.exit(0);
});
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  const result = request.method === "initialize" ? {} : {
    content: [{ type: "text", text: String(process.pid) }],
  };
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\\n");
});
`, { mode: 0o700 });
      const client = new McpStdioClient({ ...cfg, binaryPath });
      childPid = Number(await client.callTool("memory_get_context", {}));
      assert.ok(Number.isSafeInteger(childPid) && childPid > 0);

      await client.stop();

      assert.throws(() => process.kill(childPid!, 0), { code: "ESRCH" });
      if (!ignoreEof) {
        assert.equal(await readFile(eofPath, "utf8"), "eof");
      }
      await client.stop();
    });
  }

  for (const resultMode of ["success", "error", "exit", "stop", "write-error"] as const) {
    it(`releases request timers after worker ${resultMode}`, { timeout: 10_000 }, async (context) => {
      const directory = await mkdtemp(join(tmpdir(), "memory-mcp-timers-"));
      const binaryPath = join(directory, "worker.cjs");
      const timers = new Set<ReturnType<typeof setTimeout>>();
      const realTimeout = globalThis.setTimeout;
      const realClear = globalThis.clearTimeout;
      const client = new McpStdioClient({ ...cfg, binaryPath });
      context.after(async () => {
        try { await client.stop(); } finally {
          for (const timer of timers) realClear(timer);
          context.mock.restoreAll();
          await rm(directory, { recursive: true, force: true });
        }
      });
      await writeFile(binaryPath, `#!${process.execPath}
const readline = require("node:readline");
const mode = ${JSON.stringify(resultMode)};
process.stderr.write("fixture diagnostic\\n");
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method !== "initialize" && mode === "exit") process.exit(9);
  if (mode === "stop" && request.params?.arguments?.wait) return;
  const reply = request.method === "initialize" ? { result: {} }
    : mode === "error" ? { error: { code: -32000, message: "test failure" } }
    : { result: { content: [{ type: "text", text: "memory result" }] } };
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, ...reply }) + "\\n");
});
`, { mode: 0o700 });
      context.mock.method(globalThis, "setTimeout", (
        callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]
      ) => {
        const timer = realTimeout(callback, delay, ...args);
        // Only initialize and memory_get_context deadlines, not stderr or
        // shutdown watchdog timers (the fixture intentionally emits stderr).
        if (delay === 10_000 || delay === 120_000) timers.add(timer);
        return timer;
      });
      context.mock.method(globalThis, "clearTimeout", (timer: ReturnType<typeof setTimeout>) => {
        timers.delete(timer);
        realClear(timer);
      });
      if (resultMode === "error" || resultMode === "exit") {
        await assert.rejects(client.callTool("memory_get_context", {}),
          resultMode === "error" ? /test failure/ : /code=9/);
      } else {
        for (let index = 0; index < 10; index++) {
          assert.equal(await client.callTool("memory_get_context", {}), "memory result");
          assert.equal(timers.size, 0, "completed calls release request timers");
        }
        if (resultMode === "stop") {
          const rejected = assert.rejects(client.callTool("memory_get_context", { wait: true }), Error);
          await new Promise<void>((resolve) => setImmediate(resolve));
          assert.equal(timers.size, 1, "one call is outstanding before stop");
          await client.stop();
          await rejected;
        } else if (resultMode === "write-error") {
          const child = Reflect.get(client, "proc") as ChildProcess;
          assert.ok(child.stdin);
          context.mock.method(child.stdin, "write", (_payload: string, callback: (error: Error) => void) => {
            queueMicrotask(() => callback(new Error("fixture write failure")));
            return false;
          });
          await assert.rejects(client.callTool("memory_get_context", {}), /Failed to write.*fixture write failure/);
        }
      }
      assert.equal(timers.size, 0, "terminal paths release request timers");
    });
  }

  it("callTool rejects with a real error when the binary cannot spawn", async () => {
    const client = new McpStdioClient(cfg);
    try {
      await client.callTool("memory_search", { query: "x" });
      assert.fail("expected callTool to reject");
    } catch (err: any) {
      // Error surfaces from spawn (ENOENT) or the initialize timeout.
      assert.ok(err instanceof Error);
      assert.ok(err.message.length > 0);
    } finally {
      await client.stop();
    }
  });
});
