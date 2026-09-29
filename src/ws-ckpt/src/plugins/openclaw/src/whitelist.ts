/**
 * Whitelist check for the ws-ckpt OpenClaw plugin.
 *
 * Verifies all ws-ckpt tool names are present in the active OpenClaw tool
 * allowlist and warns about missing ones. The plugin never writes
 * openclaw.json itself: out-of-band writes trip the OpenClaw >= 2026.9.2
 * config snapshot-hash guard ("config changed since last load"). Entries are
 * added by install-openclaw.sh through the sanctioned `openclaw config set`
 * mutation path.
 */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import type { OpenClawPluginApi } from "../types-shim.js";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** All ws-ckpt tool names that need to be in the active allowlist. */
export const WS_CKPT_TOOL_NAMES = [
  "ws-ckpt-checkpoint",
  "ws-ckpt-rollback",
  "ws-ckpt-list",
  "ws-ckpt-delete",
  "ws-ckpt-diff",
  "ws-ckpt-config",
  "ws-ckpt-status",
];

/** Once-per-process guard: avoid repeated writes during reload loops. */
let alreadyEnsured = false;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Check that all ws-ckpt tools are present in the active OpenClaw tool
 * allowlist, warning once per process if any are missing.
 *
 * Reads the current allowlist from disk (api.config may be a stale snapshot
 * during reload). Never writes openclaw.json: config mutations belong to the
 * installer (install-openclaw.sh), which goes through `openclaw config set`.
 */
export function ensureToolsAllowlist(api: OpenClawPluginApi): void {
  if (alreadyEnsured) return;
  try {
    const configPath = resolveOpenClawConfigPath();
    if (!configPath) return;

    const onDisk = readAllowlistFromDisk(configPath);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const cfg = api.config as any;
    const fromApi = selectAllowlist(cfg?.tools) ?? [];
    const currentAllow = onDisk ?? fromApi;

    const missing = WS_CKPT_TOOL_NAMES.filter((t) => !currentAllow.includes(t));
    alreadyEnsured = true;
    if (missing.length === 0) return;

    console.warn(
      `[ws-ckpt] ${missing.length} tool(s) missing from the active tools allowlist: ${missing.join(", ")}. ` +
        `Re-run 'ws-ckpt plugin install --runtime openclaw' to add them via 'openclaw config set'.`,
    );
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.warn(`[ws-ckpt] Failed to check the tools allowlist: ${msg}`);
  }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/**
 * Resolve the openclaw.json config path (mirrors logic in openclaw-config.ts).
 */
function resolveOpenClawConfigPath(): string | null {
  try {
    const env = process.env;
    const explicitPath = env.OPENCLAW_CONFIG_PATH?.trim();
    if (explicitPath) {
      return path.resolve(explicitPath);
    }
    const stateDir =
      env.OPENCLAW_STATE_DIR?.trim() ||
      path.join(os.homedir(), ".openclaw");
    return path.join(stateDir, "openclaw.json");
  } catch {
    return null;
  }
}

function selectAllowlist(tools: unknown): string[] | null {
  if (!tools || typeof tools !== "object") return null;
  const policy = tools as { allow?: unknown; alsoAllow?: unknown };
  const allow = Array.isArray(policy.allow) ? policy.allow.map(String) : [];
  if (allow.length > 0) return allow;
  if (Array.isArray(policy.alsoAllow)) return policy.alsoAllow.map(String);
  return Array.isArray(policy.allow) ? allow : null;
}

/**
 * Returns null if the file is missing, unreadable, not plain JSON, or
 * delegates allowlist-relevant config through `$include` — in the latter
 * case the raw root file is not the effective config, and the caller falls
 * back to api.config (which OpenClaw provides include-resolved).
 */
function readAllowlistFromDisk(configPath: string): string[] | null {
  try {
    if (!fs.existsSync(configPath)) return null;
    const raw = fs.readFileSync(configPath, "utf-8");
    const parsed = JSON.parse(raw);
    if (delegatesAllowlist(parsed)) return null;
    return selectAllowlist(parsed?.tools);
  } catch {
    return null;
  }
}

/**
 * True when the root config delegates any part of the tools allowlist
 * through `$include`: a root-level include (merged into the whole config),
 * a tools-level include, or an allow/alsoAllow field that is itself an
 * include object. Includes elsewhere (e.g. logging) cannot affect the
 * allowlist, so the on-disk read stays authoritative for those.
 */
function delegatesAllowlist(parsed: unknown): boolean {
  if (!parsed || typeof parsed !== "object") return false;
  const root = parsed as Record<string, unknown>;
  if ("$include" in root) return true;
  const tools = root.tools;
  if (!tools || typeof tools !== "object") return false;
  const policy = tools as Record<string, unknown>;
  if ("$include" in policy) return true;
  return isIncludeObject(policy.allow) || isIncludeObject(policy.alsoAllow);
}

function isIncludeObject(value: unknown): boolean {
  return (
    !!value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    "$include" in (value as Record<string, unknown>)
  );
}
