/**
 * Tool handler functions for the ws-ckpt OpenClaw plugin.
 *
 * Each handle* function implements the business logic for one tool.
 * They access shared state via the pluginState singleton.
 */

import { CommandExecutor, extractTiming } from "./commands.js";
import { mapErrorToLLMMessage } from "./btrfs-manager.js";
import type { AgentToolResult } from "../types-shim.js";
import { pluginState, UNAVAILABLE_MSG, cwdInsideWorkspace, cwdInsideWorkspaceReason } from "./state.js";
import { parseWorkspaceCleanupJson } from "./config.js";
import { persistConfig } from "./persist.js";
import { CrontabManager, parseSchedulesUpdate } from "./cron.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

export function textToolResult(
  text: string,
  isError?: boolean,
): AgentToolResult {
  return {
    content: [{ type: "text", text }],
    details: isError ? { status: "failed" } : undefined,
  };
}

// ---------------------------------------------------------------------------
// Handler functions
// ---------------------------------------------------------------------------

export async function handleCheckpoint(
  argsStr?: string,
): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  // pin is no longer exposed to plugin users (auto-cleanup disabled).
  const args = argsStr ? JSON.parse(argsStr) : {};
  const id = args.id;
  if (!id) {
    return { text: "Missing required parameter: id", isError: true };
  }
  const message = args.message?.trim() || "manual checkpoint";
  const explicitWs = (args.workspace as string | undefined)?.trim();

  // Explicit workspace bypasses the manager (and its workspace-bound cache),
  // mirroring the handleDelete pattern.
  if (explicitWs) {
    const cwdCheck = cwdInsideWorkspace(explicitWs);
    if (cwdCheck.inside) {
      return { text: cwdInsideWorkspaceReason(cwdCheck.cwd, explicitWs), isError: true };
    }
    try {
      const executor = new CommandExecutor();
      const output = await executor.checkpoint(explicitWs, id, { message });
      if (output.exitCode !== 0) {
        return { text: mapErrorToLLMMessage(output.stderr, { id }), isError: true };
      }
      const timing = extractTiming(output.stdout);
      if (`${output.stdout}\n${output.stderr}`.includes("Empty workspace, no snapshot created.")) {
        return { text: 'Empty workspace, no snapshot created.', isError: false };
      }
      return { text: `Checkpoint created: ${id}${timing}`, isError: false };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return { text: `Checkpoint error: ${msg}`, isError: true };
    }
  }

  const ws = pluginState.resolvedConfig?.workspace;
  if (ws) {
    const cwdCheck = cwdInsideWorkspace(ws);
    if (cwdCheck.inside) {
      return { text: cwdInsideWorkspaceReason(cwdCheck.cwd, ws), isError: true };
    }
  }

  const result = await pluginState.manager.createCheckpoint({
    id,
    message,
  });
  if (result.skipped) {
    return { text: result.reason ?? "Empty workspace, no snapshot created.", isError: false };
  }
  return { text: result.message, isError: !result.success };
}

export async function handleRollback(
  target?: string,
  workspace?: string,
  numAncestors?: number,
  preview: boolean = false,
): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  const trimmed = target?.trim();

  if (trimmed && numAncestors !== undefined) {
    return { text: "'target' and 'numAncestors' are mutually exclusive", isError: true };
  }
  if (!trimmed && numAncestors === undefined) {
    return {
      text: "Either 'target' or 'numAncestors' is required.\n  target: snapshot id\n  numAncestors: number of ancestors to traverse (>=1)",
      isError: true,
    };
  }
  if (numAncestors !== undefined && (!Number.isFinite(numAncestors) || numAncestors < 1)) {
    return { text: "'numAncestors' must be >= 1", isError: true };
  }

  const resolvedWs = workspace?.trim() || pluginState.resolvedConfig?.workspace;
  if (!resolvedWs) {
    return { text: "No workspace configured", isError: true };
  }

  if (!preview) {
    const cwdCheck = cwdInsideWorkspace(resolvedWs);
    if (cwdCheck.inside) {
      return { text: cwdInsideWorkspaceReason(cwdCheck.cwd, resolvedWs), isError: true };
    }
  }

  if (workspace?.trim()) {
    const execResult = await runRollbackViaExecutor(resolvedWs, trimmed || undefined, numAncestors, preview);
    if (!execResult.isError && !preview) {
      pluginState.skipNextAutoCheckpoint = true;
    }
    return execResult;
  }

  const result = await pluginState.manager.rollback(trimmed || undefined, numAncestors, preview);
  if (result.success && !preview) {
    pluginState.skipNextAutoCheckpoint = true;
  }
  return { text: result.message, isError: !result.success };
}

async function runRollbackViaExecutor(
  ws: string,
  target?: string,
  numAncestors?: number,
  preview: boolean = false,
): Promise<{ text: string; isError: boolean }> {
  try {
    const executor = new CommandExecutor();
    const output = await executor.rollback(ws, target, numAncestors, preview);
    if (output.exitCode !== 0) {
      const label = target || `ancestors=${numAncestors}`;
      return { text: mapErrorToLLMMessage(output.stderr, { id: label }), isError: true };
    }
    if (preview) {
      return {
        text: output.stdout.replace(/\x1b\[[0-9;]*m/g, "").trim(),
        isError: false,
      };
    }
    const timing = extractTiming(output.stdout);
    const desc = target ? `Rolled back to ${target}` : `Rolled back ${numAncestors} ancestor(s)`;
    return { text: `${desc}${timing}`, isError: false };
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error);
    return { text: `Rollback error: ${msg}`, isError: true };
  }
}

export async function handleListCheckpoints(): Promise<{
  text: string;
  isError: boolean;
}> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  try {
    const checkpoints = await pluginState.manager.listCheckpoints();
    if (checkpoints.length === 0) {
      return {
        text: "No checkpoints found. The workspace is active and daemon is responding — there are simply no snapshots yet.",
        isError: false,
      };
    }
    const header = ["ID", "Created At", "Message", "Metadata", "Detail"];
    const rows = checkpoints.map((cp) => [
      cp.snapshot,
      cp.createdAt,
      cp.message ?? "",
      cp.metadata ? JSON.stringify(cp.metadata) : "",
      cp.detail === "summary"
        ? `summary (omitted: ${(cp.omittedFields ?? []).join(", ") || "details"})`
        : "full",
    ]);
    const widths = header.map((h, i) =>
      Math.max(h.length, ...rows.map((r) => r[i].length)),
    );
    const fmt = (cols: string[]) =>
      cols.map((c, i) => c.padEnd(widths[i])).join("  ");
    const lines: string[] = [
      `Checkpoints (${checkpoints.length}):`,
      "",
      fmt(header),
      widths.map((w) => "-".repeat(w)).join("  "),
      ...rows.map(fmt),
    ];
    return { text: lines.join("\n"), isError: false };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { text: `List checkpoints error: ${message}`, isError: true };
  }
}

export async function handleDelete(
  snapshot?: string,
  workspace?: string,
): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  if (!snapshot) {
    return {
      text: "Usage: ws-ckpt-delete <snapshot> [workspace]\n  snapshot: snapshot ID to delete (required)\n  workspace: workspace path (optional, defaults to current)",
      isError: true,
    };
  }
  const ws = workspace || pluginState.resolvedConfig?.workspace;
  if (!ws) {
    return { text: "No workspace path available", isError: true };
  }
  try {
    const executor = new CommandExecutor();
    const output = await executor.delete(snapshot, { workspace: ws, force: true });
    if (output.exitCode !== 0) {
      return { text: mapErrorToLLMMessage(output.stderr, { id: snapshot }), isError: true };
    }
    pluginState.manager.getStore().remove(snapshot);
    return { text: `Snapshot ${snapshot} deleted${extractTiming(output.stdout)}`, isError: false };
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error);
    return { text: `Delete error: ${msg}`, isError: true };
  }
}

export async function handleDiff(
  fromArg?: string,
  toArg?: string,
): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  if (!fromArg) {
    return {
      text: "Usage: ws-ckpt-diff <from> [<to>]\n  from: source snapshot id\n  to:   target snapshot id",
      isError: true,
    };
  }
  const result = await pluginState.manager.execDiffRaw(fromArg, toArg);
  return { text: result.text, isError: !result.success };
}

export async function handleStatus(): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.manager || !pluginState.environmentReady) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }
  const result = await pluginState.manager.getStatus();
  const statusText = result.success
    ? `${result.message}\n(This is the complete daemon status report.)`
    : result.message;
  return { text: statusText, isError: !result.success };
}

export async function handleConfig(
  action?: string,
  key?: string,
  value?: string,
): Promise<{ text: string; isError: boolean }> {
  if (!pluginState.resolvedConfig) {
    return { text: UNAVAILABLE_MSG, isError: true };
  }

  const act = (action ?? "view").toLowerCase();

  if (act === "view") {
    const cfg = pluginState.resolvedConfig;
    const lines: string[] = ["Current ws-ckpt configuration:\n"];
    lines.push(`  autoCheckpoint: ${cfg.autoCheckpoint}`);

    // Always query daemon via `--format json` (stable versioned wire shape;
    // text format isn't a contract). No module-level cache — render
    // directly from this call's parsed result. Aligns with hermes plugin
    // and removes by construction the cross-call RMW race the old cache
    // had. Daemon unreachable / parse-error are reported explicitly; we
    // NEVER fall back to a potentially-stale "last-known" value without
    // saying so, and NEVER silently degrade either to "disabled" (that
    // conflation was the original openclaw bug).
    const cmd = new CommandExecutor();
    const cfgResult = await cmd.config();
    if (cfgResult.exitCode !== 0) {
      lines.push(`  maxSnapshotsNum: (daemon unreachable)`);
      lines.push(`  maxSnapshotsDuration: (daemon unreachable)`);
      lines.push(`  workspace: ${cfg.workspace}`);
      lines.push(`\n(could not query daemon: ${cfgResult.stderr.trim() || "no output"})`);
      return { text: lines.join("\n"), isError: false };
    }
    const parsed = parseWorkspaceCleanupJson(cfgResult.stdout);
    switch (parsed.kind) {
      case "parse-error":
        lines.push(`  maxSnapshotsNum: (unknown — daemon response unparseable)`);
        lines.push(`  maxSnapshotsDuration: (unknown — daemon response unparseable)`);
        break;
      case "disabled":
        lines.push(`  maxSnapshotsNum: not set - auto-cleanup disabled`);
        lines.push(`  maxSnapshotsDuration: not set - auto-cleanup disabled`);
        break;
      case "count":
        lines.push(`  maxSnapshotsNum: ${parsed.num}`);
        lines.push(`  maxSnapshotsDuration: not set`);
        break;
      case "age":
        lines.push(`  maxSnapshotsNum: not set`);
        lines.push(`  maxSnapshotsDuration: ${parsed.duration}`);
        break;
    }
    lines.push(`  workspace: ${cfg.workspace}`);
    const schedules = cfg.cronSchedules ?? [];
    if (schedules.length > 0) {
      lines.push("  cronSchedules:");
      for (const expr of schedules) lines.push(`    - ${expr}`);
    } else {
      lines.push("  cronSchedules:  (disabled)");
    }
    lines.push("\nNote: maxSnapshotsNum / maxSnapshotsDuration are this workspace's effective auto-cleanup policy (per-ws override on top of the daemon default).");
    if (parsed.kind === "parse-error") {
      lines.push(`\n(daemon response did not match expected schema: ${parsed.reason})`);
    }
    return { text: lines.join("\n"), isError: false };
  }

  if (act === "update" || act === "set") {
    if (!key) {
      return {
        text: "Usage: ws-ckpt-config update <key> <value>\n  Available keys: autoCheckpoint, cronSchedules, maxSnapshotsNum, maxSnapshotsDuration, workspace",
        isError: true,
      };
    }

    // Workspace resolution + per-ws scoping live in `cmd.config()`: it falls
    // back to `pluginState.resolvedConfig.workspace`, refuses (exit 2) if
    // neither is set, and reports the chosen ws via `usedWorkspace`.

    if (key === "maxSnapshotsNum") {
      if (value === undefined) {
        return { text: "maxSnapshotsNum requires a value (positive integer, or \"unset\" to restore inherit-global)", isError: true };
      }

      // unset = restore default (delete policy.toml) so admin's later global toggle wins.
      if (value === "unset") {
        const cmd = new CommandExecutor();
        const result = await cmd.config(undefined, { reset: true });
        if (result.exitCode !== 0) {
          return { text: `Failed to reset workspace policy: ${result.stderr}`, isError: true };
        }
        return { text: `Cleared: maxSnapshotsNum unset — workspace ${result.usedWorkspace} now inherits global auto-cleanup.`, isError: false };
      }

      // --- set path ---
      // parseInt("20abc") returns 20 (silent truncation); reject anything that
      // isn't a clean positive-integer literal so we match hermes's int(value).
      const trimmed = value.trim();
      if (!/^[1-9]\d*$/.test(trimmed)) {
        return { text: "maxSnapshotsNum must be a positive integer", isError: true };
      }
      const num = Number(trimmed);
      const cmd = new CommandExecutor();
      const result = await cmd.config(undefined, { enableAutoCleanup: true, autoCleanupKeep: String(num) });
      if (result.exitCode !== 0) {
        return { text: `Failed to configure workspace: ${result.stderr}`, isError: true };
      }
      return { text: `Updated workspace policy for ${result.usedWorkspace}: maxSnapshotsNum = ${num} (auto-cleanup enabled, keep ${num})`, isError: false };
    }

    if (key === "maxSnapshotsDuration") {
      if (value === undefined) {
        return { text: "maxSnapshotsDuration requires a value (e.g. \"7d\", \"24h\", or \"unset\" to restore inherit-global)", isError: true };
      }

      // unset = restore default (delete policy.toml) so admin's later global toggle wins.
      if (value === "unset") {
        const cmd = new CommandExecutor();
        const result = await cmd.config(undefined, { reset: true });
        if (result.exitCode !== 0) {
          return { text: `Failed to reset workspace policy: ${result.stderr}`, isError: true };
        }
        return { text: `Cleared: maxSnapshotsDuration unset — workspace ${result.usedWorkspace} now inherits global auto-cleanup.`, isError: false };
      }

      // --- set path ---
      const cmd = new CommandExecutor();
      const result = await cmd.config(undefined, { enableAutoCleanup: true, autoCleanupKeep: value });
      if (result.exitCode !== 0) {
        return { text: `Failed to configure workspace: ${result.stderr}`, isError: true };
      }
      return { text: `Updated workspace policy for ${result.usedWorkspace}: maxSnapshotsDuration = ${value} (auto-cleanup enabled, keep ${value})`, isError: false };
    }

    if (key === "autoCheckpoint") {
      if (value === undefined) {
        return { text: 'autoCheckpoint requires a value: "true" or "false"', isError: true };
      }
      const normalized = value.trim().toLowerCase();
      // LLM tool callers and shell users emit a wide vocabulary; accept the
      // common bool aliases instead of failing silently for anyone who didn't
      // read stderr. Canonical form remains "true"/"false" in tool descriptions.
      let coerced: boolean;
      if (["true", "1", "yes", "on", "enabled"].includes(normalized)) {
        coerced = true;
      } else if (["false", "0", "no", "off", "disabled"].includes(normalized)) {
        coerced = false;
      } else {
        return { text: `autoCheckpoint must be "true" or "false" (got "${value}")`, isError: true };
      }
      if (coerced) {
        const ws = pluginState.resolvedConfig.workspace;
        if (ws) {
          const cwdCheck = cwdInsideWorkspace(ws);
          if (cwdCheck.inside) {
            return { text: cwdInsideWorkspaceReason(cwdCheck.cwd, ws), isError: true };
          }
        }
      }
      pluginState.resolvedConfig.autoCheckpoint = coerced;
      const persistErr = persistConfig({ autoCheckpoint: coerced });
      const persistNote = persistErr
        ? `\n\nWARNING: Failed to persist config: ${persistErr}. Change is in-memory only.`
        : "";
      return {
        text: `Config updated: autoCheckpoint = ${coerced}${persistNote}`,
        isError: false,
      };
    }

    if (key === "workspace") {
      if (!value) {
        return { text: "workspace requires a path value", isError: true };
      }
      const oldWs = pluginState.resolvedConfig.workspace;
      pluginState.resolvedConfig.workspace = value;
      const schedules = pluginState.resolvedConfig.cronSchedules ?? [];
      const warnings = await CrontabManager.migrate(oldWs, value, schedules);
      const persistErr = persistConfig({ workspace: value });
      // Re-initialize manager with new workspace so subsequent commands use it
      if (pluginState.manager) {
        await pluginState.manager.ensureWorkspace(value);
      }
      let msg = `Config updated: workspace = ${value}`;
      if (persistErr) msg += `\n\nWARNING: Failed to persist config: ${persistErr}. Change is in-memory only.`;
      if (warnings.length > 0) msg += "\n\n" + warnings.join("\n");
      return { text: msg, isError: false };
    }

    if (key === "cronSchedules") {
      if (value === undefined) {
        return {
          text: 'cronSchedules requires a value. Use: add "EXPR", remove "EXPR", or set \'["EXPR"]\'',
          isError: true,
        };
      }
      const ws = pluginState.resolvedConfig.workspace;
      if (!ws) {
        return { text: "No workspace configured", isError: true };
      }
      const current = [...(pluginState.resolvedConfig.cronSchedules ?? [])];
      const parsed = parseSchedulesUpdate(value, current);
      if ("error" in parsed) {
        return { text: parsed.error, isError: true };
      }
      pluginState.resolvedConfig.cronSchedules = parsed.schedules;
      const persistErr = persistConfig({ cronSchedules: parsed.schedules });
      let warnings = "";
      if (persistErr) {
        warnings += `\n\nWARNING: Failed to persist config: ${persistErr}. Change is in-memory only.`;
      }
      if (!(await CrontabManager.syncWithRetry(ws, parsed.schedules))) {
        warnings += "\n\nWARNING: Failed to sync crontab after 3 attempts. " +
          "Config saved but cron snapshots will not run until next session start or manual retry.";
      }
      return {
        text: `cronSchedules updated: ${parsed.schedules.length > 0 ? JSON.stringify(parsed.schedules) : "(disabled)"}` + warnings,
        isError: false,
      };
    }

    return {
      text: `Unknown config key: ${key}. Available: autoCheckpoint, cronSchedules, maxSnapshotsNum, maxSnapshotsDuration, workspace`,
      isError: true,
    };
  }

  return {
    text: `Unknown action: ${act}. Use "view" or "update".`,
    isError: true,
  };
}
