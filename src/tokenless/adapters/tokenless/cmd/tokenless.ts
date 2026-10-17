/**
 * Tokenless mod for Command Code (`cmd`).
 *
 * Command Code discovers this file under ~/.commandcode/mods — the installer
 * symlinks it there — and compiles it with jiti, so it ships with no build
 * step. The mod name cmd derives from the file name, `tokenless`, is what
 * `cmd mods list` prints and what `cmd mods disable tokenless` accepts.
 *
 * It stays inside the plain ES module subset on purpose: the shared adapter
 * suite loads these exact bytes as an ES module, and jiti accepts them
 * unchanged. Types live in the JSDoc below rather than in annotations.
 *
 * Wiring: cmd exposes two mutating tool hooks and no tool-schema seam, so this
 * mod delivers readiness gating, RTK command rewriting and response
 * compression (including TOON) — never schema compression.
 *
 *   beforeToolCall -> tool_ready_hook.sh (may block) + rewrite_hook.py
 *   afterToolCall  -> compress_response_hook.py (may replace the result)
 *
 * Everything fails open. A missing runner, a hook timeout, or an oversized or
 * undecodable reply leaves the tool call exactly as cmd would have run it: a
 * Tokenless fault must never cost the user a tool call.
 */

import { spawn } from "node:child_process";
import { existsSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";

/** Stats attribution: `tokenless stats list` reports these savings as cmd. */
export const AGENT_ID = "cmd";

const HOOK_TIMEOUT_MS = 15_000;
const MAX_HOOK_OUTPUT_BYTES = 1024 * 1024;

// cmd's shell tools. `shell_command` is the wire name the TUI shows as `bash`;
// `powershell` is the Windows-native sibling and carries the same `command`
// field, so one rewrite path covers both.
const SHELL_TOOLS = new Set(["shell_command", "powershell"]);

// cmd threads the session through AgentState (`createAgentState` seeds
// `sessionId`). The pre/post pair keys the RTK optimization state on
// (agent, session, tool call), so fall back to a per-process constant rather
// than an empty string when a host build omits it: the two hooks must agree.
const PROCESS_SESSION_ID = `cmd-${process.pid}`;

/**
 * The adapter directory this mod was loaded from.
 *
 * cmd hands jiti the symlink path it discovered under ~/.commandcode/mods, so
 * `import.meta.url` is only a starting point: realpathSync walks back to the
 * installed adapter tree, where the shared hooks live beside it. Any failure
 * degrades to "" and the FHS candidates below take over.
 *
 * @returns {string} absolute directory, or "" when it cannot be determined
 */
function moduleDir() {
  try {
    return dirname(realpathSync(fileURLToPath(import.meta.url)));
  } catch {
    return "";
  }
}

const MODULE_DIR = moduleDir();

/**
 * @param {string} path
 * @returns {boolean} true when `path` is a readable regular file
 */
function isFile(path) {
  try {
    return existsSync(path) && statSync(path).isFile();
  } catch {
    // Hook discovery is fail-open so an unreadable path never blocks startup.
    return false;
  }
}

/**
 * Locate the shared hook dispatcher.
 *
 * @returns {string | null} run-hook.sh path, or null when Tokenless is absent
 */
export function findHookRunner() {
  const adapterDir = process.env.ANOLISA_ADAPTER_DIR;
  const userHome = process.env.HOME && isAbsolute(process.env.HOME) ? process.env.HOME : homedir();
  const candidates = [
    process.env.TOKENLESS_HOOK_RUNNER,
    adapterDir ? join(adapterDir, "common", "hooks", "run-hook.sh") : "",
    MODULE_DIR ? join(MODULE_DIR, "..", "common", "hooks", "run-hook.sh") : "",
    join(
      userHome,
      ".local",
      "share",
      "anolisa",
      "adapters",
      "tokenless",
      "common",
      "hooks",
      "run-hook.sh",
    ),
    "/usr/local/share/anolisa/adapters/tokenless/common/hooks/run-hook.sh",
    "/usr/share/anolisa/adapters/tokenless/common/hooks/run-hook.sh",
  ];
  return candidates.find((candidate) => candidate && isFile(candidate)) ?? null;
}

/**
 * @param {string} raw hook stdout
 * @returns {Record<string, unknown> | null} decoded JSON object, or null
 */
function parseHookOutput(raw) {
  if (!raw.trim()) return null;
  try {
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

/**
 * Run one shared hook over the JSON protocol and decode its reply.
 *
 * @param {string | null} runner run-hook.sh path, or null to skip
 * @param {string} hookName hook script basename
 * @param {Record<string, unknown>} payload protocol request
 * @returns {Promise<Record<string, unknown> | null>} reply, or null on any fault
 */
export function runHook(runner, hookName, payload) {
  if (!runner) return Promise.resolve(null);
  let serialized;
  try {
    serialized = JSON.stringify(payload);
  } catch {
    return Promise.resolve(null);
  }

  return new Promise((resolve) => {
    let stdout = "";
    let stdoutBytes = 0;
    let settled = false;
    let child;
    let timer;
    const ownProcessGroup = process.platform !== "win32";
    try {
      child = spawn("bash", [runner, hookName], {
        env: {
          ...process.env,
          TOKENLESS_AGENT_ID: AGENT_ID,
        },
        detached: ownProcessGroup,
        stdio: ["pipe", "pipe", "ignore"],
      });
    } catch {
      resolve(null);
      return;
    }

    const finish = (value, terminate = false) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (terminate) {
        // A hook can exit while its descendants still hold stdout open. Kill
        // only our detached group, and do not let inherited pipes pin the host.
        if (ownProcessGroup && child.pid) {
          try {
            process.kill(-child.pid, "SIGKILL");
          } catch {
            child.kill("SIGKILL");
          }
        } else {
          child.kill("SIGKILL");
        }
        child.stdin.destroy();
        child.stdout.destroy();
        child.unref();
      }
      resolve(value);
    };
    timer = setTimeout(() => finish(null, true), HOOK_TIMEOUT_MS);

    child.on("error", () => finish(null));
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (settled) return;
      stdoutBytes += Buffer.byteLength(chunk, "utf8");
      if (stdoutBytes > MAX_HOOK_OUTPUT_BYTES) {
        finish(null, true);
        return;
      }
      stdout += chunk;
    });
    child.on("close", (code) => {
      finish(code === 0 ? parseHookOutput(stdout) : null);
    });

    child.stdin.on("error", () => {
      finish(null, true);
    });
    child.stdin.end(serialized);
  });
}

/**
 * @param {{ sessionId?: unknown } | undefined} state cmd's AgentState
 * @returns {string} the session id both hooks of one call agree on
 */
function sessionIdOf(state) {
  const value = state?.sessionId;
  return typeof value === "string" && value ? value : PROCESS_SESSION_ID;
}

/**
 * Translate one cmd tool-call event into the shared protocol payload.
 *
 * The hooks resolve the call id from `tool_use_id` first, so cmd's
 * `toolCallId` is written into that slot and kept under its Protocol v2 name
 * for readers that expect it.
 *
 * @param {Record<string, any>} call beforeToolCall / afterToolCall event
 * @param {unknown} [toolResponse] model-visible result, post_tool only
 * @returns {Record<string, unknown>} protocol request payload
 */
export function toolPayload(call, toolResponse) {
  const payload = {
    session_id: sessionIdOf(call?.state),
    tool_use_id: call?.toolCallId ?? "",
    tool_call_id: call?.toolCallId ?? "",
    tool_name: call?.toolName ?? "",
    tool_input: call?.input && typeof call.input === "object" ? call.input : {},
  };
  if (toolResponse !== undefined) payload.tool_response = toolResponse;
  return payload;
}

/**
 * @param {Record<string, any> | null} result decoded hook reply
 * @returns {Record<string, any> | null} its hookSpecificOutput object
 */
function hookSpecificOutput(result) {
  const output = result?.hookSpecificOutput;
  return output && typeof output === "object" ? output : null;
}

/**
 * @param {unknown} block
 * @returns {boolean} true for a `{ type: "text", text: string }` block
 */
function isTextBlock(block) {
  return (
    !!block && typeof block === "object" && block.type === "text" && typeof block.text === "string"
  );
}

/**
 * The model-visible text of a cmd tool result.
 *
 * cmd normalizes a result through its own `normalizeToolResultContent`, so a
 * hook sees a string, an array of content blocks, or an arbitrary JSON value.
 * Only the first two carry text Tokenless may shrink.
 *
 * @param {unknown} result cmd's `afterToolCall` result
 * @returns {string | null} the text, or null when there is none to compress
 */
export function resultText(result) {
  if (typeof result === "string") return result;
  if (Array.isArray(result)) {
    const texts = result.filter(isTextBlock).map((block) => block.text);
    return texts.length ? texts.join("\n") : null;
  }
  return null;
}

/**
 * Rebuild a tool result with its text replaced, preserving the incoming shape.
 *
 * Non-text blocks (images, custom entries) keep their position; the first text
 * block carries the compressed text and the remaining ones are dropped, since
 * their content was folded into it.
 *
 * @param {unknown} result cmd's `afterToolCall` result
 * @param {string} text compressed replacement
 * @returns {unknown} the rewritten result, or null when there is no text slot
 */
export function withReplacedText(result, text) {
  if (typeof result === "string") return text;
  if (!Array.isArray(result)) return null;
  let placed = false;
  const rebuilt = [];
  for (const block of result) {
    if (!isTextBlock(block)) {
      rebuilt.push(block);
      continue;
    }
    if (placed) continue;
    placed = true;
    rebuilt.push({ ...block, text });
  }
  return placed ? rebuilt : null;
}

/**
 * Build the ModHooks object Tokenless registers through `cmd.hooks(...)`.
 *
 * @param {{ runner?: string | null }} [options] test seam for the hook runner
 * @returns {Record<string, (call: Record<string, any>) => Promise<any>>} mod hooks
 */
export function createTokenlessHooks(options = {}) {
  const runner = options.runner !== undefined ? options.runner : findHookRunner();

  return {
    /**
     * Gate the call on readiness, then rewrite a shell command for RTK.
     *
     * @param {Record<string, any>} call `{ toolCallId, toolName, input, state }`
     * @returns {Promise<object | undefined>} cmd's beforeToolCall output
     */
    async beforeToolCall(call) {
      const payload = toolPayload(call);

      // Readiness first: a blocked call never reaches the rewrite hook, and
      // cmd turns the block reason into the tool_result the model sees.
      const readyResult = await runHook(runner, "tool_ready_hook.sh", payload);
      if (readyResult?.decision === "block") {
        const reason = readyResult.reason;
        return {
          block: true,
          additionalContext:
            typeof reason === "string" && reason
              ? reason
              : "Tokenless reports that the tool is not ready",
        };
      }

      const output = {};
      let opinion = false;

      const readyContext = hookSpecificOutput(readyResult)?.additionalContext;
      if (typeof readyContext === "string" && readyContext) {
        // cmd appends a beforeToolCall additionalContext to the tool_result
        // after execution, so this side caches nothing across the two hooks.
        output.additionalContext = readyContext;
        opinion = true;
      }

      const command = call?.input?.command;
      if (SHELL_TOOLS.has(call?.toolName) && typeof command === "string" && command) {
        const rewriteResult = await runHook(runner, "rewrite_hook.py", payload);
        const rewritten = hookSpecificOutput(rewriteResult)?.updatedInput?.command;
        if (typeof rewritten === "string" && rewritten && rewritten !== command) {
          // cmd replaces the input wholesale and chains it into the next mod,
          // so every other field has to be carried over.
          output.input = { ...call.input, command: rewritten };
          opinion = true;
        }
      }

      return opinion ? output : undefined;
    },

    /**
     * Compress the model-visible result and forward additive diagnostics.
     *
     * @param {Record<string, any>} call `{ ..., result, isError }`
     * @returns {Promise<object | undefined>} cmd's afterToolCall output
     */
    async afterToolCall(call) {
      const text = resultText(call?.result);
      if (text === null) return undefined;

      // Send the extracted text, not the block envelope: Core measures and
      // compresses what the model actually reads.
      const payload = toolPayload(call, text);
      if (call?.isError === true) payload.is_error = true;

      const result = await runHook(runner, "compress_response_hook.py", payload);
      const hookOutput = hookSpecificOutput(result);
      const output = {};
      let opinion = false;

      const compressed = hookOutput?.updatedToolOutput;
      if (typeof compressed === "string" && compressed) {
        const content = withReplacedText(call.result, compressed);
        if (content !== null) {
          output.content = content;
          opinion = true;
        }
      }

      const context = hookOutput?.additionalContext;
      if (typeof context === "string" && context) {
        // cmd appends this as its own text block after `content`.
        output.additionalContext = context;
        opinion = true;
      }

      return opinion ? output : undefined;
    },
  };
}

/**
 * The mod factory cmd calls with its ModApi.
 *
 * @param {{ hooks: (hooks: object) => { dispose: () => void } }} cmd ModApi
 * @returns {void}
 */
export default function tokenless(cmd) {
  cmd.hooks(createTokenlessHooks());
}
