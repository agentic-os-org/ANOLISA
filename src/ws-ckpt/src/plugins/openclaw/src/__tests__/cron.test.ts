import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("fs", () => ({
  mkdirSync: vi.fn(),
  rmdirSync: vi.fn(),
}));

vi.mock("../commands.js", () => ({
  runCrontab: vi.fn(),
}));

import { mkdirSync, rmdirSync } from "fs";
import { runCrontab } from "../commands.js";
import {
  validateCronExpr,
  parseSchedulesUpdate,
  CrontabManager,
} from "../cron.js";

const mockRunCrontab = runCrontab as ReturnType<typeof vi.fn>;
const mockMkdirSync = mkdirSync as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  mockMkdirSync.mockImplementation(() => undefined);
});

describe("validateCronExpr", () => {
  it("accepts valid 5-field expression", () => {
    expect(validateCronExpr("0 * * * *")).toBe(true);
  });

  it("strips whitespace", () => {
    expect(validateCronExpr("  0 * * * *  ")).toBe(true);
  });

  it("rejects too few fields", () => {
    expect(validateCronExpr("0 * * *")).toBe(false);
  });

  it("rejects too many fields", () => {
    expect(validateCronExpr("0 * * * * *")).toBe(false);
  });

  it("rejects empty string", () => {
    expect(validateCronExpr("")).toBe(false);
  });

  it("accepts complex expression", () => {
    expect(validateCronExpr("*/5 0-12 1,15 * 1-5")).toBe(true);
  });
});

describe("parseSchedulesUpdate", () => {
  it("add valid expression", () => {
    const r = parseSchedulesUpdate('add "0 * * * *"', []);
    expect("schedules" in r && r.schedules).toEqual(["0 * * * *"]);
  });

  it("add empty returns error", () => {
    const r = parseSchedulesUpdate("add", []);
    expect("error" in r).toBe(true);
  });

  it("add invalid returns error", () => {
    const r = parseSchedulesUpdate("add bad", []);
    expect("error" in r && r.error).toContain("Invalid cron");
  });

  it("add duplicate is idempotent", () => {
    const r = parseSchedulesUpdate('add "0 * * * *"', ["0 * * * *"]);
    expect("schedules" in r && r.schedules).toEqual(["0 * * * *"]);
  });

  it("remove existing", () => {
    const r = parseSchedulesUpdate("remove 0 * * * *", ["0 * * * *", "5 4 * * *"]);
    expect("schedules" in r && r.schedules).toEqual(["5 4 * * *"]);
  });

  it("remove missing returns error", () => {
    const r = parseSchedulesUpdate("remove 0 * * * *", []);
    expect("error" in r && r.error).toContain("not found");
  });

  it("remove empty returns error", () => {
    const r = parseSchedulesUpdate("remove", []);
    expect("error" in r).toBe(true);
  });

  it("set valid JSON array", () => {
    const r = parseSchedulesUpdate('set ["0 * * * *", "5 4 * * *"]', []);
    expect("schedules" in r && r.schedules).toEqual(["0 * * * *", "5 4 * * *"]);
  });

  it("set non-array returns error", () => {
    const r = parseSchedulesUpdate('set "0 * * * *"', []);
    expect("error" in r && r.error).toContain("JSON array");
  });

  it("set invalid cron in array returns error", () => {
    const r = parseSchedulesUpdate('set ["bad"]', []);
    expect("error" in r && r.error).toContain("Invalid cron");
  });

  it("set invalid JSON returns error", () => {
    const r = parseSchedulesUpdate("set not-json", []);
    expect("error" in r).toBe(true);
  });

  it("unknown action returns error", () => {
    const r = parseSchedulesUpdate("delete 0 * * * *", []);
    expect("error" in r && r.error).toContain("Unknown");
  });

  it("strips single quotes from value", () => {
    const r = parseSchedulesUpdate("add '0 * * * *'", []);
    expect("schedules" in r && r.schedules).toEqual(["0 * * * *"]);
  });
});

describe("CrontabManager.sync", () => {
  it("replaces old entries and adds new", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "0 * * * * ws-ckpt checkpoint -w '/ws' -i x\n# comment\n", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    const result = await CrontabManager.sync("/ws", ["5 4 * * *"]);
    expect(result).toBe(true);
    const writeCall = mockRunCrontab.mock.calls[1];
    const input = writeCall[1].input;
    expect(input).toContain("5 4 * * *");
    expect(input).toContain("# comment");
    expect(input).not.toContain("0 * * * * ws-ckpt");
  });

  it("returns false when readCrontab fails", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 2, stdout: "", stderr: "error" });
    expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(false);
  });

  it("handles no crontab for user", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 1, stdout: "", stderr: "no crontab for user" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(true);
  });
});

describe("CrontabManager.remove", () => {
  it("removes all entries for workspace", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "0 * * * * ws-ckpt checkpoint -w '/ws' -i x\nother\n", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    expect(await CrontabManager.remove("/ws")).toBe(true);
    const input = mockRunCrontab.mock.calls[1][1].input;
    expect(input).toContain("other");
    expect(input).not.toContain("ws-ckpt");
  });
});

describe("CrontabManager.syncWithRetry", () => {
  it("returns true on first success", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    expect(await CrontabManager.syncWithRetry("/ws", ["0 * * * *"])).toBe(true);
  });

  it("returns false after all retries fail", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 2, stdout: "", stderr: "error" });
    expect(await CrontabManager.syncWithRetry("/ws", ["0 * * * *"], 2)).toBe(false);
  });
});

describe("CrontabManager.removeWithRetry", () => {
  it("delegates to syncWithRetry", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    expect(await CrontabManager.removeWithRetry("/ws")).toBe(true);
  });
});

describe("CrontabManager.migrate", () => {
  it("skips remove for same workspace", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    const warnings = await CrontabManager.migrate("/ws", "/ws", ["0 * * * *"]);
    expect(warnings).toEqual([]);
  });

  it("warns when remove fails", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 2, stdout: "", stderr: "error" });
    const warnings = await CrontabManager.migrate("/old", "/new", []);
    expect(warnings.length).toBe(1);
    expect(warnings[0]).toContain("Failed to remove");
  });

  it("warns when install fails", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValue({ exitCode: 2, stdout: "", stderr: "error" });

    const warnings = await CrontabManager.migrate("/old", "/new", ["0 * * * *"]);
    expect(warnings.some((w) => w.includes("Failed to install"))).toBe(true);
  });

  it("no warnings when no schedules", async () => {
    mockRunCrontab
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" })
      .mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });

    const warnings = await CrontabManager.migrate("/old", "/new", []);
    expect(warnings).toEqual([]);
  });
});

describe("CrontabManager.listInstalled", () => {
  it("returns cron expressions for matching workspace", async () => {
    const line = '0 * * * * ws-ckpt checkpoint -w \'/ws\' -i "cron-$(date +\\%s)"';
    mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: line + "\n", stderr: "" });

    const result = await CrontabManager.listInstalled("/ws");
    expect(result).toEqual(["0 * * * *"]);
  });

  it("returns empty for no match", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "other line\n", stderr: "" });
    expect(await CrontabManager.listInstalled("/ws")).toEqual([]);
  });

  it("returns empty when readCrontab fails", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 2, stdout: "", stderr: "error" });
    expect(await CrontabManager.listInstalled("/ws")).toEqual([]);
  });
});
