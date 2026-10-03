/**
 * Hook registration for the ws-ckpt OpenClaw plugin.
 *
 * Contains SnapshotTracker (tracks user messages across turns) and
 * registerHooks() which wires up message_received, agent_end, and
 * session_start hooks.
 */

import crypto from "node:crypto";
import type { OpenClawPluginApi, PluginHookMessageReceivedEvent } from "../types-shim.js";
import type { PluginConfig } from "./types.js";
import { pluginState, cwdInsideWorkspace, cwdInsideWorkspaceReason } from "./state.js";
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
  api.on("agent_end", async (_event: unknown) => {
    if (pluginState.skipNextAutoCheckpoint) {
      pluginState.skipNextAutoCheckpoint = false;
      return;
    }
    if (!config.autoCheckpoint) return;
    if (!pluginState.manager || !pluginState.environmentReady) return;

    const workspace = pluginState.resolvedConfig?.workspace;
    const cwdCheckEnd = workspace ? cwdInsideWorkspace(workspace) : undefined;
    if (cwdCheckEnd?.inside) {
      config.autoCheckpoint = false;
      console.warn(`[ws-ckpt] Disabling auto-checkpoint: ${cwdInsideWorkspaceReason(cwdCheckEnd.cwd, workspace!)}`);
    } else {
      const snapshotId = crypto.randomUUID().slice(0, 8);
      const message = tracker.getLastUserMessage() ?? "turn end";
      const metadata = JSON.stringify({ auto: true, type: "turn_end" });

      console.log(`[ws-ckpt] End-of-turn checkpoint: ${snapshotId}`);

      try {
        const result = await pluginState.manager.createCheckpoint({ id: snapshotId, message, metadata });
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
    if (!pluginState.manager || !pluginState.environmentReady || !workspace) return;

    const cwdCheckStart = cwdInsideWorkspace(workspace);
    if (cwdCheckStart.inside) {
      config.autoCheckpoint = false;
      console.warn(`[ws-ckpt] Disabling auto-checkpoint: ${cwdInsideWorkspaceReason(cwdCheckStart.cwd, workspace)}`);
    } else {
      try {
        await pluginState.manager.initialize(workspace);
      } catch (err) {
        console.warn("[ws-ckpt] Session start workspace re-init failed:", err);
        return;
      }

      const snapshotId = crypto.randomUUID().slice(0, 8);
      const metadata = JSON.stringify({ auto: true, type: "initial" });

      console.log(`[ws-ckpt] Initial checkpoint: ${snapshotId}`);

      try {
        const result = await pluginState.manager.createCheckpoint({ id: snapshotId, message: "session start", metadata });
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
    }
  }, { priority: 0 });
}
