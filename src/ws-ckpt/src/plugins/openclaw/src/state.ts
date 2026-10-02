/**
 * Shared plugin state singleton.
 *
 * All modules that need to read or mutate manager, environmentReady,
 * resolvedConfig, or pluginApi must import
 * from this module to avoid circular dependencies.
 */

import path from "node:path";
import type { BtrfsManager } from "./btrfs-manager.js";
import type { OpenClawPluginApi } from "../types-shim.js";
import type { PluginConfig } from "./types.js";

// ---------------------------------------------------------------------------
// Mutable state object — mutated by register() in index.ts
// ---------------------------------------------------------------------------

export const pluginState = {
  /** Singleton BtrfsManager instance — created during registration. */
  manager: null as BtrfsManager | null,

  /** Whether the environment check passed. */
  environmentReady: false,

  /** Saved reference to the plugin API for use in hooks. */
  pluginApi: null as OpenClawPluginApi | null,

  /** Resolved plugin config for inspection via ws-ckpt-config tool. */
  resolvedConfig: null as PluginConfig | null,

  /** One-shot flag: skip the next turn-end auto-checkpoint (set after rollback). */
  skipNextAutoCheckpoint: false,
};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

export const UNAVAILABLE_MSG =
  "ws-ckpt plugin is not available. Run environment check for details.";

export function cwdInsideWorkspaceReason(cwd: string, workspace: string): string {
  return (
    `Refused: cwd=${cwd} is inside workspace=${workspace}. ` +
    "ws-ckpt replaces the workspace inode during init/checkpoint/rollback, " +
    "which would invalidate the process cwd. " +
    "This is NOT retryable — do NOT call any ws-ckpt tool again in this session. " +
    "The user must launch the session from outside the workspace directory."
  );
}

export function cwdInsideWorkspace(workspace: string): { inside: boolean; cwd: string } {
  let cwd: string;
  try {
    cwd = path.resolve(process.cwd());
  } catch {
    return { inside: false, cwd: "" };
  }
  const ws = path.resolve(workspace);
  return { inside: cwd === ws || cwd.startsWith(ws + path.sep), cwd };
}
