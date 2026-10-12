/**
 * Tokenless lifecycle adapter for OpenClaw.
 *
 * OpenClaw can replace arguments before a tool call and rewrite tool results before the model
 * consumes them. Core owns RTK execution and PostTool policy;
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
const USER_HOME =
  process.env.HOME && isAbsolute(process.env.HOME) ? process.env.HOME : null;
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

interface CallState {
  optimization: "none" | "rtk";
  /** Shell command line behind the result, for Core's file_read classification. */
  command?: string;
  createdAt: number;
}

interface ToolCategories {
  layer_1_skip: { tools: string[] };
  layer_2_shell: { tools: string[] };
}

interface ToolResult {
  content: unknown[];
  details?: unknown;
  [key: string]: unknown;
}

interface ToolResultEvent {
  toolName: string;
  toolCallId: string;
  args: Record<string, unknown>;
  result: ToolResult;
  isError?: boolean;
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

const FALLBACK_FILE_TOOLS = [
  "Read",
  "read",
  "read_file",
  "read_many_files",
  "Glob",
  "glob",
  "search_file",
  "list_directory",
  "list_dir",
  "Grep",
  "grep",
  "grep_code",
  "grep_search",
  "search_files",
  "Lsp",
  "lsp",
  "NotebookRead",
  "notebook_read",
  "notebookread",
];

const FALLBACK_SHELL_TOOLS = [
  "Bash",
  "bash",
  "Shell",
  "shell",
  "exec",
  "terminal",
  "run_shell_command",
  "run_in_terminal",
  "get_terminal_output",
  "execute_command",
  "process",
];

function binaryIn(directory: string | null, name: string): string {
  return directory ? join(directory, name) : "";
}

function isExecutable(path: string): boolean {
  try {
    const stat = statSync(path);
    return stat.isFile() && (stat.mode & 0o111) !== 0;
  } catch {
    return false;
  }
}

function resolveBinaryPath(
  name: string,
  ...fallbacks: string[]
): string | null {
  for (const directory of (process.env.PATH || "").split(delimiter)) {
    if (!directory) continue;
    const candidate = join(directory, name);
    if (isExecutable(candidate)) return candidate;
  }
  return fallbacks.find((path) => path && isExecutable(path)) ?? null;
}

function checkTokenless(): boolean {
  if (
    tokenlessAvailable !== null &&
    tokenlessCheckedAt !== null &&
    Date.now() - tokenlessCheckedAt > CACHE_TTL_MS
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
    join(
      import.meta.dirname,
      "..",
      "..",
      "common",
      "hooks",
      "tool_categories.json",
    ),
    join(import.meta.dirname, "common", "hooks", "tool_categories.json"),
    "/usr/share/anolisa/adapters/tokenless/common/hooks/tool_categories.json",
    "/usr/local/share/anolisa/adapters/tokenless/common/hooks/tool_categories.json",
  ];

  try {
    const path = possiblePaths.find((candidate) => existsSync(candidate));
    if (!path) return fallback;
    const parsed = JSON.parse(
      readFileSync(path, "utf-8"),
    ) as Partial<ToolCategories>;
    if (
      !Array.isArray(parsed.layer_1_skip?.tools) ||
      !Array.isArray(parsed.layer_2_shell?.tools)
    ) {
      throw new Error("tool category lists are missing");
    }
    return parsed as ToolCategories;
  } catch (error) {
    console.warn(
      `[tokenless] Failed to load tool categories: ${String(error)}`,
    );
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
      response.protocol_version !== 2 ||
      response.operation !== operation ||
      typeof response.result !== "object" ||
      response.result === null ||
      Array.isArray(response.result)
    ) {
      return null;
    }
    return response.result as Record<string, unknown>;
  } catch {
    return null;
  }
}

function tryEnvCheck(
  toolName: string,
): { status: string; diagnostic: string } | null {
  try {
    const result = execFileSync(
      tokenlessPath,
      ["env-check", "--tool", toolName, "--json"],
      {
        encoding: "utf-8",
        timeout: 3_000,
        env: process.env,
      },
    ).trim();
    const parsed = JSON.parse(result) as { status?: string };
    const status = parsed.status || "UNKNOWN";
    if (status === "UNKNOWN" || status === "READY") return null;

    const fixResult = execFileSync(
      tokenlessPath,
      ["env-check", "--tool", toolName, "--fix", "--json"],
      { encoding: "utf-8", timeout: 10_000, env: process.env },
    ).trim();
    const fixed = JSON.parse(fixResult) as {
      status?: string;
      diagnostic?: string;
    };
    if (fixed.status === "READY") return null;
    return {
      status: fixed.status || "NOT_READY",
      diagnostic:
        fixed.diagnostic ||
        `[tokenless:ready] ${toolName}: NOT_READY. Skip retry.`,
    };
  } catch {
    return null;
  }
}

function singleTextBlock(result: unknown): TextBlock | null {
  if (typeof result !== "object" || result === null || Array.isArray(result))
    return null;
  const content = (result as ToolResult).content;
  if (!Array.isArray(content) || content.length !== 1) return null;
  const block = content[0];
  if (
    typeof block !== "object" ||
    block === null ||
    (block as TextBlock).type !== "text" ||
    typeof (block as TextBlock).text !== "string"
  )
    return null;
  return block as TextBlock;
}

function toolFailed(event: ToolResultEvent): boolean {
  if (event.isError === true) return true;
  const details = event.result.details;
  if (typeof details !== "object" || details === null || Array.isArray(details))
    return false;
  const metadata = details as Record<string, unknown>;
  // Middleware runs before the host folds structured failure metadata into isError.
  if (
    metadata.ok === false ||
    metadata.success === false ||
    metadata.timedOut === true ||
    metadata.error
  )
    return true;
  if (
    typeof metadata.exitCode === "number" &&
    Number.isFinite(metadata.exitCode) &&
    metadata.exitCode !== 0
  )
    return true;
  if (metadata.ok === true || metadata.success === true) return false;
  const status =
    typeof metadata.status === "string"
      ? metadata.status.trim().toLowerCase()
      : "";
  return [
    "error",
    "failed",
    "failure",
    "timeout",
    "timed_out",
    "blocked",
    "denied",
    "forbidden",
    "unavailable",
    "approval-unavailable",
    "disabled",
    "aborted",
    "cancelled",
    "canceled",
    "killed",
    "invalid",
  ].includes(status);
}

export default {
  id: "tokenless",
  name: "Tokenless",
  version: "1.0.0",
  description:
    "Protocol v2 RTK rewriting and PostTool optimization for OpenClaw",
  register(api: any) {
    const pluginConfig = api.pluginConfig ?? {};
    const rtkEnabled = pluginConfig.rtk_enabled !== false;
    const postToolEnabled = pluginConfig.post_tool_enabled !== false;
    const warn = (message: string): void => {
      try {
        if (typeof api.logger?.warn === "function") api.logger.warn(message);
        else console.warn(message);
      } catch {
        // Logging must not turn a failed compression into a failed tool call.
      }
    };
    const toolReadyEnabled = pluginConfig.tool_ready_enabled !== false;
    const verbose = pluginConfig.verbose === true;
    const available = checkTokenless();
    let postToolActive = postToolEnabled && available;
    if (
      postToolActive &&
      typeof api.registerAgentToolResultMiddleware !== "function"
    ) {
      warn(
        "[tokenless] PostTool disabled: OpenClaw 2026.6.6 or newer with tool-result middleware is required.",
      );
      postToolActive = false;
    }

    const sessionMap = new Map<string, string>();
    const callStates = new Map<string, CallState>();
    const categories = loadToolCategories();
    const fileTools = new Set(
      categories.layer_1_skip.tools.map((tool) => tool.toLowerCase()),
    );
    const shellTools = new Set(
      categories.layer_2_shell.tools.map((tool) => tool.toLowerCase()),
    );

    const sessionIdFor = (ctx: HookContext): string => {
      if (ctx.sessionId) {
        if (ctx.sessionKey) sessionMap.set(ctx.sessionKey, ctx.sessionId);
        return ctx.sessionId;
      }
      return (
        (ctx.sessionKey && sessionMap.get(ctx.sessionKey)) ||
        ctx.sessionKey ||
        ""
      );
    };
    const stateKey = (context: CallContext): string =>
      `${context.sessionId}\0${context.toolCallId}`;
    const pruneStates = (): void => {
      const cutoff = Date.now() - OPTIMIZATION_STATE_TTL_MS;
      for (const [key, state] of callStates) {
        if (state.createdAt <= cutoff) callStates.delete(key);
      }
      while (callStates.size >= OPTIMIZATION_STATE_MAX_ENTRIES) {
        const oldest = callStates.keys().next().value as string;
        callStates.delete(oldest);
      }
    };
    const rememberState = (
      context: CallContext,
      patch: Partial<CallState>,
    ): void => {
      pruneStates();
      const key = stateKey(context);
      const previous = callStates.get(key);
      callStates.delete(key);
      callStates.set(key, {
        optimization: "none",
        ...previous,
        ...patch,
        createdAt: Date.now(),
      });
    };
    const consumeState = (context: CallContext): CallState | undefined => {
      if (!context.toolCallId) return undefined;
      const key = stateKey(context);
      const state = callStates.get(key);
      callStates.delete(key);
      return state;
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
        const sessionId =
          event.sessionId ||
          (event.sessionKey && sessionMap.get(event.sessionKey)) ||
          event.sessionKey ||
          "";
        if (event.sessionKey) sessionMap.delete(event.sessionKey);
        for (const key of callStates.keys()) {
          if (key.startsWith(`${sessionId}\0`)) callStates.delete(key);
        }
      },
    );

    if (toolReadyEnabled && available) {
      api.on(
        "before_tool_call",
        (event: { toolName: string }) => {
          const result = tryEnvCheck(event.toolName);
          if (!result) return;
          if (verbose)
            console.log(
              `[tokenless:ready] ${event.toolName}: ${result.status}`,
            );
          return { contextPrefix: result.diagnostic };
        },
        { priority: 5 },
      );
    }

    if ((rtkEnabled || postToolActive) && available) {
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
          const context: CallContext = {
            sessionId: sessionIdFor(ctx),
            toolCallId: event.toolCallId || ctx.toolCallId || "",
          };
          const command = event.params?.command;
          if (!context.toolCallId || typeof command !== "string") return;
          // Core reports a plain file print (`cat page.html`) as file_read from
          // the command line, so the page stays verbatim while data still compresses.
          if (postToolActive && shellTools.has(event.toolName.toLowerCase())) {
            rememberState(context, { command });
          }
          if (!rtkEnabled || event.toolName !== "exec") return;

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
            result?.action !== "replace_arguments" ||
            result.output_optimization !== "rtk" ||
            typeof result.arguments !== "object" ||
            result.arguments === null ||
            Array.isArray(result.arguments)
          ) {
            return;
          }
          const argumentsResult = result.arguments as Record<string, unknown>;
          if (
            typeof argumentsResult.command !== "string" ||
            argumentsResult.command === event.params.command
          ) {
            return;
          }

          if (postToolActive) {
            rememberState(context, {
              optimization: "rtk",
              command: argumentsResult.command,
            });
          }
          if (verbose) console.log(`[tokenless:rtk] rewrote ${event.toolName}`);
          return { params: argumentsResult };
        },
        { priority: 10 },
      );
    }

    if (postToolActive) {
      try {
        api.registerAgentToolResultMiddleware(
          (event: ToolResultEvent, ctx: HookContext) => {
            try {
              if (
                typeof event?.toolName !== "string" ||
                !event.toolName ||
                typeof event.toolCallId !== "string" ||
                !event.toolCallId
              )
                return;
              const context: CallContext = {
                sessionId: sessionIdFor(ctx),
                toolCallId: event.toolCallId,
              };
              const state = consumeState(context);
              const block = singleTextBlock(event.result);
              if (block === null) return;
              const toolName = event.toolName;
              const normalizedToolName = toolName.toLowerCase();
              const contentOrigin = fileTools.has(normalizedToolName)
                ? "file_content"
                : shellTools.has(normalizedToolName)
                  ? "command_output"
                  : "api_response";
              const input: Record<string, unknown> = {
                result_kind: "tool",
                tool_name: toolName,
                content: block.text,
                status: toolFailed(event) ? "error" : "success",
                content_origin: contentOrigin,
                output_optimization: state?.optimization ?? "none",
                capabilities: {
                  replace_output: true,
                  // Operator retrieval does not enforce Agent Marker visibility.
                  recovery: { kind: "none" },
                  replace_with_text: true,
                },
              };
              if (contentOrigin === "command_output") {
                const command =
                  typeof event.args?.command === "string"
                    ? event.args.command
                    : state?.command;
                if (command !== undefined) input.command = command;
              }
              const result = runOperation("post_tool", input, context);
              if (result === null) return;
              if (result.disposition === "tool_error") {
                if (
                  typeof result.additional_context !== "string" ||
                  !result.additional_context
                )
                  return;
                return {
                  result: {
                    ...event.result,
                    content: [
                      block,
                      { type: "text", text: result.additional_context },
                    ],
                  },
                };
              }
              if (
                result.disposition !== "applied" ||
                typeof result.output !== "string"
              )
                return;
              if (verbose)
                console.log(`[tokenless:post-tool] optimized ${toolName}`);
              return {
                result: {
                  ...event.result,
                  content: [{ ...block, text: result.output }],
                },
              };
            } catch {
              // The host turns thrown middleware errors into failed tool results.
              return;
            }
          },
          { runtimes: ["openclaw"] },
        );
      } catch {
        postToolActive = false;
        warn(
          "[tokenless] PostTool disabled: OpenClaw rejected tool-result middleware registration.",
        );
      }
    }

    if (verbose) {
      const features = [
        rtkEnabled && available ? "pre-tool" : null,
        postToolActive ? "post-tool" : null,
      ].filter(Boolean);
      console.log(
        `[tokenless] OpenClaw plugin registered — active features: ${features.join(", ") || "none"}`,
      );
    }
  },
};
