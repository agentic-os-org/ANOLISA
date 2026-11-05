/**
 * Unit tests for the MCP stdio client.
 *
 * Covers:
 * - `resolveMcpToolName` (the real TOOL_NAME_MAP via its exported wrapper)
 * - `buildChildEnv` allowlist behaviour
 * - `McpStdioClient.stop()` safety on an unstarted client
 * - `callTool` rejection when the binary can't spawn
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
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

  // GH-4863: a failed initialize handshake used to leave the worker we had
  // just spawned alive while `initialized` stayed false. The next tool call
  // then spawned a replacement and overwrote `this.proc`, orphaning the
  // first worker where `stop()` could no longer reach it — and leaving its
  // exit handler behind to be booked as a crash whenever it finally died.
  const handshakes = [
    { kind: "error", label: "a JSON-RPC error" },
    { kind: "null", label: "an empty result" },
  ] as const;
  for (const { kind: handshake, label } of handshakes) {
    it(`releases the worker when initialize returns ${label}`, {
      timeout: 20_000,
    }, async (context) => {
      const directory = await mkdtemp(join(tmpdir(), "memory-init-fail-"));
      const binaryPath = join(directory, "mcp-server");
      const pidPath = join(directory, "pids");
      let pids: number[] = [];
      context.after(async () => {
        for (const pid of pids) {
          try {
            process.kill(pid, "SIGKILL");
          } catch (error) {
            assert.equal((error as NodeJS.ErrnoException).code, "ESRCH");
          }
        }
        await rm(directory, { recursive: true, force: true });
      });
      const failedInitialize = handshake === "error"
        ? `{ jsonrpc: "2.0", id: request.id,
             error: { code: -32603, message: "handshake boom" } }`
        : `{ jsonrpc: "2.0", id: request.id, result: null }`;
      await writeFile(binaryPath, `#!${process.execPath}
const fs = require("node:fs");
const readline = require("node:readline");
fs.appendFileSync(${JSON.stringify(pidPath)}, process.pid + "\\n");
// Shared across respawns, so only the very first worker fails its handshake
// and the retry on the same client can succeed.
const counterPath = ${JSON.stringify(pidPath)} + ".n";
const attempt = Number(fs.existsSync(counterPath) ? fs.readFileSync(counterPath, "utf8") : 0) + 1;
fs.writeFileSync(counterPath, String(attempt));
setInterval(() => {}, 1000);
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === "initialize" && attempt === 1) {
    process.stdout.write(JSON.stringify(${failedInitialize}) + "\\n");
    return;
  }
  const result = request.method === "initialize" ? {} : {
    content: [{ type: "text", text: "ready" }],
  };
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\\n");
});
`, { mode: 0o700 });

      const client = new McpStdioClient({ ...cfg, binaryPath });
      const spawnedPids = async (): Promise<number[]> =>
        (await readFile(pidPath, "utf8")).split("\n").filter(Boolean).map(Number);
      const assertGone = (pid: number) =>
        assert.throws(() => process.kill(pid, 0), { code: "ESRCH" }, `worker ${pid} is still alive`);

      // 1. The handshake failure reaches the caller unchanged — releasing
      //    the worker must not replace it with a teardown error.
      let failure: unknown;
      try {
        await client.callTool("memory_get_context", {});
      } catch (error) {
        failure = error;
      }
      assert.ok(failure instanceof Error, "callTool must reject");
      assert.match(
        failure.message,
        handshake === "error"
          ? /JSON-RPC error -32603: handshake boom/
          : /initialize handshake returned no result/,
      );

      // 2. That worker is already gone, and no replacement was spawned
      //    behind the caller's back.
      pids = await spawnedPids();
      assert.equal(pids.length, 1, "exactly one worker spawned for the failed handshake");
      assertGone(pids[0]);

      // 3. The same client still recovers — the deliberate teardown is not
      //    booked as a crash, so the respawn budget is untouched.
      assert.equal(await client.callTool("memory_get_context", {}), "ready");
      pids = await spawnedPids();
      assert.equal(pids.length, 2, "the retry spawns exactly one replacement");

      // 4. ...and stop() reaches every worker it ever started.
      await client.stop();
      for (const pid of pids) assertGone(pid);
      await client.stop();
    });
  }

  it("decodes a reply the pipe splits inside a multibyte character", {
    timeout: 30_000,
  }, async (context) => {
    // Regression for #4694: every stdout chunk used to be decoded on its own
    // with `Buffer.toString("utf8")`, so a chunk boundary landing inside a
    // multibyte character replaced both halves with U+FFFD before JSON.parse
    // ever ran, and the tool result came back corrupted.
    //
    // The reply below is far larger than the 64 KiB pipe buffer, so the child
    // cannot hand it over in one piece and the parent must read it in several
    // chunks — that is what makes the split reproducible instead of scheduler
    // dependent. The payload alternates a 3-byte and a 4-byte code point
    // (7 bytes per unit), so consecutive 64 KiB boundaries cannot all land on
    // a code point boundary: per-chunk decoding has to corrupt the text.
    const directory = await mkdtemp(join(tmpdir(), "memory-unicode-test-"));
    const binaryPath = join(directory, "mcp-server");
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

    const units = 36_000;
    const expected = String.fromCodePoint(0x8bb0, 0x1f9e0).repeat(units);
    await writeFile(
      binaryPath,
      `#!${process.execPath}
const readline = require("node:readline");
setInterval(() => {}, 1000);
const TEXT = String.fromCodePoint(0x8bb0, 0x1f9e0).repeat(${units});
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  const result =
    request.method === "initialize"
      ? {}
      : {
          content: [
            { type: "text", text: TEXT },
            { type: "text", text: String(process.pid) },
          ],
        };
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\\n");
});
`,
      { mode: 0o700 },
    );

    const client = new McpStdioClient({ ...cfg, binaryPath });
    const reply = await client.callTool("memory_get_context", {});
    const lastNewline = reply.lastIndexOf("\n");
    childPid = Number(reply.slice(lastNewline + 1));
    const text = reply.slice(0, lastNewline);

    assert.equal(text.length, expected.length);
    assert.ok(
      !text.includes("\ufffd"),
      "reply contains a U+FFFD replacement character: the pipe split a multibyte character",
    );
    assert.equal(text, expected);

    await client.stop();
  });

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
