/**
 * Hook registration for the ws-ckpt OpenClaw plugin.
 *
 * Contains SnapshotTracker (tracks user messages across turns) and
 * registerHooks() which wires up message_received, agent_end, and
 * session_start hooks.
 */

import crypto from "node:crypto";
import { CommandExecutor } from "./commands.js";
import type { OpenClawPluginApi, OpenClawSessionContext, PluginHookMessageReceivedEvent } from "../types-shim.js";
import type { PluginConfig } from "./types.js";
import {
  pluginState, defaultWorkspaceError, cwdInsideWorkspace, cwdInsideWorkspaceReason,
  workspaceRegistrationPath, takeRollbackWorkspaces, beginWorkspaceSetup,
} from "./state.js";
import { mapErrorToLLMMessage } from "./btrfs-manager.js";
import { CrontabManager } from "./cron.js";

// ---------------------------------------------------------------------------
// SnapshotTracker — tracks message / step counters for hooks
// ---------------------------------------------------------------------------

class SnapshotTracker {
  private lastUserMessage: string | undefined;

  onMessageReceived(content?: string): void {
    this.lastUserMessage = content
      ? content.slice(0, 80) + (content.length > 80 ? "..." : "")
      : undefined;
  }

  getLastUserMessage(): string | undefined {
    return this.lastUserMessage;
  }
}

const tracker = new SnapshotTracker();

async function resolveCheckpointPath(workspace: string): Promise<string | undefined> {
  try {
    return await new CommandExecutor().resolveWorkspacePath(workspace);
  } catch (error) {
    console.warn(`[ws-ckpt] Automatic checkpoint skipped: ${error instanceof Error ? error.message : String(error)}`);
    return undefined;
  }
}

function disableAutoCheckpointInside(config: PluginConfig, workspacePath: string): boolean {
  const check = cwdInsideWorkspace(workspacePath);
  if (!check.inside) return false;
  config.autoCheckpoint = false;
  console.warn(`[ws-ckpt] Disabling auto-checkpoint: ${cwdInsideWorkspaceReason(check.cwd, workspacePath)}`);
  return true;
}

// ---------------------------------------------------------------------------
// registerHooks — wire up the 3 lifecycle hooks
// ---------------------------------------------------------------------------

/**
 * Register all ws-ckpt lifecycle hooks with the OpenClaw API.
 *
 * @param api    - Plugin API provided by the OpenClaw runtime.
 * @param config - Resolved plugin configuration.
 */
export function registerHooks(api: OpenClawPluginApi, config: PluginConfig): void {
  // Hook: message_received — record user message for checkpoint context
  api.on("message_received", (event: unknown) => {
    const e = event as PluginHookMessageReceivedEvent;
    tracker.onMessageReceived(e.content);
  }, { priority: 0 });

  // Hook: agent_end — create end-of-turn checkpoint
  api.on("agent_end", async (_event: unknown, context: unknown) => {
    const restored = takeRollbackWorkspaces(context as OpenClawSessionContext | undefined);
    if (!config.autoCheckpoint || defaultWorkspaceError()) return;
    const message = tracker.getLastUserMessage() ?? "turn end";

    // A bare-selector lookup can yield while the plugin is reconfigured.
    let workspace = config.workspace;
    let registrationPath = await resolveCheckpointPath(workspace);
    if (!config.autoCheckpoint || defaultWorkspaceError()) return;
    if (workspace !== config.workspace) {
      workspace = config.workspace;
      registrationPath = await resolveCheckpointPath(workspace);
    }
    const manager = pluginState.manager;
    if (!registrationPath || !config.autoCheckpoint || !manager ||
        config.workspace !== workspace || defaultWorkspaceError()) return;
    if (restored?.size && restored.has(workspaceRegistrationPath(registrationPath))) return;
    if (disableAutoCheckpointInside(config, registrationPath)) return;

    const snapshotId = crypto.randomUUID().slice(0, 8);
    const metadata = JSON.stringify({ auto: true, type: "turn_end" });

    console.log(`[ws-ckpt] End-of-turn checkpoint: ${snapshotId}`);

    try {
      const result = await manager.createCheckpoint({ id: snapshotId, message, metadata });
      if (result.skipped) {
        console.debug(`[ws-ckpt] Checkpoint skipped: ${result.reason ?? "no changes"}`);
      } else if (result.success) {
        console.log(`[ws-ckpt] Checkpoint created: ${result.snapshot}`);
      } else {
        console.warn(`[ws-ckpt] Checkpoint failed: ${result.message}`);
      }
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      console.warn(`[ws-ckpt] End-of-turn checkpoint error: ${mapErrorToLLMMessage(msg)}`);
    }
  }, { priority: 0 });

  // Hook: session_start — sync cron + create initial checkpoint
  api.on("session_start", async (_event: unknown) => {
    // Sync cron schedules — independent of autoCheckpoint
    const cronWs = pluginState.resolvedConfig?.workspace;
    if (cronWs) {
      const schedules = config.cronSchedules ?? [];
      if (schedules.length > 0) {
        try {
          if (await CrontabManager.syncWithRetry(cronWs, schedules)) {
            console.log(`[ws-ckpt] Cron synced: ${schedules.length} schedule(s)`);
          } else {
            console.warn("[ws-ckpt] Cron sync failed after 3 attempts");
          }
        } catch (err) {
          console.warn("[ws-ckpt] Cron sync error:", err instanceof Error ? err.message : String(err));
        }
      }
    }

    if (!config.autoCheckpoint) return;
    const workspace = pluginState.resolvedConfig?.workspace;
    const manager = pluginState.manager;
    if (!manager || !pluginState.environmentReady || !workspace || pluginState.workspaceSetup) return;

    const registrationPath = await resolveCheckpointPath(workspace);
    if (!registrationPath || !config.autoCheckpoint || !pluginState.environmentReady ||
        pluginState.manager !== manager || config.workspace !== workspace || pluginState.workspaceSetup) return;
    if (disableAutoCheckpointInside(config, registrationPath)) return;

    // A bound selector can be a registered ID. Only paths can go through init,
    // so use status-first setup when the configured selector still needs binding.
    if (manager.getWorkspacePath() !== workspace) {
      if (pluginState.registrationSetup) return;
      const finishSetup = beginWorkspaceSetup("session");
      try {
        if (!(await manager.ensureWorkspace(workspace))) return;
      } catch (err) {
        console.warn("[ws-ckpt] Session start workspace setup failed:", err);
        return;
      } finally {
        finishSetup();
      }
    }

    if (!config.autoCheckpoint || pluginState.manager !== manager ||
        pluginState.resolvedConfig?.workspace !== workspace || defaultWorkspaceError()) return;

    const snapshotId = crypto.randomUUID().slice(0, 8);
    const metadata = JSON.stringify({ auto: true, type: "initial" });

    console.log(`[ws-ckpt] Initial checkpoint: ${snapshotId}`);

    try {
      const result = await manager.createCheckpoint({ id: snapshotId, message: "session start", metadata });
      if (result.skipped) {
        console.debug(`[ws-ckpt] Initial checkpoint skipped: ${result.reason ?? "no changes"}`);
      } else if (result.success) {
        console.log(`[ws-ckpt] Initial checkpoint created: ${result.snapshot}`);
      } else {
        console.warn(`[ws-ckpt] Initial checkpoint failed: ${result.message}`);
      }
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      console.warn(`[ws-ckpt] Initial checkpoint error: ${mapErrorToLLMMessage(msg)}`);
    }
  }, { priority: 0 });
}
