/**
 * Tokenless lifecycle adapter for OpenClaw.
 *
 * OpenClaw can replace arguments before a tool call and synchronously rewrite
 * tool-result transcript entries. Core owns RTK execution and PostTool policy;
 * this adapter only translates those host events to Protocol v2.
 */

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { delimiter, isAbsolute, join } from "node:path";

const CACHE_TTL_MS = 5 * 60 * 1000;
const OPERATION_TIMEOUT_MS = 8_000;
const OPTIMIZATION_STATE_TTL_MS = 24 * 60 * 60 * 1000;
const OPTIMIZATION_STATE_MAX_ENTRIES = 1_024;

let tokenlessAvailable: boolean | null = null;
let tokenlessCheckedAt: number | null = null;
let tokenlessPath = "tokenless";

const TOKENLESS_FALLBACK = "/usr/bin/tokenless";
const SYSTEM_BIN = "/usr/local/bin";
const USER_HOME = process.env.HOME && isAbsolute(process.env.HOME) ? process.env.HOME : null;
const LOCAL_BIN = USER_HOME ? join(USER_HOME, ".local", "bin") : null;
const LOCAL_LIB = USER_HOME
  ? join(USER_HOME, ".local", "lib", "anolisa", "tokenless")
  : null;
const LOCAL_FALLBACK = USER_HOME
  ? join(USER_HOME, ".local", "share", "anolisa", "tokenless")
  : null;

interface CallContext {
  sessionId: string;
  toolCallId: string;
}

interface OptimizationState {
  optimization: "rtk";
  createdAt: number;
}

interface ToolCategories {
  layer_1_skip: { tools: string[] };
  layer_2_shell: { tools: string[] };
}

interface ToolResultEvent {
  toolName?: string;
  toolCallId?: string;
  message: unknown;
  isSynthetic?: boolean;
}

interface HookContext {
  agentId?: string;
  sessionId?: string;
  sessionKey?: string;
  toolName?: string;
  toolCallId?: string;
  runId?: string;
}

interface TextBlock {
  type: "text";
  text: string;
  [key: string]: unknown;
}

type ContentSlot =
  | { kind: "string"; content: string; replaceWithText: true }
  | {
    kind: "tool_text";
    content: string;
    replaceWithText: true;
    message: Record<string, unknown>;
    block: TextBlock;
  }
  | {
    kind: "structured";
    content: string;
    replaceWithText: false;
    message: Record<string, unknown> | unknown[];
  };

const FALLBACK_FILE_TOOLS = [
  "Read", "read", "read_file", "read_many_files",
  "Glob", "glob", "search_file", "list_directory", "list_dir",
  "Grep", "grep", "grep_code", "grep_search", "search_files",
  "Lsp", "lsp", "NotebookRead", "notebook_read", "notebookread",
];

const FALLBACK_SHELL_TOOLS = [
  "Bash", "bash", "Shell", "shell", "exec", "terminal",
  "run_shell_command", "run_in_terminal", "get_terminal_output",
  "execute_command", "process",
];

function binaryIn(directory: string | null, name: string): string {
  return directory ? join(directory, name) : "";
}

function isExecutable(path: string): boolean {
  try {
    return existsSync(path) && (statSync(path).mode & 0o111) !== 0;
  } catch {
    return false;
  }
}

function resolveBinaryPath(name: string, ...fallbacks: string[]): string | null {
  for (const directory of (process.env.PATH || "").split(delimiter)) {
    if (!directory) continue;
    const candidate = join(directory, name);
    if (isExecutable(candidate)) return candidate;
  }
  return fallbacks.find((path) => path && isExecutable(path)) ?? null;
}

function checkTokenless(): boolean {
  if (
    tokenlessAvailable !== null
    && tokenlessCheckedAt !== null
    && Date.now() - tokenlessCheckedAt > CACHE_TTL_MS
  ) {
    tokenlessAvailable = null;
  }
  if (tokenlessAvailable !== null) return tokenlessAvailable;

  const resolved = resolveBinaryPath(
    "tokenless",
    binaryIn(LOCAL_BIN, "tokenless"),
    join(SYSTEM_BIN, "tokenless"),
    TOKENLESS_FALLBACK,
    binaryIn(LOCAL_FALLBACK, "tokenless"),
    binaryIn(LOCAL_LIB, "tokenless"),
  );
  tokenlessAvailable = resolved !== null;
  if (resolved !== null) tokenlessPath = resolved;
  tokenlessCheckedAt = Date.now();
  return tokenlessAvailable;
}

function loadToolCategories(): ToolCategories {
  const fallback: ToolCategories = {
    layer_1_skip: { tools: FALLBACK_FILE_TOOLS },
    layer_2_shell: { tools: FALLBACK_SHELL_TOOLS },
  };
  const possiblePaths = [
    join(import.meta.dirname, "tool_categories.json"),
    join(import.meta.dirname, "..", "..", "common", "hooks", "tool_categories.json"),
    join(import.meta.dirname, "common", "hooks", "tool_categories.json"),
    "/usr/share/anolisa/adapters/tokenless/common/hooks/tool_categories.json",
    "/usr/local/share/anolisa/adapters/tokenless/common/hooks/tool_categories.json",
  ];

  try {
    const path = possiblePaths.find((candidate) => existsSync(candidate));
    if (!path) return fallback;
    const parsed = JSON.parse(readFileSync(path, "utf-8")) as Partial<ToolCategories>;
    if (
      !Array.isArray(parsed.layer_1_skip?.tools)
      || !Array.isArray(parsed.layer_2_shell?.tools)
    ) {
      throw new Error("tool category lists are missing");
    }
    return parsed as ToolCategories;
  } catch (error) {
    console.warn(`[tokenless] Failed to load tool categories: ${String(error)}`);
    return fallback;
  }
}

function runOperation(
  operation: "pre_tool" | "post_tool",
  input: Record<string, unknown>,
  context: CallContext,
): Record<string, unknown> | null {
  const attribution: Record<string, string> = { agent_id: "openclaw" };
  if (context.sessionId) attribution.session_id = context.sessionId;
  if (context.toolCallId) attribution.tool_use_id = context.toolCallId;

  try {
    const stdout = execFileSync(tokenlessPath, ["compress"], {
      encoding: "utf-8",
      timeout: OPERATION_TIMEOUT_MS,
      input: JSON.stringify({
        protocol_version: 2,
        operation,
        attribution,
        input,
      }),
      env: process.env,
    });
    const response = JSON.parse(stdout) as Record<string, unknown>;
    if (
      response.protocol_version !== 2
      || response.operation !== operation
      || typeof response.result !== "object"
      || response.result === null
      || Array.isArray(response.result)
    ) {
      return null;
    }
    return response.result as Record<string, unknown>;
  } catch {
    return null;
  }
}

function tryEnvCheck(toolName: string): { status: string; diagnostic: string } | null {
  try {
    const result = execFileSync(tokenlessPath, ["env-check", "--tool", toolName, "--json"], {
      encoding: "utf-8",
      timeout: 3_000,
      env: process.env,
    }).trim();
    const parsed = JSON.parse(result) as { status?: string };
    const status = parsed.status || "UNKNOWN";
    if (status === "UNKNOWN" || status === "READY") return null;

    const fixResult = execFileSync(
      tokenlessPath,
      ["env-check", "--tool", toolName, "--fix", "--json"],
      { encoding: "utf-8", timeout: 10_000, env: process.env },
    ).trim();
    const fixed = JSON.parse(fixResult) as { status?: string; diagnostic?: string };
    if (fixed.status === "READY") return null;
    return {
      status: fixed.status || "NOT_READY",
      diagnostic: fixed.diagnostic
        || `[tokenless:ready] ${toolName}: NOT_READY. Skip retry.`,
    };
  } catch {
    return null;
  }
}

function contentSlot(message: unknown): ContentSlot | null {
  if (typeof message === "string") {
    return { kind: "string", content: message, replaceWithText: true };
  }
  if (typeof message !== "object" || message === null) return null;

  const messageObject = message as Record<string, unknown>;
  if (!Array.isArray(message) && messageObject.role === "toolResult") {
    const content = messageObject.content;
    if (!Array.isArray(content) || content.length !== 1) return null;
    const block = content[0];
    if (
      typeof block !== "object"
      || block === null
      || block.type !== "text"
      || typeof block.text !== "string"
    ) {
      return null;
    }
    return {
      kind: "tool_text",
      content: block.text,
      replaceWithText: true,
      message: messageObject,
      block: block as TextBlock,
    };
  }

  return {
    kind: "structured",
    content: JSON.stringify(message),
    replaceWithText: false,
    message: message as Record<string, unknown> | unknown[],
  };
}

function applyOutput(slot: ContentSlot, output: string): unknown | null {
  if (slot.kind === "string") return output;
  if (slot.kind === "tool_text") {
    return {
      ...slot.message,
      content: [{ ...slot.block, text: output }],
    };
  }

  try {
    const parsed = JSON.parse(output) as unknown;
    if (typeof parsed !== "object" || parsed === null) return null;
    if (Array.isArray(parsed) !== Array.isArray(slot.message)) return null;
    return parsed;
  } catch {
    return null;
  }
}

function appendDiagnostic(slot: ContentSlot, diagnostic: string): unknown | null {
  if (!diagnostic) return null;
  if (slot.kind === "string") return `${slot.content}\n\n${diagnostic}`;
  if (slot.kind === "tool_text") {
    return {
      ...slot.message,
      content: [slot.block, { type: "text" as const, text: diagnostic }],
    };
  }
  return null;
}

export default {
  id: "tokenless",
  name: "Tokenless",
  version: "1.0.0",
  description: "Protocol v2 RTK rewriting and PostTool optimization for OpenClaw",
  register(api: any) {
    const pluginConfig = api.pluginConfig ?? {};
    const rtkEnabled = pluginConfig.rtk_enabled !== false;
    const postToolEnabled = pluginConfig.post_tool_enabled !== false;
    const toolReadyEnabled = pluginConfig.tool_ready_enabled !== false;
    const verbose = pluginConfig.verbose === true;
    const available = checkTokenless();

    const sessionMap = new Map<string, string>();
    const optimizationStates = new Map<string, OptimizationState>();
    const categories = loadToolCategories();
    const fileTools = new Set(categories.layer_1_skip.tools.map((tool) => tool.toLowerCase()));
    const shellTools = new Set(categories.layer_2_shell.tools.map((tool) => tool.toLowerCase()));

    const sessionIdFor = (ctx: HookContext): string => {
      if (ctx.sessionId) {
        if (ctx.sessionKey) sessionMap.set(ctx.sessionKey, ctx.sessionId);
        return ctx.sessionId;
      }
      return (ctx.sessionKey && sessionMap.get(ctx.sessionKey)) || ctx.sessionKey || "";
    };
    const stateKey = (context: CallContext): string =>
      `${context.sessionId}\0${context.toolCallId}`;
    const pruneStates = (): void => {
      const cutoff = Date.now() - OPTIMIZATION_STATE_TTL_MS;
      for (const [key, state] of optimizationStates) {
        if (state.createdAt <= cutoff) optimizationStates.delete(key);
      }
      while (optimizationStates.size >= OPTIMIZATION_STATE_MAX_ENTRIES) {
        const oldest = optimizationStates.keys().next().value as string;
        optimizationStates.delete(oldest);
      }
    };
    const markOptimized = (context: CallContext): void => {
      pruneStates();
      const key = stateKey(context);
      optimizationStates.delete(key);
      optimizationStates.set(key, { optimization: "rtk", createdAt: Date.now() });
    };
    const consumeOptimization = (context: CallContext): "none" | "rtk" => {
      if (!context.toolCallId) return "none";
      const key = stateKey(context);
      const state = optimizationStates.get(key);
      optimizationStates.delete(key);
      return state?.optimization ?? "none";
    };

    api.on(
      "session_start",
      (event: { sessionId: string; sessionKey?: string }) => {
        if (event.sessionKey && event.sessionId) {
          sessionMap.set(event.sessionKey, event.sessionId);
        }
      },
    );
    api.on(
      "session_end",
      (event: { sessionId?: string; sessionKey?: string }) => {
        const sessionId = event.sessionId
          || (event.sessionKey && sessionMap.get(event.sessionKey))
          || event.sessionKey
          || "";
        if (event.sessionKey) sessionMap.delete(event.sessionKey);
        for (const key of optimizationStates.keys()) {
          if (key.startsWith(`${sessionId}\0`)) optimizationStates.delete(key);
        }
      },
    );

    if (toolReadyEnabled && available) {
      api.on(
        "before_tool_call",
        (event: { toolName: string }) => {
          const result = tryEnvCheck(event.toolName);
          if (!result) return;
          if (verbose) console.log(`[tokenless:ready] ${event.toolName}: ${result.status}`);
          return { contextPrefix: result.diagnostic };
        },
        { priority: 5 },
      );
    }

    if ((rtkEnabled || postToolEnabled) && available) {
      api.on(
        "before_tool_call",
        (
          event: {
            toolName: string;
            params: Record<string, unknown>;
            toolCallId?: string;
          },
          ctx: HookContext,
        ) => {
          const sessionId = sessionIdFor(ctx);
          if (
            !rtkEnabled
            || event.toolName !== "exec"
            || typeof event.params?.command !== "string"
          ) {
            return;
          }
          const context: CallContext = {
            sessionId,
            toolCallId: event.toolCallId || ctx.toolCallId || "",
          };
          if (!context.toolCallId) return;

          const result = runOperation(
            "pre_tool",
            {
              tool_name: event.toolName,
              arguments: event.params,
              command_field: "command",
              capabilities: {
                replace_arguments: true,
                block_and_suggest: false,
              },
            },
            context,
          );
          if (
            result?.action !== "replace_arguments"
            || result.output_optimization !== "rtk"
            || typeof result.arguments !== "object"
            || result.arguments === null
            || Array.isArray(result.arguments)
          ) {
            return;
          }
          const argumentsResult = result.arguments as Record<string, unknown>;
          if (
            typeof argumentsResult.command !== "string"
            || argumentsResult.command === event.params.command
          ) {
            return;
          }

          if (postToolEnabled) markOptimized(context);
          if (verbose) console.log(`[tokenless:rtk] rewrote ${event.toolName}`);
          return { params: argumentsResult };
        },
        { priority: 10 },
      );
    }

    if (postToolEnabled && available) {
      api.on(
        "tool_result_persist",
        (event: ToolResultEvent, ctx: HookContext) => {
          const context: CallContext = {
            sessionId: sessionIdFor(ctx),
            toolCallId: ctx.toolCallId || event.toolCallId || "",
          };
          const outputOptimization = consumeOptimization(context);
          if (event.isSynthetic) return;

          const slot = contentSlot(event.message);
          if (slot === null) return;
          const toolName = event.toolName || ctx.toolName || "";
          const normalizedToolName = toolName.toLowerCase();
          const contentOrigin = fileTools.has(normalizedToolName)
            ? "file_content"
            : shellTools.has(normalizedToolName)
              ? "command_output"
              : "api_response";
          const isError = slot.kind === "tool_text" && slot.message.isError === true;

          const result = runOperation(
            "post_tool",
            {
              result_kind: "tool",
              tool_name: toolName,
              content: slot.content,
              status: isError ? "error" : "success",
              content_origin: contentOrigin,
              output_optimization: outputOptimization,
              capabilities: {
                replace_output: true,
                // The trusted operator CLI does not enforce Agent Marker visibility.
                recovery: { kind: 'none' },
                replace_with_text: slot.replaceWithText,
              },
            },
            context,
          );
          if (result === null) return;

          if (result.disposition === "tool_error") {
            const diagnostic = typeof result.additional_context === "string"
              ? result.additional_context
              : "";
            const message = appendDiagnostic(slot, diagnostic);
            return message === null ? undefined : { message };
          }
          if (result.disposition !== "applied" || typeof result.output !== "string") return;

          const message = applyOutput(slot, result.output);
          if (message === null) return;
          if (verbose) console.log(`[tokenless:post-tool] optimized ${toolName}`);
          return { message };
        },
        { priority: 10 },
      );
    }

    if (verbose) {
      const features = [
        rtkEnabled && available ? "pre-tool" : null,
        postToolEnabled && available ? "post-tool" : null,
      ].filter(Boolean);
      console.log(
        `[tokenless] OpenClaw plugin registered — active features: ${features.join(", ") || "none"}`,
      );
    }
  },
};
