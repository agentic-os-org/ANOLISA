import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { PluginConfig } from "./types.js";

type PersistableKeys = Pick<PluginConfig, "autoCheckpoint" | "workspace" | "cronSchedules">;

function resolveConfigPath(): string {
  const stateDir =
    process.env.OPENCLAW_STATE_DIR?.trim() ||
    path.join(os.homedir(), ".openclaw");
  return path.join(stateDir, "ws-ckpt.json");
}

export function loadPersistedConfig(): Partial<PluginConfig> {
  try {
    const p = resolveConfigPath();
    if (!fs.existsSync(p)) return {};
    const raw = fs.readFileSync(p, "utf-8");
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const result: Partial<PluginConfig> = {};
    if (typeof parsed.autoCheckpoint === "boolean") result.autoCheckpoint = parsed.autoCheckpoint;
    if (typeof parsed.workspace === "string") result.workspace = parsed.workspace;
    if (Array.isArray(parsed.cronSchedules)) {
      result.cronSchedules = parsed.cronSchedules.filter((e: unknown) => typeof e === "string");
    }
    return result;
  } catch {
    return {};
  }
}

export function persistConfig(partial: Partial<PersistableKeys>): string {
  try {
    const configPath = resolveConfigPath();
    let existing: Record<string, unknown> = {};
    try {
      if (fs.existsSync(configPath)) {
        const raw = fs.readFileSync(configPath, "utf-8");
        const parsed = JSON.parse(raw);
        if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
          existing = parsed;
        }
      }
    } catch { /* start fresh */ }
    Object.assign(existing, partial);
    const dir = path.dirname(configPath);
    fs.mkdirSync(dir, { recursive: true });
    const tmpPath = `${configPath}.tmp.${process.pid}`;
    try {
      fs.writeFileSync(tmpPath, JSON.stringify(existing, null, 2) + "\n", {
        encoding: "utf-8",
        mode: 0o600,
      });
      fs.renameSync(tmpPath, configPath);
    } catch (err) {
      // A partial write or a failed rename must not strand the temp file in
      // the state directory; repeated failures would otherwise accumulate
      // one .tmp.<pid> residue per attempt. Cleanup is best-effort and the
      // original error still wins.
      try {
        fs.unlinkSync(tmpPath);
      } catch {
        /* the temp file may never have been created */
      }
      throw err;
    }
    return "";
  } catch (err) {
    return err instanceof Error ? err.message : String(err);
  }
}
