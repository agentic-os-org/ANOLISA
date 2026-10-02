/**
 * Configuration management for the ws-ckpt plugin.
 *
 * Handles loading configuration from user-provided values and environment
 * variables, merging with sensible defaults, and validating the result.
 */

import type { PluginConfig } from "./types.js";
import { validateCronExpr } from "./cron.js";

/**
 * Outcome of extracting per-ws effective auto-cleanup state from a
 * `ws-ckpt config --format json` payload. Keeps the three "no value" cases
 * distinct so handlers can react differently:
 *
 *   - `parse-error`     → stdout didn't match the expected schema. MUST NOT
 *                         be treated as "disabled" — that was the orig bug.
 *   - `disabled`        → effective policy is genuinely off (the CLI
 *                         pre-computes `is_disabled` on the wire).
 *   - `count` | `age`   → real cleanup with the carried value.
 */
export type WorkspaceCleanupParsed =
  | { kind: "parse-error"; reason: string }
  | { kind: "disabled" }
  | { kind: "count"; num: number }
  | { kind: "age"; duration: string };

/**
 * Parse a `ws-ckpt config -w <ws> --format json` stdout payload.
 *
 * The JSON shape is versioned (`schema: "ws-ckpt-policy/v1"`); a mismatch
 * is reported as `parse-error` so a future daemon bump doesn't silently
 * get reinterpreted as "auto-cleanup disabled" by an old plugin.
 */
export function parseWorkspaceCleanupJson(stdout: string): WorkspaceCleanupParsed {
  let doc: unknown;
  try {
    doc = JSON.parse(stdout);
  } catch (e) {
    return {
      kind: "parse-error",
      reason: `not valid JSON: ${e instanceof Error ? e.message : String(e)}`,
    };
  }
  if (!doc || typeof doc !== "object") {
    return { kind: "parse-error", reason: "JSON root is not an object" };
  }
  const root = doc as Record<string, unknown>;
  if (root.schema !== "ws-ckpt-policy/v1") {
    return {
      kind: "parse-error",
      reason: `unknown schema: ${JSON.stringify(root.schema)} (expected "ws-ckpt-policy/v1")`,
    };
  }
  const eff = root.effective as Record<string, unknown> | undefined;
  if (!eff || typeof eff !== "object") {
    return { kind: "parse-error", reason: "missing or invalid `effective`" };
  }
  // Trust the daemon's pre-computed is_disabled (covers auto_cleanup=false
  // AND Count(0)/Age{secs:0}); re-deriving it was the old parser's bug.
  if (eff.is_disabled === true) return { kind: "disabled" };
  const keep = eff.auto_cleanup_keep as Record<string, unknown> | undefined;
  if (!keep || typeof keep !== "object") {
    return {
      kind: "parse-error",
      reason: "missing or invalid `effective.auto_cleanup_keep`",
    };
  }
  if (keep.mode === "count") {
    // `typeof === "number"` alone admits NaN, Infinity, and floats. Match
    // hermes's int() and the daemon's u32 contract: require a finite,
    // non-negative integer.
    if (
      typeof keep.count !== "number" ||
      !Number.isInteger(keep.count) ||
      keep.count < 0
    ) {
      return {
        kind: "parse-error",
        reason: "`count` field must be a non-negative integer",
      };
    }
    return { kind: "count", num: keep.count };
  }
  if (keep.mode === "age") {
    if (typeof keep.raw !== "string") {
      return { kind: "parse-error", reason: "`raw` field is not a string" };
    }
    return { kind: "age", duration: keep.raw };
  }
  return {
    kind: "parse-error",
    reason: `unknown auto_cleanup_keep.mode: ${JSON.stringify(keep.mode)}`,
  };
}

/** Default configuration values. */
export const DEFAULT_CONFIG: PluginConfig = {
  workspace: `${process.env.HOME ?? "/root"}/.openclaw/workspace`,
  autoCheckpoint: false,
  cronSchedules: [],
};

// Intentionally no module-level workspaceCleanup cache.
//
// view path queries daemon every call, so a register-time prefetch would
// be wasted work; falling back to a stale cache when daemon is unreachable
// would hand the user/LLM potentially-hours-old values without any "as of
// when" annotation — worse than a loud "daemon unreachable" error.
// Matches the hermes plugin (no cache either) and eliminates by
// construction the cross-call RMW race that the cache used to risk.

/**
 * Configuration manager for the ws-ckpt plugin.
 *
 * Loads configuration from the plugin's user-provided config and validates
 * the result. Configuration sources, in priority order:
 *   1. user config (from openclaw.json `plugins.entries.ws-ckpt.config`)
 *   2. DEFAULT_CONFIG
 */
export class PluginConfigManager {
  private config: PluginConfig;

  /**
   * Create a new PluginConfigManager.
   *
   * @param persistedConfig - Overrides from ~/.openclaw/ws-ckpt.json.
   */
  constructor(persistedConfig: Partial<PluginConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...persistedConfig };
    if (Array.isArray(this.config.cronSchedules)) {
      const valid = this.config.cronSchedules.filter((e) => typeof e === "string" && validateCronExpr(e));
      const skipped = this.config.cronSchedules.filter((e) => typeof e === "string" && !validateCronExpr(e));
      if (skipped.length > 0) {
        console.warn(`[ws-ckpt] Ignoring invalid cron expression(s): ${JSON.stringify(skipped)}`);
      }
      this.config.cronSchedules = valid;
    }
  }

  /** Return the resolved configuration. */
  public getConfig(): PluginConfig {
    return { ...this.config };
  }

  /**
   * Validate the current configuration.
   *
   * @returns An object with `valid` flag and any `errors` found.
   */
  public validate(): { valid: boolean; errors: string[] } {
    const errors: string[] = [];
    return { valid: errors.length === 0, errors };
  }
}