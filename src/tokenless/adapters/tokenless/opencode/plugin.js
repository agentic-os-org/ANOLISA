/**
 * Tokenless integration for OpenCode's local plugin API.
 *
 * The adapter translates OpenCode's in-process hooks to the shared Tokenless
 * JSON hook protocol, keeping compression and readiness behavior consistent
 * with the other agent integrations.
 */

import { spawn } from "node:child_process";
import { existsSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";

const AGENT_ID = "opencode";
const HOOK_TIMEOUT_MS = 15_000;
const MAX_HOOK_OUTPUT_BYTES = 1024 * 1024;
const MAX_CACHE_ENTRIES = 256;
const PLUGIN_FILE = fileURLToPath(import.meta.url);
const PLUGIN_DIR = dirname(realpathSync(PLUGIN_FILE));

function isFile(path) {
  try {
    return existsSync(path) && statSync(path).isFile();
  } catch {
    // Hook discovery is fail-open so unreadable paths never block OpenCode startup.
    return false;
  }
}

function findHookRunner() {
  const adapterDir = process.env.ANOLISA_ADAPTER_DIR;
  const userHome = process.env.HOME && isAbsolute(process.env.HOME)
    ? process.env.HOME
    : homedir();
  const candidates = [
    process.env.TOKENLESS_HOOK_RUNNER,
    adapterDir ? join(adapterDir, "common", "hooks", "run-hook.sh") : "",
    // The installed plugin remains beside the shared adapter tree after its
    // global registration symlink is resolved through PLUGIN_FILE.
    join(PLUGIN_DIR, "..", "common", "hooks", "run-hook.sh"),
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

function parseHookOutput(raw) {
  if (!raw.trim()) return null;
  try {
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function runHook(runner, hookName, payload) {
  if (!runner) return Promise.resolve(null);
  let serialized;
  try {
    serialized = JSON.stringify(payload);
  } catch {
    return Promise.resolve(null);
  }

  return new Promise((resolve) => {
    let stdout = "";
    let settled = false;
    let child;
    let timer;
    try {
      child = spawn("bash", [runner, hookName], {
        env: {
          ...process.env,
          TOKENLESS_AGENT_ID: AGENT_ID,
        },
        stdio: ["pipe", "pipe", "ignore"],
      });
    } catch {
      resolve(null);
      return;
    }

    const finish = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    };
    timer = setTimeout(() => {
      child.kill();
      finish(null);
    }, HOOK_TIMEOUT_MS);

    child.on("error", () => finish(null));
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (stdout.length < MAX_HOOK_OUTPUT_BYTES) {
        stdout += chunk.slice(0, MAX_HOOK_OUTPUT_BYTES - stdout.length);
      }
    });
    child.on("close", (code) => {
      finish(code === 0 ? parseHookOutput(stdout) : null);
    });

    child.stdin.on("error", () => {
      child.kill();
      finish(null);
    });
    child.stdin.end(serialized);
  });
}

function toolPayload(input, args, toolResponse) {
  const payload = {
    session_id: input.sessionID,
    tool_use_id: input.callID,
    tool_call_id: input.callID,
    tool_name: input.tool,
    tool_input: args ?? {},
  };
  if (toolResponse !== undefined) payload.tool_response = toolResponse;
  return payload;
}

function hookSpecificOutput(result) {
  const output = result?.hookSpecificOutput;
  return output && typeof output === "object" ? output : null;
}

function prependContext(context, content) {
  if (typeof context !== "string" || !context) return content;
  return content ? `${context}\n${content}` : context;
}

export const TokenlessPlugin = async () => {
  const runner = findHookRunner();
  const readinessContext = new Map();
  const schemaCache = new Map();

  const cacheValue = (cache, key, value) => {
    if (cache.has(key)) {
      cache.delete(key);
    } else if (cache.size >= MAX_CACHE_ENTRIES) {
      cache.delete(cache.keys().next().value);
    }
    cache.set(key, value);
  };

  const cachedValue = (cache, key) => {
    if (!cache.has(key)) return undefined;
    const value = cache.get(key);
    cache.delete(key);
    cache.set(key, value);
    return value;
  };

  return {
    "tool.execute.before": async (input, output) => {
      const payload = toolPayload(input, output.args);
      const readyResult = await runHook(runner, "tool_ready_hook.sh", payload);
      if (readyResult?.decision === "block") {
        throw new Error(
          readyResult.reason || "Tokenless reports that the tool is not ready",
        );
      }

      const readyOutput = hookSpecificOutput(readyResult);
      if (typeof readyOutput?.additionalContext === "string") {
        cacheValue(
          readinessContext,
          `${input.sessionID}:${input.callID}`,
          readyOutput.additionalContext,
        );
      }

      if (input.tool !== "bash" || typeof output.args?.command !== "string") return;
      const rewriteResult = await runHook(runner, "rewrite_hook.py", payload);
      const rewritten = hookSpecificOutput(rewriteResult)?.updatedInput?.command;
      if (typeof rewritten === "string" && rewritten) output.args.command = rewritten;
    },

    "tool.execute.after": async (input, output) => {
      const key = `${input.sessionID}:${input.callID}`;
      const readyContext = readinessContext.get(key);
      readinessContext.delete(key);

      const result = await runHook(
        runner,
        "compress_response_hook.py",
        toolPayload(input, input.args, output.output),
      );
      const hookOutput = hookSpecificOutput(result);
      const compressedOutput = hookOutput?.updatedToolOutput;
      if (typeof compressedOutput === "string") {
        output.output = compressedOutput;
      }
      const context = [readyContext, hookOutput?.additionalContext]
        .filter((value) => typeof value === "string" && value)
        .join("\n");
      output.output = prependContext(context, output.output);
    },

    "tool.definition": async (input, output) => {
      const declaration = {
        name: input.toolID,
        description: output.description,
        parameters: output.parameters,
      };
      let cacheKey;
      try {
        cacheKey = JSON.stringify(declaration);
      } catch {
        return;
      }
      let tools = cachedValue(schemaCache, cacheKey);
      if (!tools) {
        const result = await runHook(runner, "compress_schema_hook.py", {
          llm_request: { config: { tools: [declaration] } },
        });
        tools = hookSpecificOutput(result)?.llm_request?.config?.tools;
        if (Array.isArray(tools)) cacheValue(schemaCache, cacheKey, tools);
      }
      if (!Array.isArray(tools) || tools.length !== 1 || tools[0]?.name !== input.toolID) {
        return;
      }

      if (typeof tools[0].description === "string") output.description = tools[0].description;
      if (tools[0].parameters && typeof tools[0].parameters === "object") {
        output.parameters = tools[0].parameters;
      }
    },
  };
};
