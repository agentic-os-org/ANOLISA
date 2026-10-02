/**
 * Core manager for the ws-ckpt plugin.
 *
 * Orchestrates workspace initialization, checkpoint creation, rollback,
 * listing, diff, status queries, and cleanup by delegating to the
 * {@link CommandExecutor} and maintaining a local {@link SnapshotStore} cache.
 */

import { CommandExecutor, extractTiming } from "./commands.js";
import { SnapshotStore } from "./snapshot-store.js";
import type {
  CheckpointResult,
  CleanupResult,
  PluginConfig,
  RollbackResult,
  SnapshotInfo,
  StatusReport,
} from "./types.js";

/**
 * Map CLI stderr to LLM-friendly error messages.
 * These messages are designed to be understood by AI agents.
 */
export function mapErrorToLLMMessage(stderr: string, context?: { id?: string }): string {
  if (stderr.includes('already exists')) {
    return `Snapshot ID '${context?.id ?? 'unknown'}' already exists in this workspace. Use a different ID.`;
  }
  if (stderr.includes('active write') || stderr.includes('write operations')) {
    return 'Workspace has active write operations. Wait a moment and retry.';
  }
  if (stderr.includes('Insufficient disk space') || stderr.includes('insufficient')) {
    return 'Insufficient disk space for snapshot. Delete old snapshots to free space.';
  }
  // daemon flattens anyhow chains via `format!("{:#}", e)`, so inner io errors leak generic substrings.
  if (stderr.includes('cwd scan failed')) {
    return 'ws-ckpt could not scan /proc to verify workspace occupants ' +
      '(typically a transient /proc/canonicalize race). ' +
      'This is retryable — wait a moment and try again.';
  }
  if (stderr.includes('have cwd inside workspace')) {
    return 'Other processes have their working directory inside the workspace. ' +
      'ws-ckpt cannot proceed because the symlink swap would break those processes. ' +
      'This is NOT retryable. The user must move affected processes out of the workspace.';
  }
  if (stderr.includes('daemon is not running') || stderr.includes('daemon is starting up')) {
    return 'ws-ckpt daemon is not responding. Is it running?';
  }
  if (stderr.includes('not found') && stderr.toLowerCase().includes('snapshot')) {
    return `Snapshot '${context?.id ?? 'unknown'}' not found. Use ws-ckpt-list to view available snapshots.`;
  }
  if (stderr.includes('not found') && stderr.toLowerCase().includes('workspace')) {
    return 'Workspace not found. Use ws-ckpt-init to initialize first.';
  }
  // Default: return original stderr cleaned of ANSI codes
  return stderr.replace(/\x1b\[[0-9;]*m/g, '').trim();
}

/**
 * BtrfsManager is the high-level API consumed by the plugin entry point.
 *
 * It tracks message / step counters internally so callers (hooks, tools)
 * do not need to manage sequencing themselves.
 */
export class BtrfsManager {
  private executor: CommandExecutor;
  private store: SnapshotStore;
  private config: PluginConfig;

  /** Current workspace path (set during {@link initialize}). */
  private workspacePath: string | null = null;

  /**
   * Create a new BtrfsManager.
   *
   * @param config - Resolved plugin configuration.
   */
  constructor(config: PluginConfig) {
    this.config = config;
    this.executor = new CommandExecutor();
    this.store = new SnapshotStore();
  }

  // -----------------------------------------------------------------------
  // Lifecycle
  // -----------------------------------------------------------------------

  /**
   * Ensure the workspace is initialized.
   *
   * Checks whether the workspace is already managed by ws-ckpt
   * (via `ws-ckpt status`). If yes, just stores the path. If not,
   * runs `ws-ckpt init`.
   *
   * @param workspacePath - Absolute path to the workspace directory.
   * @returns `true` if the workspace is ready, `false` otherwise.
   */
  public async ensureWorkspace(workspacePath: string): Promise<boolean> {
    // Check if already initialized
    const status = await this.executor.status(workspacePath);
    if (status.exitCode === 0) {
      this.workspacePath = workspacePath;
      // Fill store even if workspace already exists
      await this.refreshSnapshotCache();
      return true;
    }
    // Not yet initialized — run init
    return this.initialize(workspacePath);
  }

  public updateConfig(config: PluginConfig): void {
    this.config = config;
  }
  
  /**
   *
   * This must be called before any other operation. If the workspace is
   * already managed by ws-ckpt, the init command is idempotent.
   *
   * @param workspacePath - Absolute path to the workspace directory.
   * @returns `true` if initialization succeeded, `false` otherwise.
   */
  public async initialize(workspacePath: string): Promise<boolean> {
    const output = await this.executor.init(workspacePath);
    if (output.exitCode !== 0) {
      // Already initialized is not an error — just use it
      if (output.stderr.includes("AlreadyInitialized") || output.stderr.includes("already initialized")) {
        this.workspacePath = workspacePath;
        await this.refreshSnapshotCache();
        return true;
      }
      console.error(
        `[ws-ckpt] Failed to initialize workspace: ${output.stderr}`,
      );
      return false;
    }

    this.workspacePath = workspacePath;
    console.log(`[ws-ckpt] Workspace initialized: ${workspacePath}`);

    // Refresh the snapshot cache after init
    await this.refreshSnapshotCache();
    return true;
  }

  // -----------------------------------------------------------------------
  // Checkpoint / Rollback / List (exposed as tools)
  // -----------------------------------------------------------------------

  /**
   * Create a checkpoint of the current workspace state.
   *
   * The daemon assigns a hash-based snapshot ID and returns it in
   * `CheckpointOk { snapshot_id }`. The plugin forwards message
   * and metadata.
   *
   * @param options - Checkpoint options: message, metadata JSON string.
   * @returns A {@link CheckpointResult} describing the outcome.
   */
  public async createCheckpoint(options?: {
    id?: string;
    message?: string;
    metadata?: string;
  }): Promise<CheckpointResult> {
    if (!this.workspacePath) {
      return { success: false, message: "Workspace not initialized" };
    }

    try {
      const output = await this.executor.checkpoint(
        this.workspacePath,
        options?.id ?? `snap-${Date.now()}`,
        {
          message: options?.message,
          metadata: options?.metadata,
        },
      );

      if (output.exitCode !== 0) {
        return {
          success: false,
          message: mapErrorToLLMMessage(output.stderr, { id: options?.id }),
        };
      }

      // The CLI reports CheckpointSkipped on stderr while keeping exit code 0.
      if (`${output.stdout}\n${output.stderr}`.includes("Empty workspace, no snapshot created.")) {
        return { success: true, skipped: true, reason: 'Empty workspace, no snapshot created.', message: 'Empty workspace, no snapshot created.' };
      }

      // Use the caller-supplied ID directly — CLI stdout may contain
      // ANSI codes / prompt text that breaks parseSnapshotIdFromOutput.
      const snapshotId = options?.id ?? `snap-${Date.now()}`;

      // Update local cache
      let parsedMetadata: Record<string, unknown> | undefined;
      if (options?.metadata) {
        try { parsedMetadata = JSON.parse(options.metadata); } catch { /* ignore */ }
      }
      const info: SnapshotInfo = {
        snapshot: snapshotId,
        message: options?.message,
        metadata: parsedMetadata,
        createdAt: new Date().toISOString(),
      };
      this.store.add(info);

      return {
        success: true,
        snapshot: snapshotId,
        message: `Checkpoint created: ${snapshotId}${extractTiming(output.stdout)}`,
      };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return { success: false, message: `Checkpoint error: ${msg}` };
    }
  }

  /**
   * Roll back the workspace to a specified checkpoint or N ancestors back.
   *
   * @param target       - Snapshot identifier (mutually exclusive with numAncestors).
   * @param numAncestors - Number of ancestors to traverse (mutually exclusive with target).
   * @param preview      - Show changes without executing the rollback.
   * @returns A {@link RollbackResult} describing the outcome.
   */
  public async rollback(
    target?: string,
    numAncestors?: number,
    preview: boolean = false,
  ): Promise<RollbackResult> {
    if (!this.workspacePath) {
      return { success: false, message: "Workspace not initialized" };
    }

    const label = target || `ancestors=${numAncestors}`;
    try {
      const output = await this.executor.rollback(
        this.workspacePath,
        target,
        numAncestors,
        preview,
      );

      if (output.exitCode !== 0) {
        return {
          success: false,
          target,
          message: mapErrorToLLMMessage(output.stderr, { id: label }),
        };
      }

      if (preview) {
        return {
          success: true,
          target,
          message: output.stdout.replace(/\x1b\[[0-9;]*m/g, "").trim(),
        };
      }

      // Rollback changes the workspace state; refresh snapshot cache from daemon
      try {
        const listOutput = await this.executor.list(this.workspacePath, "json");
        if (listOutput.exitCode === 0) {
          const parsed = this.parseSnapshotList(listOutput.stdout);
          this.store.setAll(parsed);
        }
      } catch { /* ignore refresh errors */ }

      const desc = target ? `Rolled back to ${target}` : `Rolled back ${numAncestors} ancestor(s)`;
      return { success: true, target, message: `${desc}${extractTiming(output.stdout)}` };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return { success: false, target, message: `Rollback error: ${msg}` };
    }
  }

  /**
   * List all checkpoints for the current workspace.
   *
   * @returns An array of {@link SnapshotInfo} objects.
   */
  public async listCheckpoints(): Promise<SnapshotInfo[]> {
    if (!this.workspacePath) {
      return [];
    }

    try {
      const output = await this.executor.list(this.workspacePath, "json");

      if (output.exitCode !== 0) {
        throw new Error(
          `Failed to list checkpoints: ${mapErrorToLLMMessage(output.stderr)}`,
        );
      }

      const parsed = this.parseSnapshotList(output.stdout);
      this.store.setAll(parsed);
      return this.store.getAll();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`[ws-ckpt] List error: ${message}`);
      if (message.startsWith("Failed to list checkpoints:")) {
        throw error;
      }
      throw new Error(`Failed to list checkpoints: ${message}`);
    }
  }

  // -----------------------------------------------------------------------
  // Phase 2 extensions
  // -----------------------------------------------------------------------

  /**
   * Execute diff and return the raw CLI output without parsing.
   */
  public async execDiffRaw(from: string, to?: string): Promise<{ success: boolean; text: string }> {
    if (!this.workspacePath) {
      return { success: false, text: "Workspace not initialized" };
    }
    try {
      const output = await this.executor.diff(this.workspacePath, from, to);
      if (output.exitCode !== 0) {
        return { success: false, text: mapErrorToLLMMessage(output.stderr) };
      }
      const stdout = output.stdout.replace(/\x1b\[[0-9;]*m/g, '').trim();
      const target = to ?? "current workspace";
      return { success: true, text: stdout || `No changes between ${from} and ${target}.` };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return { success: false, text: `Diff error: ${msg}` };
    }
  }

  /**
   * Check whether there are file changes between two snapshots.
   *
   * Fail-closed: when the diff cannot be determined (diff command fails,
   * daemon error, exception), assume changes exist so callers that gate
   * checkpoint creation on this do not silently skip a checkpoint.
   *
   * @returns `true` if changes exist or cannot be determined, `false` only
   *          when the diff was successfully produced and shows no changes.
   */
  public async hasChanges(from: string, to: string): Promise<boolean> {
    if (!this.workspacePath) return false;
    try {
      const output = await this.executor.diff(this.workspacePath, from, to);
      if (output.exitCode !== 0) return true;
      const stdout = output.stdout.replace(/\x1b\[[0-9;]*m/g, '').trim();
      // CLI outputs "No differences found." when identical
      return stdout.length > 0 && !stdout.startsWith("No differences");
    } catch {
      return true;
    }
  }

  /**
   * Query the daemon and workspace status.
   *
   * @returns A {@link StatusReport}.
   */
  public async getStatus(): Promise<StatusReport> {
    try {
      const output = await this.executor.status(this.workspacePath ?? undefined);

      if (output.exitCode !== 0) {
        return {
          success: false,
          daemonRunning: false,
          message: mapErrorToLLMMessage(output.stderr),
        };
      }

      return {
        success: true,
        daemonRunning: true,
        message: output.stdout,
      };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return {
        success: false,
        daemonRunning: false,
        message: `Status error: ${msg}`,
      };
    }
  }

  /**
   * Clean up old snapshots, keeping the most recent N.
   *
   * @param keep - Number of snapshots to keep (defaults to 20 if unset).
   * @returns A {@link CleanupResult}.
   */
  public async cleanup(keep?: number): Promise<CleanupResult> {
    if (!this.workspacePath) {
      return { success: false, removedCount: 0, remainingCount: 0, message: "Workspace not initialized" };
    }

    const keepCount = keep ?? 20;

    try {
      const output = await this.executor.cleanup(this.workspacePath, keepCount);

      if (output.exitCode !== 0) {
        return {
          success: false,
          removedCount: 0,
          remainingCount: this.store.count,
          message: mapErrorToLLMMessage(output.stderr),
        };
      }

      // Refresh cache after cleanup
      await this.refreshSnapshotCache();

      return {
        success: true,
        removedCount: 0, // Exact count would come from CLI output parsing
        remainingCount: this.store.count,
        message: output.stdout || `Cleanup completed, keeping ${keepCount} snapshots`,
      };
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      return {
        success: false,
        removedCount: 0,
        remainingCount: this.store.count,
        message: `Cleanup error: ${msg}`,
      };
    }
  }

  // -----------------------------------------------------------------------
  // Accessors
  // -----------------------------------------------------------------------

  /**
   * Return the current workspace path, or `null` if not initialized.
   */
  public getWorkspacePath(): string | null {
    return this.workspacePath;
  }

  /**
   * Return the internal snapshot store (for testing or advanced usage).
   */
  public getStore(): SnapshotStore {
    return this.store;
  }

  // -----------------------------------------------------------------------
  // Private helpers
  // -----------------------------------------------------------------------

  /**
   * Refresh the local snapshot cache from the CLI.
   */
  private async refreshSnapshotCache(): Promise<void> {
    if (!this.workspacePath) return;

    try {
      const output = await this.executor.list(this.workspacePath, "json");
      if (output.exitCode === 0) {
        const parsed = this.parseSnapshotList(output.stdout);
        this.store.setAll(parsed);
      }
    } catch {
      // Silently ignore — cache may be stale but that's acceptable
    }
  }

  /**
   * Parse the JSON output of `ws-ckpt list --format json`.
   *
   * Expected format: a JSON array of snapshot objects.
   */
  private parseSnapshotList(stdout: string): SnapshotInfo[] {
    if (!stdout.trim()) return [];

    try {
      const data = JSON.parse(stdout);
      if (Array.isArray(data)) {
        return data.map((item: Record<string, unknown>) => {
          // Fields live under item.meta in the current CLI format;
          // fall back to top-level for backward compatibility.
          const meta = (item.meta ?? {}) as Record<string, unknown>;
          return {
            snapshot: String(item.snapshot ?? item.id ?? ""),
            message: (meta.message ?? item.message) ? String(meta.message ?? item.message) : undefined,
            metadata: (meta.metadata ?? item.metadata) as Record<string, unknown> | undefined,
            createdAt: String(
              meta.created_at ?? meta.createdAt
              ?? item.created_at ?? item.createdAt
              ?? new Date().toISOString(),
            ),
            detail: item.detail === "summary" ? "summary" : "full",
            omittedFields: Array.isArray(item.omitted_fields)
              ? item.omitted_fields.map(String)
              : undefined,
          };
        });
      }
      return [];
    } catch {
      console.warn(`[ws-ckpt] Failed to parse snapshot list output: ${stdout.substring(0, 200)}`);
      return [];
    }
  }

}
