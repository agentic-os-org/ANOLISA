/**
 * MCP stdio client for agent-memory.
 *
 * Spawns the agent-memory binary as a child process, communicates via
 * JSON-RPC 2.0 over stdin/stdout pipes. Implements lazy start (first
 * tool call triggers spawn), single-instance reuse, and automatic
 * respawn on child process crash (bounded by MAX_RESPAWN_ATTEMPTS).
 */

import { ChildProcess, spawn } from "node:child_process";
import type { AgentMemoryConfig } from "./config.js";

// Build-time injection by esbuild's --define flag (see package.json
// "build" script). Falls back to "0.0.0-dev" when the bundle is loaded
// outside the Makefile pipeline (e.g. `tsx tests/...`). The Makefile
// `sync-versions` target writes Cargo.toml's version into package.json
// just before npm runs, so the bundle always ships in lock-step with
// the Rust crate — no second source of truth.
declare const __AGENT_MEMORY_VERSION__: string | undefined;
const PLUGIN_VERSION: string =
  typeof __AGENT_MEMORY_VERSION__ === "string" ? __AGENT_MEMORY_VERSION__ : "0.0.0-dev";

type JsonRpcRequest = {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: Record<string, unknown>;
};

type JsonRpcResponse = {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: {
    code: number;
    message: string;
    data?: unknown;
  };
};

type PendingCall = {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
};

const INIT_TIMEOUT_MS = 10_000;
// Per-method timeouts. The defaults are conservative; agent-memory
// tools that walk the mount tree (mem_grep, memory_get_context,
// memory_search after a fresh `cargo vendor`) can take seconds on a
// large store. The plugin config doesn't expose this yet, but the
// table is the single place to tune it.
const DEFAULT_CALL_TIMEOUT_MS = 30_000;
const TOOL_TIMEOUT_MS: Record<string, number> = {
  mem_grep: 120_000,
  memory_search: 120_000,
  memory_get_context: 120_000,
  mem_snapshot: 120_000,
  mem_snapshot_restore: 300_000,
};
const MAX_RESPAWN_ATTEMPTS = 3;

// Allowlist of env vars passed to the agent-memory subprocess. Avoid
// leaking the parent's full environment (which on a desktop dev box
// may include unrelated secrets) into the child process.
//
// USER_ID is an exact-match entry, NOT a prefix: a prefix match would
// accidentally let unrelated vars like USER_IDX through.
const ENV_ALLOWLIST = new Set([
  "PATH",
  "HOME",
  "USER",
  "USER_ID",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "TZ",
  "TMPDIR",
  "XDG_RUNTIME_DIR",
]);
// Prefixes end with `_` so partial-name collisions (USER_ID vs USER_IDX,
// MEMORY_FOO vs MEMORYCACHE) cannot leak through.
const ENV_PREFIX_ALLOWLIST = ["MEMORY_", "RUST_"];

// stderr from the child is logged with a fixed-size ring buffer so a
// runaway loop can't flood the gateway's logs.
const STDERR_RING_CAPACITY = 64; // most recent N lines kept per flush cycle
const STDERR_FLUSH_INTERVAL_MS = 5_000;

// OpenClaw contract name → agent-memory MCP tool name mapping.
const TOOL_NAME_MAP: Record<string, string> = {
  anolisa_memory_search: "memory_search",
  anolisa_memory_get: "mem_read",
  memory_observe: "memory_observe",
  memory_get_context: "memory_get_context",
};

/** Resolve `contractName` (what the OpenClaw layer calls) to the
 * native agent-memory tool name. Exported so the unit test exercises
 * the real table instead of restating it. */
export function resolveMcpToolName(contractName: string): string {
  return TOOL_NAME_MAP[contractName] ?? contractName;
}

/** Plain MCP `CallToolResult` shape used for `isError` detection. */
type CallToolResultLike = {
  content?: Array<{ type?: string; text?: string }>;
  isError?: boolean;
};

/** Build the env handed to the agent-memory child, masking everything
 * outside the allowlist. Plugin config overrides take precedence. */
export function buildChildEnv(
  parent: NodeJS.ProcessEnv,
  pluginEnv: Record<string, string>,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(parent)) {
    if (typeof v !== "string") continue;
    if (ENV_ALLOWLIST.has(k) || ENV_PREFIX_ALLOWLIST.some((p) => k.startsWith(p))) {
      out[k] = v;
    }
  }
  for (const [k, v] of Object.entries(pluginEnv)) {
    out[k] = v;
  }
  return out;
}

export class McpStdioClient {
  private proc: ChildProcess | null = null;
  private nextId = 1;
  private pending: Map<number, PendingCall> = new Map();
  private buffer = "";
  private initialized = false;
  private respawnAttempts = 0;
  private giveUp = false;
  private startingPromise: Promise<void> | null = null;
  private readonly config: AgentMemoryConfig;
  // Bounded ring buffer for child stderr so a chatty/looping subprocess
  // can't blow up the gateway's log volume.
  private stderrRing: string[] = [];
  private stderrFlushTimer: NodeJS.Timeout | null = null;
  private stderrDroppedSinceLastFlush = 0;

  constructor(config: AgentMemoryConfig) {
    this.config = config;
  }

  /** Lazy-start: spawn + initialize on first use. */
  private async ensureStarted(): Promise<void> {
    if (this.giveUp) {
      throw new Error(
        `agent-memory process repeatedly crashed; gave up after ${MAX_RESPAWN_ATTEMPTS} respawn attempts`,
      );
    }
    if (this.initialized && this.proc && !this.proc.killed) {
      return;
    }
    if (this.startingPromise) {
      return this.startingPromise;
    }
    this.startingPromise = this.doStart();
    try {
      await this.startingPromise;
    } finally {
      this.startingPromise = null;
    }
  }

  private async doStart(): Promise<void> {
    this.spawnProcess();

    // Send MCP initialize handshake.
    const initResult = await this.sendRaw("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: {
        name: "openclaw-agent-memory-plugin",
        version: PLUGIN_VERSION,
      },
    });

    if (!initResult) {
      throw new Error("agent-memory initialize handshake returned no result");
    }

    // Send initialized notification (no response expected).
    this.sendNotification("notifications/initialized");

    this.initialized = true;
    // Successful init: reset the respawn counter so the next crash
    // gets a full quota of retries again.
    this.respawnAttempts = 0;
  }

  private spawnProcess(): void {
    const pluginEnv: Record<string, string> = {
      MEMORY_PROFILE: this.config.profile,
      MEMORY_MAX_READ_BYTES: String(this.config.maxReadBytes),
      MEMORY_MAX_WRITE_BYTES: String(this.config.maxWriteBytes),
      // Pin the session id + dir across every spawn (and across
      // lazy-start respawns) so a single OpenClaw plugin instance
      // looks like one session to agent-memory. Without this, every
      // crash-respawn would generate a fresh `ses_<ULID>` and
      // `mem_promote` would never find the previous scratch.
      MEMORY_SESSION_ID: this.config.sessionId,
      MEMORY_SESSION_DIR: this.config.sessionDir,
    };

    // Only set USER_ID if the config specifies one (agent-memory defaults to OS uid).
    if (this.config.userId) {
      pluginEnv.USER_ID = this.config.userId;
    }

    const env = buildChildEnv(process.env, pluginEnv);

    this.proc = spawn(this.config.binaryPath, ["serve"], {
      stdio: ["pipe", "pipe", "pipe"],
      env,
      detached: false,
    });

    this.proc.stdout?.on("data", (chunk: Buffer) => {
      this.handleData(chunk.toString("utf8"));
    });

    this.proc.stderr?.on("data", (chunk: Buffer) => {
      this.appendStderr(chunk.toString("utf8"));
    });

    this.proc.on("exit", (code, signal) => {
      this.handleExit(code, signal);
    });

    this.proc.on("error", (err) => {
      this.handleError(err);
    });
  }

  /** Call an MCP tool by OpenClaw contract name (auto-mapped). */
  async callTool(contractName: string, args: Record<string, unknown>): Promise<string> {
    return this.callToolByName(resolveMcpToolName(contractName), args);
  }

  /** Call an MCP tool by its native agent-memory name. */
  async callToolByName(name: string, args: Record<string, unknown>): Promise<string> {
    await this.ensureStarted();

    const result = await this.sendRaw(
      "tools/call",
      {
        name,
        arguments: args,
      },
      TOOL_TIMEOUT_MS[name] ?? DEFAULT_CALL_TIMEOUT_MS,
    );

    // MCP tools/call result shape:
    //   { content: [{type: "text", text: "..."}], isError?: boolean }
    const resultObj = result as CallToolResultLike | null;
    if (!resultObj?.content || !Array.isArray(resultObj.content)) {
      throw new Error(`agent-memory tool '${name}' returned unexpected result shape`);
    }

    const text = resultObj.content
      .filter((block) => block.type === "text" && typeof block.text === "string")
      .map((block) => block.text!)
      .join("\n");

    // MCP spec: isError:true means the tool ran but returned a domain
    // error (file not found, sandbox refusal, size cap exceeded, ...).
    // Throw so OpenClaw's caller can branch instead of mistaking the
    // error string for a successful payload.
    if (resultObj.isError === true) {
      throw new Error(`agent-memory tool '${name}' failed: ${text}`);
    }

    return text;
  }

  /** Send a JSON-RPC request and wait for the response. */
  private sendRaw(
    method: string,
    params?: Record<string, unknown>,
    timeoutOverrideMs?: number,
  ): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.proc || this.proc.killed) {
        reject(new Error("agent-memory process not running"));
        return;
      }

      const id = this.nextId++;
      const request: JsonRpcRequest = {
        jsonrpc: "2.0",
        id,
        method,
        params,
      };

      const pending: PendingCall = { resolve, reject };
      this.pending.set(id, pending);

      const payload = JSON.stringify(request) + "\n";
      this.proc.stdin!.write(payload, (err) => {
        if (err) {
          this.pending.delete(id);
          reject(new Error(`Failed to write to agent-memory stdin: ${err.message}`));
        }
      });

      // Timeout: reject the call if no response arrives.
      const timeoutMs =
        timeoutOverrideMs ?? (method === "initialize" ? INIT_TIMEOUT_MS : DEFAULT_CALL_TIMEOUT_MS);
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`agent-memory call '${method}' timed out after ${timeoutMs}ms`));
        }
      }, timeoutMs).unref();
    });
  }

  /** Send a JSON-RPC notification (no id, no response expected). */
  private sendNotification(method: string, params?: Record<string, unknown>): void {
    if (!this.proc || this.proc.killed) {
      return;
    }
    const notification: Record<string, unknown> = {
      jsonrpc: "2.0",
      method,
    };
    if (params !== undefined) {
      notification.params = params;
    }
    const payload = JSON.stringify(notification) + "\n";
    // Notifications have no id, so a write failure can't reject any
    // pending call — log it instead of swallowing silently.
    this.proc.stdin!.write(payload, (err) => {
      if (err) {
        console.error(
          `[agent-memory] failed to send notification '${method}': ${err.message}`,
        );
      }
    });
  }

  /** Parse incoming stdout data for JSON-RPC responses. */
  private handleData(data: string): void {
    this.buffer += data;

    // JSON-RPC messages are separated by newlines.
    const lines = this.buffer.split("\n");
    // Keep the last (possibly incomplete) fragment in the buffer.
    this.buffer = lines.pop() ?? "";

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) {
        continue;
      }
      try {
        const msg = JSON.parse(trimmed) as JsonRpcResponse;
        this.handleResponse(msg);
      } catch {
        // Not a JSON-RPC message; skip (could be debug output).
      }
    }
  }

  private handleResponse(msg: JsonRpcResponse): void {
    const pending = this.pending.get(msg.id);
    if (!pending) {
      return;
    }
    this.pending.delete(msg.id);

    if (msg.error) {
      pending.reject(
        new Error(`agent-memory JSON-RPC error ${msg.error.code}: ${msg.error.message}`),
      );
    } else {
      pending.resolve(msg.result ?? null);
    }
  }

  /** Append a stderr chunk to the ring buffer and arm the flush timer. */
  private appendStderr(chunk: string): void {
    for (const line of chunk.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      if (this.stderrRing.length >= STDERR_RING_CAPACITY) {
        this.stderrRing.shift();
        this.stderrDroppedSinceLastFlush++;
      }
      this.stderrRing.push(trimmed);
    }
    if (!this.stderrFlushTimer) {
      this.stderrFlushTimer = setTimeout(() => this.flushStderr(), STDERR_FLUSH_INTERVAL_MS);
      this.stderrFlushTimer.unref();
    }
  }

  private flushStderr(): void {
    this.stderrFlushTimer = null;
    if (this.stderrRing.length === 0 && this.stderrDroppedSinceLastFlush === 0) return;
    if (this.stderrDroppedSinceLastFlush > 0) {
      console.error(
        `[agent-memory stderr] (dropped ${this.stderrDroppedSinceLastFlush} earlier lines due to volume)`,
      );
      this.stderrDroppedSinceLastFlush = 0;
    }
    for (const line of this.stderrRing) {
      console.error(`[agent-memory stderr] ${line}`);
    }
    this.stderrRing = [];
  }

  /** Handle child process exit: reject all pending calls. The next
   * `ensureStarted()` will respawn unless we've crossed the cap. */
  private handleExit(code: number | null, signal: string | null): void {
    this.initialized = false;
    this.proc = null;
    this.flushStderr();

    // Reject all pending calls so awaiters don't hang forever.
    for (const [id, pending] of this.pending) {
      this.pending.delete(id);
      pending.reject(
        new Error(
          `agent-memory process exited (code=${code ?? "unknown"}, signal=${signal ?? "none"})`,
        ),
      );
    }

    // Count this as a crash if the exit was unexpected. SIGTERM /
    // SIGKILL from `stop()` are deliberate and don't count.
    if (!this.deliberateStop) {
      this.respawnAttempts++;
      if (this.respawnAttempts >= MAX_RESPAWN_ATTEMPTS) {
        this.giveUp = true;
        console.error(
          `[agent-memory] crashed ${this.respawnAttempts} times; will not respawn further. Last exit code=${code}, signal=${signal}.`,
        );
      } else {
        console.error(
          `[agent-memory] child exited (code=${code}, signal=${signal}); will respawn on next tool call (attempt ${this.respawnAttempts + 1}/${MAX_RESPAWN_ATTEMPTS})`,
        );
      }
    }
    this.deliberateStop = false;
  }

  private handleError(err: Error): void {
    this.initialized = false;
    this.proc = null;
    this.flushStderr();

    for (const [id, pending] of this.pending) {
      this.pending.delete(id);
      pending.reject(new Error(`agent-memory process error: ${err.message}`));
    }
  }

  private deliberateStop = false;

  /** Gracefully shut down the child process. */
  async stop(): Promise<void> {
    this.initialized = false;
    this.deliberateStop = true;

    if (!this.proc) {
      return;
    }

    // Attempt graceful shutdown: send SIGTERM, then wait briefly.
    try {
      this.proc.kill("SIGTERM");
    } catch {
      // Ignore — process may already be dead.
    }

    // Give the process 2 seconds to exit gracefully.
    await new Promise<void>((resolve) => {
      const timeout = setTimeout(() => {
        if (this.proc && !this.proc.killed) {
          this.proc.kill("SIGKILL");
        }
        resolve();
      }, 2000);
      timeout.unref();

      this.proc!.once("exit", () => {
        clearTimeout(timeout);
        resolve();
      });
    });

    this.proc = null;
    this.pending.clear();
    // flushStderr writes once more, but we also need to cancel the
    // pending flush timer so no extra summary line fires after stop().
    this.flushStderr();
    if (this.stderrFlushTimer) {
      clearTimeout(this.stderrFlushTimer);
      this.stderrFlushTimer = null;
    }
  }
}
