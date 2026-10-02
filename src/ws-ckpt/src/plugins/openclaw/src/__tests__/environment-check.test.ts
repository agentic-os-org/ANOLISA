import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock child_process the same way as commands.test.ts
vi.mock("child_process", () => {
  const sym = Symbol.for("nodejs.util.promisify.custom");
  const promisifiedFn = vi.fn();
  const fn = vi.fn();
  (fn as any)[sym] = promisifiedFn;
  return { execFile: fn };
});

import { execFile } from "child_process";
import {
  EnvironmentChecker,
  type EnvironmentCheckResult,
} from "../environment-check.js";

const promisifiedMock = (execFile as any)[
  Symbol.for("nodejs.util.promisify.custom")
] as ReturnType<typeof vi.fn>;

describe("EnvironmentChecker", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe("check()", () => {
    it("returns passed when CLI and daemon both available", async () => {
      promisifiedMock.mockResolvedValue({ stdout: "ok", stderr: "" });
      const checker = new EnvironmentChecker();
      const r = await checker.check();
      expect(r.passed).toBe(true);
      expect(r.cliAvailable).toBe(true);
      expect(r.daemonRunning).toBe(true);
      expect(r.errors).toHaveLength(0);
    });

    it("returns failed when CLI not found", async () => {
      promisifiedMock.mockRejectedValue(new Error("not found"));
      const checker = new EnvironmentChecker();
      const r = await checker.check();
      expect(r.passed).toBe(false);
      expect(r.cliAvailable).toBe(false);
      expect(r.errors.length).toBeGreaterThan(0);
      expect(r.errors[0]).toContain("CLI not found");
    });

    it("returns failed when CLI available but daemon not running", async () => {
      promisifiedMock
        .mockResolvedValueOnce({ stdout: "/usr/bin/ws-ckpt", stderr: "" }) // which
        .mockRejectedValueOnce(new Error("connection refused")); // status
      const checker = new EnvironmentChecker();
      const r = await checker.check();
      expect(r.passed).toBe(false);
      expect(r.cliAvailable).toBe(true);
      expect(r.daemonRunning).toBe(false);
      expect(r.errors).toHaveLength(1);
      expect(r.errors[0]).toContain("daemon");
    });

    it("does not check daemon when CLI is missing", async () => {
      promisifiedMock.mockRejectedValue(new Error("ENOENT"));
      const checker = new EnvironmentChecker();
      const r = await checker.check();
      expect(r.daemonRunning).toBe(false);
      // Only one call (the which check), no daemon check
      expect(promisifiedMock).toHaveBeenCalledTimes(1);
    });
  });

  describe("generateReport", () => {
    it("generates PASSED report", () => {
      const checker = new EnvironmentChecker();
      const result: EnvironmentCheckResult = {
        passed: true,
        cliAvailable: true,
        daemonRunning: true,
        errors: [],
        warnings: [],
      };
      const report = checker.generateReport(result);
      expect(report).toContain("PASSED");
      expect(report).toContain("OK");
    });

    it("generates FAILED report with errors", () => {
      const checker = new EnvironmentChecker();
      const result: EnvironmentCheckResult = {
        passed: false,
        cliAvailable: false,
        daemonRunning: false,
        errors: ["CLI not found"],
        warnings: [],
      };
      const report = checker.generateReport(result);
      expect(report).toContain("FAILED");
      expect(report).toContain("NOT FOUND");
      expect(report).toContain("CLI not found");
    });

    it("includes warnings section", () => {
      const checker = new EnvironmentChecker();
      const result: EnvironmentCheckResult = {
        passed: true,
        cliAvailable: true,
        daemonRunning: true,
        errors: [],
        warnings: ["some warning"],
      };
      const report = checker.generateReport(result);
      expect(report).toContain("Warnings:");
      expect(report).toContain("some warning");
    });

    it("omits warnings section when empty", () => {
      const checker = new EnvironmentChecker();
      const result: EnvironmentCheckResult = {
        passed: true,
        cliAvailable: true,
        daemonRunning: true,
        errors: [],
        warnings: [],
      };
      const report = checker.generateReport(result);
      expect(report).not.toContain("Warnings:");
    });
  });
});
