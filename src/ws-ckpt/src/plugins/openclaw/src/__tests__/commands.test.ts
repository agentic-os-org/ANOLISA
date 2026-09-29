import { describe, it, expect, vi, beforeEach } from "vitest";

// We need to mock the promisified execFile that commands.ts uses internally.
// The module does: const execFileAsync = promisify(execFile);
// We mock the entire child_process so execFile's callback-based API works
// with promisify.
// execFile has a custom promisify symbol, so we mock it on that symbol too.
vi.mock("child_process", () => {
  const sym = Symbol.for("nodejs.util.promisify.custom");
  const promisifiedFn = vi.fn();
  const fn = vi.fn();
  (fn as any)[sym] = promisifiedFn;
  return { execFile: fn };
});

import { execFile } from "child_process";
import { CommandExecutor } from "../commands.js";

const promisifiedMock = (execFile as any)[
  Symbol.for("nodejs.util.promisify.custom")
] as ReturnType<typeof vi.fn>;

function mockSuccess(stdout = "", stderr = "") {
  promisifiedMock.mockResolvedValue({ stdout, stderr });
}

function mockFailure(code: number, stderr = "", stdout = "") {
  const err: any = new Error("command failed");
  err.code = code;
  err.stdout = stdout;
  err.stderr = stderr;
  promisifiedMock.mockRejectedValue(err);
}

describe("CommandExecutor", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe("init", () => {
    it("returns success", async () => {
      mockSuccess("initialized");
      const exec = new CommandExecutor();
      const r = await exec.init("/my/ws");
      expect(r.exitCode).toBe(0);
      expect(r.stdout).toBe("initialized");
    });
  });

  describe("checkpoint", () => {
    it("basic call", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.checkpoint("/ws", "snap1", { message: "test" });
      expect(r.exitCode).toBe(0);
    });

    it("with metadata", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      await exec.checkpoint("/ws", "snap1", {
        message: "m",
        metadata: '{"k":"v"}',
      });
      expect(promisifiedMock).toHaveBeenCalled();
    });

    it("no options", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.checkpoint("/ws", "snap1");
      expect(r.exitCode).toBe(0);
    });
  });

  describe("rollback", () => {
    it("uses the cleanup-compatible timeout and returns success", async () => {
      mockSuccess("rolled back");
      const exec = new CommandExecutor();
      const r = await exec.rollback("/ws", "snap1");
      expect(r.exitCode).toBe(0);
      expect(promisifiedMock.mock.calls[0][2].timeout).toBe(240_000);
    });

    it("passes preview flag", async () => {
      mockSuccess("preview");
      const exec = new CommandExecutor();
      await exec.rollback("/ws", "snap1", undefined, true);
      const args = promisifiedMock.mock.calls[0][1];
      expect(args).toContain("--preview");
    });
  });

  describe("delete", () => {
    it("with force and workspace", async () => {
      mockSuccess("deleted");
      const exec = new CommandExecutor();
      const r = await exec.delete("snap1", { workspace: "/ws", force: true });
      expect(r.exitCode).toBe(0);
    });

    it("no options", async () => {
      mockSuccess("deleted");
      const exec = new CommandExecutor();
      const r = await exec.delete("snap1");
      expect(r.exitCode).toBe(0);
    });
  });

  describe("list", () => {
    it("json format", async () => {
      mockSuccess("[]");
      const exec = new CommandExecutor();
      const r = await exec.list("/ws", "json");
      expect(r.exitCode).toBe(0);
    });

    it("table format", async () => {
      mockSuccess("ID  MESSAGE");
      const exec = new CommandExecutor();
      const r = await exec.list("/ws", "table");
      expect(r.exitCode).toBe(0);
    });

    it("default format", async () => {
      mockSuccess("[]");
      const exec = new CommandExecutor();
      const r = await exec.list("/ws");
      expect(r.exitCode).toBe(0);
    });
  });

  describe("diff", () => {
    it("passes from and to", async () => {
      mockSuccess("diff output");
      const exec = new CommandExecutor();
      const r = await exec.diff("/ws", "a", "b");
      expect(r.exitCode).toBe(0);
      expect(r.stdout).toBe("diff output");
    });
  });

  describe("status", () => {
    it("without workspace", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.status();
      expect(r.exitCode).toBe(0);
    });

    it("with workspace", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.status("/ws");
      expect(r.exitCode).toBe(0);
    });
  });

  describe("cleanup", () => {
    it("with keep", async () => {
      mockSuccess("cleaned");
      const exec = new CommandExecutor();
      const r = await exec.cleanup("/ws", 5);
      expect(r.exitCode).toBe(0);
    });

    it("without keep", async () => {
      mockSuccess("cleaned");
      const exec = new CommandExecutor();
      const r = await exec.cleanup("/ws");
      expect(r.exitCode).toBe(0);
    });
  });

  describe("config", () => {
    it("returns error when no workspace", async () => {
      const exec = new CommandExecutor();
      const r = await exec.config(undefined);
      expect(r.exitCode).toBe(2);
      expect(r.stderr).toContain("No workspace");
    });

    it("with explicit workspace", async () => {
      mockSuccess('{"schema":"ws-ckpt-policy/v1"}');
      const exec = new CommandExecutor();
      const r = await exec.config("/ws");
      expect(r.exitCode).toBe(0);
      expect(r.usedWorkspace).toBe("/ws");
    });

    it("reset flag", async () => {
      mockSuccess("reset");
      const exec = new CommandExecutor();
      const r = await exec.config("/ws", { reset: true });
      expect(r.exitCode).toBe(0);
    });

    it("enable-auto-cleanup with keep", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.config("/ws", {
        enableAutoCleanup: true,
        autoCleanupKeep: "10",
      });
      expect(r.exitCode).toBe(0);
    });

    it("disable-auto-cleanup", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.config("/ws", { disableAutoCleanup: true });
      expect(r.exitCode).toBe(0);
    });
  });

  describe("error handling", () => {
    it("returns non-zero exit code", async () => {
      mockFailure(1, "something went wrong");
      const exec = new CommandExecutor();
      const r = await exec.init("/ws");
      expect(r.exitCode).toBe(1);
      expect(r.stderr).toContain("something went wrong");
    });

    it("exit code defaults to 1 when code is string", async () => {
      const err: any = new Error("generic");
      err.code = "ENOENT";
      err.stdout = "";
      err.stderr = "not found";
      promisifiedMock.mockRejectedValue(err);
      const exec = new CommandExecutor();
      const r = await exec.init("/ws");
      expect(r.exitCode).toBe(1);
    });
  });

  describe("rollback — numAncestors", () => {
    it("passes numAncestors as-is when autoCheckpoint is not enabled", async () => {
      mockSuccess("rolled back");
      const exec = new CommandExecutor();
      const r = await exec.rollback("/ws", undefined, 2);
      expect(r.exitCode).toBe(0);
      const args = promisifiedMock.mock.calls[0][1];
      expect(args).toContain("--num-ancestors");
      expect(args).toContain("2");
    });

    it("throws when neither target nor numAncestors", async () => {
      const exec = new CommandExecutor();
      await expect(exec.rollback("/ws")).rejects.toThrow("Either");
    });

    it("uses numAncestors when target is undefined", async () => {
      mockSuccess("ok");
      const exec = new CommandExecutor();
      const r = await exec.rollback("/ws", undefined, 1);
      expect(r.exitCode).toBe(0);
      const args = promisifiedMock.mock.calls[0][1];
      expect(args).toContain("--num-ancestors");
      expect(args).not.toContain("--snapshot");
    });
  });
});

// ---------------------------------------------------------------------------
// runCrontab
// ---------------------------------------------------------------------------

describe("runCrontab", () => {
  // runCrontab uses execFile directly (not the CommandExecutor class),
  // but the same child_process mock applies.

  it("runs crontab -l successfully", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "line1\n", stderr: "" });
    const { runCrontab } = await import("../commands.js");
    const r = await runCrontab(["-l"]);
    expect(r.exitCode).toBe(0);
    expect(r.stdout).toBe("line1\n");
  });

  it("returns error on failure without input", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "permission denied";
    promisifiedMock.mockRejectedValue(err);
    const { runCrontab } = await import("../commands.js");
    const r = await runCrontab(["-l"]);
    expect(r.exitCode).toBe(1);
    expect(r.stderr).toContain("permission denied");
  });

  it("returns exitCode 1 for string error code", async () => {
    const err: any = new Error("fail");
    err.code = "ENOENT";
    err.stderr = "not found";
    promisifiedMock.mockRejectedValue(err);
    const { runCrontab } = await import("../commands.js");
    const r = await runCrontab(["-l"]);
    expect(r.exitCode).toBe(1);
  });

  it("runs with input via temp file", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "", stderr: "" });
    const { runCrontab } = await import("../commands.js");
    const r = await runCrontab(["-"], { input: "line1\n" });
    expect(r.exitCode).toBe(0);
  });

  it("handles error with input", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "error";
    promisifiedMock.mockRejectedValue(err);
    const { runCrontab } = await import("../commands.js");
    const r = await runCrontab(["-"], { input: "line1\n" });
    expect(r.exitCode).toBe(1);
  });
});
