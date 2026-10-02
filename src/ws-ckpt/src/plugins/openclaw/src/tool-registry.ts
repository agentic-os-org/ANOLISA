/**
 * Tool registration for the ws-ckpt OpenClaw plugin.
 *
 * registerTools() registers all 7 ws-ckpt tools with the OpenClaw API.
 * Command registration (registerCommand) is intentionally omitted —
 * all capability is exposed exclusively via Tool Calling.
 */

import type { OpenClawPluginApi } from "../types-shim.js";
import {
  handleCheckpoint,
  handleRollback,
  handleListCheckpoints,
  handleDelete,
  handleDiff,
  handleStatus,
  handleConfig,
  textToolResult,
} from "./handlers.js";

/**
 * Register all 7 ws-ckpt tools with the OpenClaw plugin API.
 *
 * @param api - Plugin API provided by the OpenClaw runtime.
 */
export function registerTools(api: OpenClawPluginApi): void {
  // --- ws-ckpt-config ---
  api.registerTool(
    {
      name: "ws-ckpt-config",
      description:
        "View or update ws-ckpt configuration. " +
        "Configurable keys: " +
        "autoCheckpoint (whether to auto-snapshot at the end of each conversation turn), " +
        "workspace (default workspace absolute path; used by every command without -w. " +
        "If the path is a symlink, use the link itself — do NOT replace it with the " +
        "resolved real path; the daemon registers and matches by the exact string you pass), " +
        "cronSchedules (scheduled cron snapshots using standard 5-field cron expressions; " +
        "value format: 'add \"CRON_EXPR\"', 'remove \"CRON_EXPR\"', or 'set [\"CRON_EXPR\"]'; " +
        "operates on the current workspace; " +
        "if the user's scheduling intent cannot be exactly expressed as a cron expression, " +
        "do NOT write an approximate/degraded schedule — present the closest option and await confirmation), " +
        "maxSnapshotsNum (number of snapshots to keep when auto-cleanup is by count), " +
        "maxSnapshotsDuration (duration to keep when auto-cleanup is by time, e.g. \"7d\"/\"24h\"). " +
        "Only update the specific key requested by the user.",
      parameters: {
        type: "object",
        properties: {
          action: {
            type: "string",
            description: 'Action to perform: "view" (default) or "update"',
          },
          key: {
            type: "string",
            description:
              "Config key to update: autoCheckpoint, workspace, cronSchedules, maxSnapshotsNum, maxSnapshotsDuration",
          },
          value: {
            type: "string",
            description:
              "New value as a string. Formats: " +
              "autoCheckpoint = \"true\"/\"false\"; " +
              "workspace = absolute path; " +
              "cronSchedules = 'add \"CRON_EXPR\"' / 'remove \"CRON_EXPR\"' / 'set [\"CRON_EXPR\"]'; " +
              "maxSnapshotsNum = positive integer (or \"unset\" to restore inherit-global); " +
              "maxSnapshotsDuration = e.g. \"7d\"/\"24h\" (or \"unset\" to restore inherit-global).",
          },
        },
      },
      async execute(_toolCallId, params) {
        const r = await handleConfig(
          params.action as string | undefined,
          params.key as string | undefined,
          params.value as string | undefined,
        );
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-config" },
  );

  // --- ws-ckpt-checkpoint ---
  api.registerTool(
    {
      name: "ws-ckpt-checkpoint",
      description: "Create a checkpoint of the default or specified workspace. Communicates directly with ws-ckpt daemon — no additional CLI verification needed.",
      parameters: {
        type: "object",
        properties: {
          id: {
            type: "string",
            description: "Required: caller-provided snapshot identifier",
          },
          message: {
            type: "string",
            description: "Optional message describing the checkpoint",
          },
          workspace: {
            type: "string",
            description:
              "Optional: workspace absolute path. Defaults to the " +
              "configured workspace. If the path is a symlink, use the " +
              "link itself — do NOT replace it with the resolved real path.",
          },
        },
        required: ["id"],
      },
      async execute(_toolCallId, params) {
        const r = await handleCheckpoint(JSON.stringify(params));
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-checkpoint" },
  );

  // --- ws-ckpt-rollback ---
  api.registerTool(
    {
      name: "ws-ckpt-rollback",
      description:
        "Preview or roll back the workspace to a previous snapshot or N ancestors back. " +
        "Set preview=true to inspect file changes without modifying the workspace. " +
        "Always call ws-ckpt-list first to confirm the target snapshot id " +
        "exists; never roll back to an id you haven't verified.",
      parameters: {
        type: "object",
        properties: {
          target: {
            type: "string",
            description:
              "Snapshot id to roll back to (mutually exclusive with numAncestors).",
          },
          numAncestors: {
            type: "integer",
            description:
              "Number of steps to go back " +
              "(>=1, mutually exclusive with target). " +
              "1 = undo last turn, 2 = undo last two turns.",
          },
          workspace: {
            type: "string",
            description:
              "Optional: workspace absolute path. Defaults to the " +
              "configured workspace. If the path is a symlink, use the " +
              "link itself — do NOT replace it with the resolved real path.",
          },
          preview: {
            type: "boolean",
            description:
              "Optional: preview the file changes without modifying the workspace.",
          },
        },
      },
      async execute(_toolCallId, params) {
        const r = await handleRollback(
          params.target as string | undefined,
          params.workspace as string | undefined,
          params.numAncestors as number | undefined,
          params.preview as boolean | undefined,
        );
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-rollback" },
  );

  // --- ws-ckpt-list ---
  api.registerTool(
    {
      name: "ws-ckpt-list",
      description:
        "List all snapshots managed by ws-ckpt. " +
        "Always display the FULL untruncated table to the user.",
      parameters: { type: "object", properties: {} },
      async execute() {
        const r = await handleListCheckpoints();
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-list" },
  );

  // --- ws-ckpt-diff ---
  api.registerTool(
    {
      name: "ws-ckpt-diff",
      description:
        "Compare file changes between two snapshots, or between a snapshot " +
        "and the current workspace state. Omit 'to' to diff against the " +
        "current workspace. Always display the FULL untruncated diff. " +
        "Do NOT re-interpret or contradict the tool output.",
      parameters: {
        type: "object",
        properties: {
          from: {
            type: "string",
            description: "Source snapshot id",
          },
          to: {
            type: "string",
            description:
              "Target snapshot id or name. Omit to diff against current workspace state.",
          },
        },
        required: ["from"],
      },
      async execute(_toolCallId, params) {
        const r = await handleDiff(
          params.from as string | undefined,
          params.to as string | undefined,
        );
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-diff" },
  );

  // --- ws-ckpt-delete ---
  api.registerTool(
    {
      name: "ws-ckpt-delete",
      description:
        "Delete a snapshot. Confirm the id with ws-ckpt-list first; " +
        "never delete without an explicit user request — " +
        "deletion is permanent and not reversible.",
      parameters: {
        type: "object",
        properties: {
          snapshot: {
            type: "string",
            description: "Required: snapshot id to delete.",
          },
          workspace: {
            type: "string",
            description:
              "Optional: workspace absolute path. Defaults to the " +
              "configured workspace. If the path is a symlink, use the " +
              "link itself — do NOT replace it with the resolved real path.",
          },
        },
        required: ["snapshot"],
      },
      async execute(_toolCallId, params) {
        const r = await handleDelete(
          params.snapshot as string,
          params.workspace as string | undefined,
        );
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-delete" },
  );

  // --- ws-ckpt-status ---
  api.registerTool(
    {
      name: "ws-ckpt-status",
      description:
        "Show ws-ckpt daemon and workspace status — snapshot count, disk " +
        "usage, auto-cleanup policy. Returns complete status from the " +
        "daemon; no extra CLI verification needed.",
      parameters: { type: "object", properties: {} },
      async execute() {
        const r = await handleStatus();
        return textToolResult(r.text, r.isError);
      },
    },
    { name: "ws-ckpt-status" },
  );
}
