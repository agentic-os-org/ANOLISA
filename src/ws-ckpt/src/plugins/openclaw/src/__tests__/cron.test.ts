import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { execFileSync } from "child_process";
import { resolve } from "path";

vi.mock("fs", () => ({
  mkdirSync: vi.fn(),
  rmdirSync: vi.fn(),
  accessSync: vi.fn(),
  statSync: vi.fn(),
  constants: { X_OK: 1 },
}));

vi.mock("../commands.js", () => ({
  runCrontab: vi.fn(),
}));

import { accessSync, statSync, mkdirSync, rmdirSync } from "fs";
import { runCrontab } from "../commands.js";
import {
  validateCronExpr,
  parseSchedulesUpdate,
  CrontabManager,
} from "../cron.js";

const mockRunCrontab = runCrontab as ReturnType<typeof vi.fn>;
const mockMkdirSync = mkdirSync as ReturnType<typeof vi.fn>;
const mockAccessSync = vi.mocked(accessSync);
const mockStatSync = vi.mocked(statSync);

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubEnv("PATH", "/usr/bin:/bin");
  mockMkdirSync.mockImplementation(() => undefined);
  mockAccessSync.mockImplementation(() => undefined);
  mockStatSync.mockReturnValue({ isFile: () => true } as ReturnType<typeof statSync>);
});

afterEach(() => {
  vi.unstubAllEnvs();
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

describe("cron executable discovery", () => {
  beforeEach(() => {
    mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });
  });

  it.each(["/usr/bin", "/usr/local/bin", "/opt/anolisa/bin"])(
    "records the executable from %s rather than a fixed install prefix",
    async (directory) => {
      vi.stubEnv("PATH", `/missing:${directory}`);
      mockAccessSync.mockImplementation((path) => {
        if (path === "/missing/ws-ckpt") throw new Error("not found");
      });
      expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(true);
      expect(mockRunCrontab.mock.calls[1][1].input).toContain(
        `0 * * * * '${directory}/ws-ckpt' checkpoint`,
      );
      expect(mockAccessSync).toHaveBeenCalledWith(`${directory}/ws-ckpt`, 1);
    },
  );

  it("fails without touching the crontab when no executable is available", async () => {
    mockAccessSync.mockImplementation(() => { throw new Error("not found"); });
    expect(await CrontabManager.syncWithRetry("/ws", ["0 * * * *"])).toBe(false);
    expect(mockRunCrontab).not.toHaveBeenCalled();
    expect(mockRmdirSync).toHaveBeenCalledTimes(3);
  });

  it.each(["/opt/bad\npath", "/opt/bad\rpath", "/opt/100%"])(
    "rejects cron-unsafe executable path %j before reading or writing",
    async (directory) => {
      vi.stubEnv("PATH", directory);
      expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(false);
      expect(mockRunCrontab).not.toHaveBeenCalled();
    },
  );

  it("removes schedules without requiring the CLI to remain installed", async () => {
    mockAccessSync.mockImplementation(() => { throw new Error("uninstalled"); });
    mockRunCrontab.mockResolvedValueOnce({
      exitCode: 0, stdout: "0 * * * * '/usr/bin/ws-ckpt' checkpoint -w '/ws'\n# keep\n", stderr: "",
    });
    expect(await CrontabManager.remove("/ws")).toBe(true);
    expect(mockAccessSync).not.toHaveBeenCalled();
    expect(mockRunCrontab.mock.calls[1][1].input).toBe("# keep\n");
  });

  it("replaces legacy entries and remains idempotent after quoting the binary", async () => {
    let installed = "0 * * * * /usr/local/bin/ws-ckpt checkpoint -w '/ws'\n" +
      "5 * * * * ws-ckpt checkpoint -w /other\n# keep\n";
    mockRunCrontab.mockImplementation(async (_args, opts) => {
      if (opts?.input !== undefined) installed = opts.input;
      return { exitCode: 0, stdout: installed, stderr: "" };
    });
    expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(true);
    const first = installed;
    expect(first).not.toContain("/usr/local/bin/ws-ckpt");
    expect(first).toContain("5 * * * * ws-ckpt checkpoint -w /other\n# keep\n");
    expect(await CrontabManager.sync("/ws", ["0 * * * *"])).toBe(true);
    expect(installed).toBe(first);
    expect(await CrontabManager.listInstalled("/ws")).toEqual(["0 * * * *"]);
  });

  it("executes a discovered launcher with a restricted cron PATH and literal shell characters", async () => {
    const fs = await vi.importActual<typeof import("fs")>("fs");
    const directory = fs.mkdtempSync(resolve("node_modules", "cron-launcher-"));
    const bindir = `${directory}/owner's bin;$literal`;
    const launcher = `${bindir}/ws-ckpt`;
    const target = `${directory}/release-cli`;
    const workspace = "/workspace with spaces";
    try {
      fs.mkdirSync(bindir);
      fs.writeFileSync(target, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n", { mode: 0o755 });
      fs.symlinkSync(target, launcher);
      vi.stubEnv("PATH", bindir);
      mockAccessSync.mockImplementation(fs.accessSync);
      mockStatSync.mockImplementation(fs.statSync);
      expect(await CrontabManager.sync(workspace, ["0 * * * *"])).toBe(true);
      const line: string = mockRunCrontab.mock.calls[1][1].input.trim();
      expect(line).not.toContain(target);
      const command = line.replace(/^\S+\s+\S+\s+\S+\s+\S+\s+\S+\s+/, "")
        .replace(/ >\/dev\/null 2>&1$/, "").replace(/\\%/g, "%");
      const output = execFileSync("/bin/sh", ["-c", command], {
        encoding: "utf-8", env: { PATH: "/usr/bin:/bin" },
      });
      expect(output.split("\n")).toEqual([
        "checkpoint", "-w", workspace, "-s", expect.stringMatching(/^cron-\d+$/),
        "-m", "scheduled snapshot", "--metadata", '{"auto":true,"type":"cron"}', "",
      ]);
    } finally {
      fs.rmSync(directory, { recursive: true, force: true });
    }
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

describe("cron line-break guard", () => {
  it("sync fails closed on a newline workspace without touching the crontab", async () => {
    const ok = await CrontabManager.sync(
      "/tmp/legit\n* * * * * touch /tmp/pwned",
      ["0 * * * *"],
    );
    expect(ok).toBe(false);
    expect(mockRunCrontab).not.toHaveBeenCalled();
  });

  it("sync fails closed on a newline schedule without touching the crontab", async () => {
    const ok = await CrontabManager.sync("/ws", ["*\n* * * * *"]);
    expect(ok).toBe(false);
    expect(mockRunCrontab).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// withLock ownership (appended)
// ---------------------------------------------------------------------------

const mockRmdirSync = rmdirSync as ReturnType<typeof vi.fn>;

describe("withLock ownership", () => {
  it("the owner removes the lock after the operation", async () => {
    mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });

    expect(await CrontabManager.sync("/ws", [])).toBe(true);
    expect(mockRmdirSync).toHaveBeenCalledTimes(1);
  });

  it("a timed-out caller must not evict the current holder's lock", async () => {
    vi.useFakeTimers();
    try {
      // The lock directory exists and its owner never releases it.
      mockMkdirSync.mockImplementation(() => {
        const e = new Error("EEXIST") as NodeJS.ErrnoException;
        e.code = "EEXIST";
        throw e;
      });
      mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });

      const pending = CrontabManager.sync("/ws", []);
      await vi.advanceTimersByTimeAsync(5100);
      // A contender must never read or rewrite the crontab without ownership.
      expect(await pending).toBe(false);
      expect(mockRunCrontab).not.toHaveBeenCalled();
      // But the caller never owned the lock, so it must not remove it —
      // doing so would admit a third writer while the holder still runs.
      expect(mockRmdirSync).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a caller blocked by a non-contention error also leaves the lock alone", async () => {
    vi.useFakeTimers();
    try {
      mockMkdirSync.mockImplementation(() => {
        const e = new Error("EACCES") as NodeJS.ErrnoException;
        e.code = "EACCES";
        throw e;
      });
      mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });

      const pending = CrontabManager.sync("/ws", []);
      await vi.advanceTimersByTimeAsync(5100);
      expect(await pending).toBe(false);
      expect(mockRunCrontab).not.toHaveBeenCalled();
      expect(mockMkdirSync).toHaveBeenCalledTimes(1);
      expect(mockRmdirSync).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("withLock recovery", () => {
  it("waits for the owner to release before touching the crontab", async () => {
    vi.useFakeTimers();
    try {
      mockMkdirSync.mockImplementationOnce(() => {
        throw Object.assign(new Error("busy"), { code: "EEXIST" });
      });
      mockRunCrontab.mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });
      const pending = CrontabManager.sync("/ws", []);
      expect(mockRunCrontab).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(50);
      expect(await pending).toBe(true);
      expect(mockRunCrontab).toHaveBeenCalledTimes(2);
      expect(mockRmdirSync).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("releases its own lock when the operation throws", async () => {
    mockRunCrontab.mockRejectedValueOnce(new Error("spawn failed"));
    await expect(CrontabManager.sync("/ws", [])).rejects.toThrow(
      "spawn failed",
    );
    expect(mockRmdirSync).toHaveBeenCalledTimes(1);
  });
});
