/**
 * Shared plugin state singleton.
 *
 * All modules that need to read or mutate manager, environmentReady,
 * resolvedConfig, or pluginApi must import
 * from this module to avoid circular dependencies.
 */

import path from "node:path";
import fs from "node:fs";
import type { BtrfsManager } from "./btrfs-manager.js";
import type { OpenClawPluginApi, OpenClawSessionContext } from "../types-shim.js";
import type { PluginConfig } from "./types.js";

// ---------------------------------------------------------------------------
// Mutable state object — mutated by register() in index.ts
// ---------------------------------------------------------------------------

export const pluginState = {
  /** Singleton BtrfsManager instance — created during registration. */
  manager: null as BtrfsManager | null,

  /** Whether the environment check passed. */
  environmentReady: false,

  /** Runtime workspace setup owns the manager binding until it finishes. */
  workspaceSetup: null as { source: "config" | "session"; done: Promise<void> } | null,

  /** Registration work that must finish before a runtime workspace update binds. */
  registrationSetup: null as { done: Promise<void>; changesBinding: boolean } | null,

  /** Saved reference to the plugin API for use in hooks. */
  pluginApi: null as OpenClawPluginApi | null,

  /** Resolved plugin config for inspection via ws-ckpt-config tool. */
  resolvedConfig: null as PluginConfig | null,

  /** Tool updates and reloaded disables invalidate an earlier enable's validation. */
  autoCheckpointConfigRevision: 0,

  /** Registrations restored by each session; only that session consumes them. */
  rollbackWorkspacesBySession: new Map<string, Set<string>>(),
};

/** Hold the binding until the current config update or session recovery finishes. */
export function beginWorkspaceSetup(source: "config" | "session"): () => void {
  let complete!: () => void;
  const done = new Promise<void>((resolve) => { complete = resolve; });
  pluginState.workspaceSetup = { source, done };
  return () => {
    pluginState.workspaceSetup = null;
    complete();
  };
}

function rollbackOwner(context?: OpenClawSessionContext): string | undefined {
  return context?.sessionKey
    ? JSON.stringify([context.sessionKey, context.sessionId ?? null])
    : undefined;
}

/** Record a rollback only when the host supplies a session owner. */
export function rememberRollback(workspace: string, context?: OpenClawSessionContext): void {
  const owner = rollbackOwner(context);
  if (owner === undefined) return;
  let workspaces = pluginState.rollbackWorkspacesBySession.get(owner);
  if (!workspaces) {
    workspaces = new Set();
    pluginState.rollbackWorkspacesBySession.set(owner, workspaces);
  }
  workspaces.add(workspace);
}

/** Consume this session's state even when its turn cannot create a checkpoint. */
export function takeRollbackWorkspaces(context?: OpenClawSessionContext): Set<string> | undefined {
  const owner = rollbackOwner(context);
  if (owner === undefined) return undefined;
  const workspaces = pluginState.rollbackWorkspacesBySession.get(owner);
  pluginState.rollbackWorkspacesBySession.delete(owner);
  if (!context?.sessionId) return workspaces;

  // The minimum supported SDK omits sessionId from tool factories, but
  // supplies it to hooks. Only consume its sessionKey-only entry as fallback;
  // never consume a different conversation's exact sessionId entry.
  const legacyOwner = rollbackOwner({ sessionKey: context.sessionKey })!;
  const legacy = pluginState.rollbackWorkspacesBySession.get(legacyOwner);
  pluginState.rollbackWorkspacesBySession.delete(legacyOwner);
  if (!workspaces) return legacy;
  for (const workspace of legacy ?? []) workspaces.add(workspace);
  return workspaces;
}

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

export const UNAVAILABLE_MSG =
  "ws-ckpt plugin is not available. Run environment check for details.";

/** Check that implicit operations would use the exact configured selector. */
export function defaultWorkspaceError(): string | undefined {
  const { manager, environmentReady, resolvedConfig, workspaceSetup, registrationSetup } = pluginState;
  if (!manager || !environmentReady) return UNAVAILABLE_MSG;
  const workspace = resolvedConfig?.workspace;
  if (!workspace) return "No workspace configured";
  if (workspaceSetup || registrationSetup?.changesBinding || manager.getWorkspacePath() !== workspace) {
    return `Default workspace ${workspace} is not ready. Wait for any workspace setup to finish, ` +
      "or fix the workspace setup and retry the same workspace configuration.";
  }
  return undefined;
}

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
    cwd = resolveExistingPath(process.cwd());
  } catch {
    return { inside: false, cwd: "" };
  }
  const ws = resolveExistingPath(workspace);
  const relative = path.relative(ws, cwd);
  const inside = relative === "" || (
    relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative)
  );
  return { inside, cwd };
}

function resolveExistingPath(value: string): string {
  const absolute = path.resolve(value);
  try {
    return fs.realpathSync(absolute);
  } catch {
    // A workspace may not exist yet, before its first init command.
    return absolute;
  }
}

/** Resolve parent aliases without following the mutable workspace symlink. */
export function workspaceRegistrationPath(workspace: string): string {
  // Strip trailing separators and "." without collapsing ".." across a
  // parent symlink. The daemon uses the registration anchor, not its target.
  const spelling = workspace.replace(/(?:\/\.?)+$/, "") || "/";
  try {
    // The native resolver preserves symlink/.. filesystem semantics.
    if (path.basename(spelling) === "..") return fs.realpathSync.native(spelling);
    return path.join(fs.realpathSync.native(path.dirname(spelling)), path.basename(spelling));
  } catch {
    // Preserve an unavailable registration's spelling rather than inventing
    // equivalence by cancelling unresolved ".." components.
    return path.isAbsolute(spelling) ? spelling : process.cwd() + "/" + spelling;
  }
}
