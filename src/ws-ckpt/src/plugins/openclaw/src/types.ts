/**
 * Core type definitions for the ws-ckpt OpenClaw Plugin.
 *
 * Covers plugin configuration, snapshot metadata, command results,
 * and Phase 2 extensions (diff, status, cleanup).
 */

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

/** Information about a single btrfs snapshot. */
export interface SnapshotInfo {
  /** Snapshot identifier — daemon-assigned hash ID. */
  snapshot: string;
  /** Commit message associated with the snapshot. */
  message?: string;
  /** Additional metadata JSON. */
  metadata?: Record<string, unknown>;
  /** ISO 8601 creation timestamp. */
  createdAt: string;
  /** Detail level returned by ws-ckpt. Summary entries omit large optional fields. */
  detail?: "full" | "summary";
  /** Fields omitted when detail is "summary". */
  omittedFields?: string[];
}

/** Result of a checkpoint operation. */
export interface CheckpointResult {
  /** Whether the operation succeeded. */
  success: boolean;
  /** Snapshot identifier created (daemon-assigned hash ID). */
  snapshot?: string;
  /** Whether the checkpoint was skipped (e.g. empty workspace). */
  skipped?: boolean;
  /** Reason for skipping (when skipped is true). */
  reason?: string;
  /** Human-readable message. */
  message: string;
}

/** Result of a rollback operation. */
export interface RollbackResult {
  /** Whether the operation succeeded. */
  success: boolean;
  /** The snapshot that was rolled back to. */
  target?: string;
  /** Human-readable message. */
  message: string;
}

// ---------------------------------------------------------------------------
// Phase 2 types
// ---------------------------------------------------------------------------

/** Workspace and daemon status report. */
export interface StatusReport {
  /** Whether the operation succeeded. */
  success: boolean;
  /** Whether the daemon is running. */
  daemonRunning: boolean;
  /** Btrfs filesystem health information. */
  filesystemInfo?: {
    /** Total space in bytes. */
    totalBytes?: number;
    /** Used space in bytes. */
    usedBytes?: number;
    /** Usage percentage. */
    usagePercent?: number;
  };
  /** Per-workspace status. */
  workspace?: {
    /** Workspace path. */
    path: string;
    /** Number of snapshots. */
    snapshotCount: number;
    /** Last snapshot identifier. */
    lastSnapshot?: string;
  };
  /** Human-readable message (raw CLI output). */
  message: string;
}

/** Result of a cleanup operation. */
export interface CleanupResult {
  /** Whether the operation succeeded. */
  success: boolean;
  /** Number of snapshots removed. */
  removedCount: number;
  /** Number of snapshots remaining. */
  remainingCount: number;
  /** Human-readable message. */
  message: string;
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/** Plugin configuration interface. */
export interface PluginConfig {
  /** Workspace path for snapshot operations. */
  workspace: string;
  /** Whether to automatically create a checkpoint at end of each turn. */
  autoCheckpoint: boolean;
  /** Cron expressions for scheduled snapshots. */
  cronSchedules?: string[];
}

// ---------------------------------------------------------------------------
// Internal command result
// ---------------------------------------------------------------------------

/** Raw result from executing a ws-ckpt CLI command. */
export interface CommandOutput {
  /** Process exit code. */
  exitCode: number;
  /** Standard output. */
  stdout: string;
  /** Standard error output. */
  stderr: string;
}
