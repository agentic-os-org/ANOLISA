/**
 * Command executor for the ws-ckpt plugin.
 *
 * Wraps all `ws-ckpt` CLI invocations using `child_process.execFile`.
 * Each method constructs the appropriate CLI arguments, executes the
 * command, and returns a typed {@link CommandOutput}.
 */

import { execFile } from "child_process";
import { writeFileSync, unlinkSync, mkdtempSync, rmdirSync } from "fs";
import { join } from "path";
import { tmpdir } from "os";
import { promisify } from "util";
import type { CommandOutput } from "./types.js";
import { pluginState } from "./state.js";

const execFileAsync = promisify(execFile);

/** Allows the daemon's 180 s bounded reclamation plus RPC overhead. */
const DEFAULT_TIMEOUT_MS = 240_000;

/** The ws-ckpt CLI binary name. */
const WS_CKPT_BIN = "ws-ckpt";

/**
 * Executes ws-ckpt CLI commands and returns structured output.
 *
 * All methods are async and resolve with a {@link CommandOutput} containing
 * the exit code, stdout, and stderr of the CLI invocation.
 */
export class CommandExecutor {
  private timeoutMs: number;

  /**
   * Create a new CommandExecutor.
   *
   * @param timeoutMs - Timeout for each CLI invocation (default 240 s).
   */
  constructor(timeoutMs: number = DEFAULT_TIMEOUT_MS) {
    this.timeoutMs = timeoutMs;
  }

  // -----------------------------------------------------------------------
  // Phase 1 commands
  // -----------------------------------------------------------------------

  /**
   * Initialize a workspace for ws-ckpt management.
   *
   * Equivalent to: `ws-ckpt init --workspace <workspace>`
   *
   * @param workspace - Workspace directory path.
   */
  public async init(workspace: string): Promise<CommandOutput> {
    return this.run(["init", "--workspace", workspace]);
  }

  /**
   * Create a checkpoint (snapshot) of the workspace.
   *
   * Equivalent to:
   * ```
   * ws-ckpt checkpoint --workspace <ws> --snapshot <id> [--message <msg>] [--metadata <json>]
   * ```
   *
   * @param workspace - Workspace directory path.
   * @param id        - Caller-provided snapshot identifier.
   * @param options   - Optional message and metadata.
   */
  public async checkpoint(
    workspace: string,
    id: string,
    options?: { message?: string; metadata?: string },
  ): Promise<CommandOutput> {
    const args = [
      "checkpoint",
      "--workspace", workspace,
      "--snapshot", id,
    ];

    if (options?.message) {
      args.push("--message", options.message);
    }

    if (options?.metadata) {
      args.push("--metadata", options.metadata);
    }

    return this.run(args);
  }

  /**
   * Roll back the workspace to a specific snapshot or N ancestors back.
   *
   * @param workspace     - Workspace directory path.
   * @param target        - Snapshot identifier (mutually exclusive with numAncestors).
   * @param numAncestors  - Number of ancestors to traverse (mutually exclusive with target).
   * @param preview       - Show changes without executing the rollback.
   */
  public async rollback(
    workspace: string,
    target?: string,
    numAncestors?: number,
    preview: boolean = false,
  ): Promise<CommandOutput> {
    if (!target && numAncestors === undefined) {
      throw new Error("Either 'target' or 'numAncestors' is required");
    }
    const args = ["rollback", "--workspace", workspace];
    if (numAncestors !== undefined) {
      args.push("--num-ancestors", String(numAncestors));
    } else if (target) {
      args.push("--snapshot", target);
    }
    if (preview) {
      args.push("--preview");
    }
    return this.run(args);
  }

  /**
   * Delete a specific snapshot.
   *
   * Equivalent to: `ws-ckpt delete [--workspace <ws>] --snapshot <id> [--force]`
   *
   * @param snapshot  - Snapshot ID to delete.
   * @param options   - Optional workspace and force flag.
   */
  public async delete(
    snapshot: string,
    options?: { workspace?: string; force?: boolean },
  ): Promise<CommandOutput> {
    const args = ["delete"];

    if (options?.workspace) {
      args.push("--workspace", options.workspace);
    }

    args.push("--snapshot", snapshot);

    if (options?.force) {
      args.push("--force");
    }

    return this.run(args);
  }

  // -----------------------------------------------------------------------
  // Phase 2 commands
  // -----------------------------------------------------------------------

  /**
   * List all snapshots for a workspace.
   *
   * Equivalent to: `ws-ckpt list --workspace <ws> [--format <fmt>]`
   *
   * @param workspace - Workspace directory path.
   * @param format    - Output format: "table" or "json" (default "json").
   */
  public async list(workspace: string, format: "table" | "json" = "json"): Promise<CommandOutput> {
    return this.run(["list", "--workspace", workspace, "--format", format]);
  }

  /**
   * Show the diff between two snapshots, or between a snapshot and the current workspace.
   *
   * Equivalent to: `ws-ckpt diff --workspace <ws> --from <a> [--to <b>]`
   *
   * @param workspace - Workspace directory path.
   * @param from      - Source snapshot identifier or name.
   * @param to        - Target snapshot identifier or name. Omit to diff against current workspace.
   */
  public async diff(workspace: string, from: string, to?: string): Promise<CommandOutput> {
    const args = ["diff", "--workspace", workspace, "--from", from];
    if (to) {
      args.push("--to", to);
    }
    return this.run(args);
  }

  /**
   * Query daemon and/or workspace status.
   *
   * Equivalent to: `ws-ckpt status [--workspace <ws>]`
   *
   * @param workspace - Optional workspace path for workspace-specific status.
   */
  public async status(workspace?: string): Promise<CommandOutput> {
    const args = ["status"];
    if (workspace) {
      args.push("--workspace", workspace);
    }
    return this.run(args);
  }

  /**
   * Clean up old snapshots, keeping the most recent N.
   *
   * Equivalent to: `ws-ckpt cleanup --workspace <ws> [--keep <N>]`
   *
   * @param workspace - Workspace directory path.
   * @param keep      - Number of snapshots to keep.
   */
  public async cleanup(workspace: string, keep?: number): Promise<CommandOutput> {
    const args = ["cleanup", "--workspace", workspace];
    if (keep !== undefined) {
      args.push("--keep", String(keep));
    }
    return this.run(args);
  }

  /**
   * View or update **per-workspace** auto-cleanup config.
   * Maps to `ws-ckpt config -w <workspace> --format json [flags]`.
   *
   * Always uses `--format json`: text output isn't a contract and grepping
   * it conflated regex-miss / real-disabled / Count(0). The JSON shape is
   * versioned, so parse failures stay distinct from a real "disabled" state.
   *
   * Workspace resolution (mirrors `checkpoint` etc.): explicit `workspace`
   * arg → `pluginState.resolvedConfig.workspace` → else error (no silent
   * `-g` fallback; plugin ops are per-workspace by design). The result's
   * `usedWorkspace` names the ws actually changed.
   */
  public async config(
    workspace?: string,
    options?: {
      enableAutoCleanup?: boolean;
      disableAutoCleanup?: boolean;
      autoCleanupKeep?: string;
      reset?: boolean;
    },
  ): Promise<CommandOutput & { usedWorkspace?: string }> {
    const ws = workspace ?? pluginState.resolvedConfig?.workspace;
    if (!ws) {
      return {
        exitCode: 2,
        stdout: "",
        stderr:
          "No workspace specified: pass workspace explicitly or set plugins.entries.ws-ckpt.config.workspace.",
      };
    }
    const args = ["config", "-w", ws, "--format", "json"];
    if (options?.reset) {
      args.push("--reset");
    } else {
      if (options?.enableAutoCleanup) args.push("--enable-auto-cleanup");
      if (options?.disableAutoCleanup) args.push("--disable-auto-cleanup");
      if (options?.autoCleanupKeep !== undefined) {
        args.push("--auto-cleanup-keep", options.autoCleanupKeep);
      }
    }
    const out = await this.run(args);
    return { ...out, usedWorkspace: ws };
  }

  // -----------------------------------------------------------------------
  // Internal
  // -----------------------------------------------------------------------

  /**
   * Execute a ws-ckpt CLI command and return structured output.
   *
   * @param args - CLI arguments (excluding the binary name).
   * @returns A {@link CommandOutput} with exit code, stdout, and stderr.
   */
  private async run(args: string[]): Promise<CommandOutput> {
    try {
      const { stdout, stderr } = await execFileAsync(WS_CKPT_BIN, args, {
        timeout: this.timeoutMs,
        encoding: "utf-8",
        env: { ...process.env, WS_CKPT_AGENT_NAME: "openclaw" },
      });

      return {
        exitCode: 0,
        stdout: stdout ?? "",
        stderr: stderr ?? "",
      };
    } catch (error: unknown) {
      // execFile rejects with an error that may contain exit code and output.
      const err = error as {
        code?: number | string;
        stdout?: string;
        stderr?: string;
        message?: string;
      };

      return {
        exitCode: typeof err.code === "number" ? err.code : 1,
        stdout: err.stdout ?? "",
        stderr: err.stderr ?? err.message ?? "Unknown command error",
      };
    }
  }

}

export function extractTiming(stdout: string): string {
  const match = stdout.match(/^(Completed in .+|Failed after .+)$/m);
  return match ? ` (${match[1]})` : "";
}

/**
 * Execute a crontab command. Returns structured output, never throws.
 * When input is provided, writes to a temp file and runs `crontab <file>`
 * instead of stdin — execFile does not support the input option.
 */
export async function runCrontab(
  args: string[],
  opts?: { input?: string; timeout?: number },
): Promise<CommandOutput> {
  const timeout = opts?.timeout ?? 10_000;

  if (opts?.input !== undefined) {
    const tmpDir = mkdtempSync(join(tmpdir(), "ws-ckpt-cron-"));
    const tmpFile = join(tmpDir, "crontab");
    try {
      writeFileSync(tmpFile, opts.input, "utf-8");
      const { stdout, stderr } = await execFileAsync("crontab", [tmpFile], {
        timeout,
        encoding: "utf-8",
      });
      return { exitCode: 0, stdout: String(stdout ?? ""), stderr: String(stderr ?? "") };
    } catch (error: unknown) {
      const err = error as {
        code?: number | string;
        stdout?: string;
        stderr?: string;
        message?: string;
      };
      return {
        exitCode: typeof err.code === "number" ? err.code : 1,
        stdout: err.stdout ?? "",
        stderr: err.stderr ?? err.message ?? "Unknown command error",
      };
    } finally {
      try { unlinkSync(tmpFile); rmdirSync(tmpDir); } catch { /* cleanup best-effort */ }
    }
  }

  try {
    const { stdout, stderr } = await execFileAsync("crontab", args, {
      timeout,
      encoding: "utf-8",
    });
    return { exitCode: 0, stdout: String(stdout ?? ""), stderr: String(stderr ?? "") };
  } catch (error: unknown) {
    const err = error as {
      code?: number | string;
      stdout?: string;
      stderr?: string;
      message?: string;
    };
    return {
      exitCode: typeof err.code === "number" ? err.code : 1,
      stdout: err.stdout ?? "",
      stderr: err.stderr ?? err.message ?? "Unknown command error",
    };
  }
}
