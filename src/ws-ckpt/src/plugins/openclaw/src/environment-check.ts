/**
 * Environment checker for the ws-ckpt plugin.
 *
 * Verifies that the runtime environment meets the requirements:
 * - ws-ckpt CLI binary is installed and on PATH
 * - ws-ckpt daemon is running (via `ws-ckpt status`)
 */

import { execFile } from "child_process";
import { promisify } from "util";

const execFileAsync = promisify(execFile);

/** Result of an environment check. */
export interface EnvironmentCheckResult {
  /** Whether all critical checks passed. */
  passed: boolean;
  /** Whether the ws-ckpt CLI binary is available. */
  cliAvailable: boolean;
  /** Whether the daemon is running. */
  daemonRunning: boolean;
  /** Critical errors that prevent plugin operation. */
  errors: string[];
  /** Non-critical warnings. */
  warnings: string[];
}

/**
 * Checks the runtime environment for ws-ckpt availability.
 *
 * The checker does not throw on failure — it returns a structured result
 * so the plugin can decide whether to operate in degraded mode.
 */
export class EnvironmentChecker {
  constructor() {
    // No config needed — checks are performed via CLI
  }

  /**
   * Run all environment checks and return a combined result.
   */
  public async check(): Promise<EnvironmentCheckResult> {
    const result: EnvironmentCheckResult = {
      passed: false,
      cliAvailable: false,
      daemonRunning: false,
      errors: [],
      warnings: [],
    };

    // 1. Check ws-ckpt CLI availability
    result.cliAvailable = await this.checkCli();
    if (!result.cliAvailable) {
      result.errors.push(
        "ws-ckpt CLI not found. Ensure ws-ckpt is installed and on PATH.",
      );
      // Cannot proceed with daemon health check without CLI
      return result;
    }

    // 2. Check daemon health via CLI
    result.daemonRunning = await this.checkDaemonHealth();

    if (!result.daemonRunning) {
      result.errors.push(
        "ws-ckpt daemon is not running. Ensure the daemon is running (systemctl status ws-ckpt).",
      );
    }

    // Overall pass requires CLI + daemon
    result.passed = result.cliAvailable && result.daemonRunning;

    return result;
  }

  /**
   * Generate a human-readable report from a check result.
   */
  public generateReport(result: EnvironmentCheckResult): string {
    const lines: string[] = [];

    lines.push("=== ws-ckpt Environment Check ===");
    lines.push("");
    lines.push(result.passed ? "Status: PASSED" : "Status: FAILED");
    lines.push("");
    lines.push(`  ws-ckpt CLI:    ${result.cliAvailable ? "OK" : "NOT FOUND"}`);
    lines.push(`  Daemon running:  ${result.daemonRunning ? "OK" : "NOT FOUND"}`);

    if (result.errors.length > 0) {
      lines.push("");
      lines.push("Errors:");
      for (const err of result.errors) {
        lines.push(`  - ${err}`);
      }
    }

    if (result.warnings.length > 0) {
      lines.push("");
      lines.push("Warnings:");
      for (const warn of result.warnings) {
        lines.push(`  - ${warn}`);
      }
    }

    return lines.join("\n");
  }

  /**
   * Check whether the `ws-ckpt` CLI binary is on PATH.
   */
  private async checkCli(): Promise<boolean> {
    try {
      await execFileAsync("which", ["ws-ckpt"], { timeout: 5000 });
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Check daemon health via `ws-ckpt status`.
   *
   * Fail-closed: only exit code 0 is treated as healthy. Any non-zero exit
   * (connection refused, timeout, protocol error, daemon-side failure, …)
   * means the daemon is not usable from the plugin's perspective.
   */
  private async checkDaemonHealth(): Promise<boolean> {
    try {
      await execFileAsync("ws-ckpt", ["status"], {
        timeout: 10000,
        encoding: "utf-8",
      });
      return true;
    } catch {
      return false;
    }
  }
}
