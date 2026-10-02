/**
 * Minimal type shim for OpenClaw plugin SDK contracts.
 *
 * These types mirror the relevant parts of the OpenClaw plugin API so that
 * this plugin can be type-checked without depending on the openclaw package
 * at build time. When openclaw is installed at runtime, the real
 * implementations are resolved via the jiti alias.
 */

// ---------------------------------------------------------------------------
// Hook event types
// ---------------------------------------------------------------------------

/** Event payload received by the message_received hook. */
export type PluginHookMessageReceivedEvent = {
  from: string;
  content: string;
  timestamp?: number;
  metadata?: Record<string, unknown>;
};

// ---------------------------------------------------------------------------
// Tool types
// ---------------------------------------------------------------------------

export type ToolResultContentItem = {
  type: "text" | "image" | "resource";
  text?: string;
  url?: string;
  mimeType?: string;
};

export type AgentToolResult<T = Record<string, unknown>> = {
  content: ToolResultContentItem[];
  details?: T;
};

export type AnyAgentTool = {
  name: string;
  description: string;
  parameters?: Record<string, unknown>;
  execute(toolCallId: string, params: Record<string, unknown>): Promise<AgentToolResult>;
};

export type OpenClawPluginToolOptions = {
  name?: string;
  names?: string[];
  optional?: boolean;
};

/** Plugin logger interface. */
export type PluginLogger = {
  info: (...args: unknown[]) => void;
  warn: (...args: unknown[]) => void;
  error: (...args: unknown[]) => void;
  debug: (...args: unknown[]) => void;
};

// ---------------------------------------------------------------------------
// Plugin API
// ---------------------------------------------------------------------------

/** The OpenClaw Plugin API surface exposed to plugin register functions. */
export type OpenClawPluginApi = {
  id: string;
  name: string;
  version?: string;
  description?: string;
  source: string;
  rootDir?: string;
  registrationMode: string;
  config: Record<string, unknown>;
  pluginConfig?: Record<string, unknown>;
  logger: PluginLogger;
  runtime: {
    agent: {
      resolveAgentWorkspaceDir: (config: Record<string, unknown>, agentId: string) => string;
      [key: string]: unknown;
    };
    [key: string]: unknown;
  };
  registerTool: (tool: AnyAgentTool | ((...args: unknown[]) => AnyAgentTool), opts?: OpenClawPluginToolOptions) => void;
  registerHook(event: string, handler: (...args: unknown[]) => unknown, opts?: { priority?: number; name?: string; description?: string }): void;
  resolvePath: (input: string) => string;
  on: <K extends string>(hookName: K, handler: (...args: unknown[]) => unknown, opts?: { priority?: number }) => void;
};

// ---------------------------------------------------------------------------
// Plugin entry helper (mirrors definePluginEntry from plugin-sdk)
// ---------------------------------------------------------------------------

export type PluginKind =
  | "tool"
  | "memory"
  | "context-engine"
  | "provider"
  | "channel"
  | "service"
  | "compaction";

export type PluginEntryOptions = {
  id: string;
  name: string;
  description?: string;
  kind?: PluginKind;
  register: (api: OpenClawPluginApi) => void;
};

/** Minimal no-op shim so the module resolves when openclaw is not installed. */
export function definePluginEntry(opts: PluginEntryOptions): PluginEntryOptions {
  return opts;
}
