import { join } from "node:path";
import { homedir } from "node:os";
import type { OpenClawPluginApi } from "../openclaw-bridge.js";
import type { SelectiveClawConfig } from "../types.js";
import { DEFAULT_CONFIG } from "../types.js";
import { createConnection } from "../db/connection.js";
import { SelectiveContextEngine } from "../engine.js";
import { createSummarizer } from "../summarize.js";

function resolveConfig(api: OpenClawPluginApi): SelectiveClawConfig {
  const pluginConfig = api.config ?? {};
  const stateDir = process.env.OPENCLAW_STATE_DIR?.trim() || join(homedir(), ".openclaw");

  return {
    enabled: pluginConfig.enabled ?? DEFAULT_CONFIG.enabled,
    freshTailTurns: pluginConfig.freshTailTurns ?? DEFAULT_CONFIG.freshTailTurns,
    dbPath: pluginConfig.dbPath ?? join(stateDir, "selective-claw.db"),
  };
}

function getRuntimeLlmComplete(api: OpenClawPluginApi) {
  const runtime = api.runtime as any;
  return typeof runtime?.llm?.complete === "function"
    ? runtime.llm.complete
    : undefined;
}

function createExpandTurnTool(engine: SelectiveContextEngine) {
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
      const sessionId = engine.getActiveSessionId();
      if (sessionId === null || turnIds.length === 0) {
        return {
          content: [{ type: "text" as const, text: JSON.stringify({ found: 0, turns: [] }) }],
          details: { found: 0, turns: [] },
        };
      }
      const result = engine.expandTurns(sessionId, turnIds);
      return {
        content: [{ type: "text" as const, text: JSON.stringify(result, null, 2) }],
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
      (_ctx: any) => createExpandTurnTool(engine),
      { name: "expand_turn" },
    );
  }
}
