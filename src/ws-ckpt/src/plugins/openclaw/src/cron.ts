import { mkdirSync, rmdirSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { runCrontab } from "./commands.js";

const LOCK_DIR = join(tmpdir(), "ws-ckpt-cron.lock");

// `crontab` rejects the *entire* submitted file when a single field is out of
// range, so a schedule that passes a token-count check but fails cron's own
// validation ("99 99 99 99 99", "0 25 * * *", "* * 32 * *") permanently
// bricks every later sync for that workspace: the garbage entry stays in the
// plugin config (it also passes the load-time filter) and each sync attempt
// writes a file crontab refuses to install. Validate the real 5-field cron
// grammar (values, ranges, lists, steps, month/day names) like crontab(5).
const CRON_FIELD_NAMES: ReadonlyArray<Readonly<Record<string, number>> | undefined> = [
  undefined,
  undefined,
  undefined,
  {
    jan: 1, feb: 2, mar: 3, apr: 4, may: 5, jun: 6,
    jul: 7, aug: 8, sep: 9, oct: 10, nov: 11, dec: 12,
  },
  { sun: 0, mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6 },
];

const CRON_FIELD_BOUNDS: ReadonlyArray<{ min: number; max: number }> = [
  { min: 0, max: 59 }, // minute
  { min: 0, max: 23 }, // hour
  { min: 1, max: 31 }, // day of month
  { min: 1, max: 12 }, // month
  { min: 0, max: 7 }, // day of week (0 and 7 are both Sunday)
];

/** Parse one comma-separated cron field item: a star, a star with a step,
 *  a plain value ("5"), a range ("1-6"), a range with a step ("1-30/2"),
 *  or a name range ("mon-fri"). Returns false for anything crontab would
 *  reject. (A star-step is written as a star followed by "/15"; it cannot be
 *  spelled out here because it would terminate this comment.) */
function isValidCronItem(item: string, fieldIndex: number): boolean {
  const slash = item.indexOf("/");
  const base = slash === -1 ? item : item.slice(0, slash);
  const step = slash === -1 ? undefined : item.slice(slash + 1);

  if (step !== undefined) {
    // crontab only accepts positive numeric steps.
    if (!/^\d+$/.test(step) || parseInt(step, 10) < 1) return false;
  }
  if (slash !== -1 && base === "") return false;

  if (base === "*") {
    // "*" with a step is fine; "*" is fine. A bare "*" cannot be a range
    // endpoint, which the parsing below already guarantees.
    return true;
  }

  const bounds = CRON_FIELD_BOUNDS[fieldIndex];
  const names = CRON_FIELD_NAMES[fieldIndex];
  const parseEndpoint = (text: string): number | undefined => {
    if (/^\d+$/.test(text)) {
      const value = parseInt(text, 10);
      return value >= bounds.min && value <= bounds.max ? value : undefined;
    }
    if (names) {
      const value = names[text.toLowerCase()];
      if (value !== undefined && value >= bounds.min && value <= bounds.max) {
        return value;
      }
    }
    return undefined;
  };

  const dash = base.indexOf("-");
  if (dash === -1) {
    return parseEndpoint(base) !== undefined;
  }
  const start = base.slice(0, dash);
  const end = base.slice(dash + 1);
  // Both endpoints must exist; mixing names and numbers ("1-fri") is not
  // something crontab documents, so reject it. Wrap-around ranges ("fri-mon")
  // are rejected as well: write them as an explicit list instead.
  const startIsName = !/^\d+$/.test(start);
  const endIsName = !/^\d+$/.test(end);
  if (start === "" || end === "" || startIsName !== endIsName) return false;
  const startValue = parseEndpoint(start);
  const endValue = parseEndpoint(end);
  return (
    startValue !== undefined && endValue !== undefined && startValue <= endValue
  );
}

function isValidCronField(field: string, fieldIndex: number): boolean {
  if (field === "") return false;
  // crontab rejects empty list items (",,", trailing comma).
  return field.split(",").every((item) => item !== "" && isValidCronItem(item, fieldIndex));
}

export function validateCronExpr(expr: string): boolean {
  const fields = expr.trim().split(/\s+/);
  if (fields.length !== 5) return false;
  return fields.every((field, index) => isValidCronField(field, index));
}

// Match: ws-ckpt checkpoint ... -w '<path>' or -w <path>
const MARKER_RE_QUOTED = /ws-ckpt\s+checkpoint\s+.*-w\s+'([^']+)'/;
const MARKER_RE_UNQUOTED = /ws-ckpt\s+checkpoint\s+.*-w\s+(\S+)/;

function shellQuote(s: string): string {
  return "'" + s.replace(/'/g, "'\\''") + "'";
}

function buildCronLine(workspace: string, schedule: string): string {
  return (
    `${schedule} /usr/local/bin/ws-ckpt checkpoint -w ${shellQuote(workspace)}` +
    ` -s "cron-$(date +\\%s)"` +
    ` -m "scheduled snapshot"` +
    ` --metadata '{"auto":true,"type":"cron"}'` +
    ` >/dev/null 2>&1`
  );
}

async function withLock<T>(fn: () => Promise<T>): Promise<T> {
  const maxWait = 5000;
  const interval = 50;
  let waited = 0;
  while (true) {
    try {
      mkdirSync(LOCK_DIR);
      break;
    } catch {
      if (waited >= maxWait) break; // proceed unlocked rather than fail
      await new Promise((r) => setTimeout(r, interval));
      waited += interval;
    }
  }
  try {
    return await fn();
  } finally {
    try { rmdirSync(LOCK_DIR); } catch { /* already removed */ }
  }
}

async function readCrontab(): Promise<string[] | null> {
  const result = await runCrontab(["-l"], { timeout: 10_000 });
  if (result.exitCode !== 0) {
    if (result.exitCode === 1 && result.stderr.includes("no crontab for")) {
      return [];
    }
    return null;
  }
  return result.stdout.split("\n").filter((l: string) => l !== "");
}

async function writeCrontab(lines: string[]): Promise<boolean> {
  let content = lines.join("\n");
  if (content && !content.endsWith("\n")) content += "\n";
  const result = await runCrontab(["-"], { input: content, timeout: 10_000 });
  return result.exitCode === 0;
}

function extractWorkspace(line: string): string | null {
  let m = MARKER_RE_QUOTED.exec(line);
  if (m) return m[1];
  m = MARKER_RE_UNQUOTED.exec(line);
  if (m) return m[1];
  return null;
}

function matchesWorkspace(line: string, workspace: string): boolean {
  return extractWorkspace(line) === workspace;
}

export type ParseResult = { schedules: string[] } | { error: string };

export function parseSchedulesUpdate(value: string, current: string[]): ParseResult {
  const spaceIdx = value.indexOf(" ");
  const subAction = (spaceIdx >= 0 ? value.slice(0, spaceIdx) : value).toLowerCase();
  let subVal = spaceIdx >= 0 ? value.slice(spaceIdx + 1).trim() : "";
  if (subVal.length >= 2 && subVal[0] === subVal[subVal.length - 1] && (subVal[0] === '"' || subVal[0] === "'")) {
    subVal = subVal.slice(1, -1);
  }

  const result = [...current];
  if (subAction === "add") {
    if (!subVal) return { error: "add requires a cron expression" };
    if (!validateCronExpr(subVal)) {
      return { error: `Invalid cron expression: "${subVal}". Expected 5 fields, e.g. "0 * * * *"` };
    }
    if (!result.includes(subVal)) result.push(subVal);
  } else if (subAction === "remove") {
    if (!subVal) return { error: "remove requires a cron expression" };
    const idx = result.indexOf(subVal);
    if (idx < 0) return { error: `"${subVal}" not found in current schedules` };
    result.splice(idx, 1);
  } else if (subAction === "set") {
    try {
      const parsed = JSON.parse(subVal);
      if (!Array.isArray(parsed)) throw new Error("not an array");
      for (const e of parsed) {
        if (!validateCronExpr(String(e))) {
          return { error: `Invalid cron expression in array: "${e}"` };
        }
      }
      return { schedules: parsed.map(String) };
    } catch {
      return { error: "set requires a JSON array, e.g. '[\"0 * * * *\"]'" };
    }
  } else {
    return { error: `Unknown cronSchedules sub-action: ${subAction}. Use: add "EXPR", remove "EXPR", or set '["EXPR"]'` };
  }
  return { schedules: result };
}

export class CrontabManager {
  static async sync(workspace: string, schedules: string[]): Promise<boolean> {
    return withLock(async () => {
      const lines = await readCrontab();
      if (lines === null) return false;
      const kept = lines.filter((l) => !matchesWorkspace(l, workspace));
      for (const s of schedules) {
        kept.push(buildCronLine(workspace, s));
      }
      return writeCrontab(kept);
    });
  }

  static async remove(workspace: string): Promise<boolean> {
    return CrontabManager.sync(workspace, []);
  }

  static async syncWithRetry(workspace: string, schedules: string[], retries = 3): Promise<boolean> {
    for (let i = 0; i < retries; i++) {
      if (await CrontabManager.sync(workspace, schedules)) return true;
    }
    return false;
  }

  static async removeWithRetry(workspace: string, retries = 3): Promise<boolean> {
    return CrontabManager.syncWithRetry(workspace, [], retries);
  }

  static async migrate(
    oldWorkspace: string,
    newWorkspace: string,
    schedules: string[],
  ): Promise<string[]> {
    const warnings: string[] = [];
    if (oldWorkspace && oldWorkspace !== newWorkspace) {
      if (!(await CrontabManager.removeWithRetry(oldWorkspace))) {
        warnings.push(
          `WARNING: Failed to remove cron entries for old workspace ${oldWorkspace}. ` +
          `Run \`crontab -e\` to manually remove lines containing -w '${oldWorkspace}'.`
        );
      }
    }
    if (schedules.length > 0) {
      if (!(await CrontabManager.syncWithRetry(newWorkspace, schedules))) {
        warnings.push(
          `WARNING: Failed to install cron entries for ${newWorkspace}. ` +
          `Cron snapshots will not run until next session start or manual retry.`
        );
      }
    }
    return warnings;
  }

  static async listInstalled(workspace: string): Promise<string[]> {
    const lines = await readCrontab();
    if (lines === null) return [];
    const result: string[] = [];
    for (const line of lines) {
      if (!matchesWorkspace(line, workspace)) continue;
      const parts = line.split(/\s+/);
      if (parts.length >= 5) result.push(parts.slice(0, 5).join(" "));
    }
    return result;
  }
}
