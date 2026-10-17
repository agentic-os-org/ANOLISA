import { join } from "node:path";
import { homedir } from "node:os";
import type { OpenClawPluginApi, SessionContext } from "../openclaw-bridge.js";
import type { SelectiveClawConfig } from "../types.js";
import { DEFAULT_CONFIG } from "../types.js";
import { createConnection } from "../db/connection.js";
import { SelectiveContextEngine } from "../engine.js";
import { createSummarizer } from "../summarize.js";
import { emptySearchResult, executeSearchTurns } from "../search-tool.js";

function resolveConfig(api: OpenClawPluginApi): SelectiveClawConfig {
  const pluginConfig = api.pluginConfig ?? {};
  const stateDir = process.env.OPENCLAW_STATE_DIR?.trim() || join(homedir(), ".openclaw");
  const dbPath = pluginConfig.dbPath ?? join(stateDir, "selective-claw.db");

  return {
    enabled: pluginConfig.enabled ?? DEFAULT_CONFIG.enabled,
    freshTailTurns: pluginConfig.freshTailTurns ?? DEFAULT_CONFIG.freshTailTurns,
    dbPath: dbPath.startsWith("~/") ? join(homedir(), dbPath.slice(2)) : dbPath,
  };
}

function getRuntimeLlmComplete(api: OpenClawPluginApi) {
  const runtime = api.runtime as any;
  return typeof runtime?.llm?.complete === "function"
    ? runtime.llm.complete
    : undefined;
}

function createExpandTurnTool(engine: SelectiveContextEngine, boundSessionId: string | null) {
  return {
    name: "expand_turn",
    label: "Expand Turn",
    description:
      "Expand summarized conversation turns to see their full original messages. " +
      "Use when the [summary] block mentions a turn whose details you need. " +
      "Pass the turn numbers from the summary to retrieve the complete content.",
    parameters: {
      type: "object",
      required: ["turn_ids"],
      properties: {
        turn_ids: {
          type: "array",
          items: { type: "integer" },
          description: "Turn numbers to expand (from the [summary] block)",
        },
      },
    },
    async execute(_toolCallId: string, params: any) {
      const turnIds = Array.isArray(params?.turn_ids) ? params.turn_ids : [];
      if (boundSessionId === null || turnIds.length === 0) {
        const result = boundSessionId === null
          ? { status: "session_unresolved", found: 0, turns: [] }
          : { found: 0, turns: [] };
        return {
          content: [{ type: "text" as const, text: JSON.stringify(result) }],
          details: result,
        };
      }
      const result = engine.expandTurns(boundSessionId, turnIds);
      return {
        content: [{ type: "text" as const, text: JSON.stringify(result) }],
        details: result,
      };
    },
  };
}

function createSearchTurnTool(
  engine: SelectiveContextEngine,
  boundSessionId: string | null,
  onError: (error: unknown) => void,
) {
  return {
    name: "search_turns",
    label: "Search Turns",
    description:
      "Search all stored messages in this conversation, including recent unsummarized messages. " +
      "All keywords must match the same message; quoted phrases also require adjacency. " +
      "Results count messages, so several may belong to one turn. Follow nextOffset when truncated, " +
      "then pass relevant turn numbers to expand_turn for full messages.",
    parameters: {
      type: "object",
      required: ["query"],
      properties: {
        query: { type: "string", minLength: 1, description: "Keywords or a quoted phrase to find" },
        limit: { type: "integer", minimum: 1, maximum: 20, default: 5, description: "Maximum matching messages" },
        offset: { type: "integer", minimum: 0, default: 0, description: "Number of matching messages to skip; use nextOffset for the next page" },
      },
    },
    async execute(_toolCallId: string, params: { query?: unknown; limit?: unknown; offset?: unknown }) {
      let result = emptySearchResult("session_unresolved");
      if (boundSessionId !== null) {
        try {
          result = executeSearchTurns(engine.getStore(), boundSessionId, params?.query, params?.limit, params?.offset);
        } catch (error) {
          onError(error);
          result = emptySearchResult("search_unavailable");
        }
      }
      return {
        content: [{ type: "text" as const, text: JSON.stringify(result) }],
        details: result,
      };
    },
  };
}

export default function activate(api: OpenClawPluginApi): void {
  const config = resolveConfig(api);

  if (!config.enabled) {
    return;
  }

  const db = createConnection(config.dbPath);
  const engine = new SelectiveContextEngine(db, config);

  const runtimeLlmComplete = getRuntimeLlmComplete(api);
  if (runtimeLlmComplete) {
    engine.setSummarizeFn(createSummarizer(runtimeLlmComplete));
  } else {
    console.warn("[selective-claw] runtime.llm.complete not available, summaries will use fallback");
  }

  api.registerContextEngine("selective-claw", () => engine);

  if (typeof api.registerTool === "function") {
    (api.registerTool as any)(
      (ctx: SessionContext) => createExpandTurnTool(engine, engine.resolveSessionId(ctx)),
      { name: "expand_turn" },
    );
    (api.registerTool as any)(
      (ctx: SessionContext) => createSearchTurnTool(engine, engine.resolveSessionId(ctx), (error) => {
        const message = `[selective-claw] search unavailable: ${error instanceof Error ? error.message : "unknown database error"}`;
        if (api.logger?.warn) api.logger.warn(message);
        else console.warn(message);
      }),
      { name: "search_turns" },
    );
  }
}
