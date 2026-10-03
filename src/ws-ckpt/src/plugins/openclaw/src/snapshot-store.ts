/**
 * In-memory snapshot cache for the ws-ckpt plugin.
 *
 * Maintains a lightweight list of known snapshots so that the plugin
 * can quickly look up the most recent checkpoint without issuing a
 * CLI call on every hook invocation.
 */

import type { SnapshotInfo } from "./types.js";

/**
 * In-memory snapshot store.
 *
 * This is **not** a persistent database — it mirrors the snapshot list
 * obtained from `ws-ckpt list` and is updated whenever the plugin
 * creates, deletes, or lists snapshots.
 */
export class SnapshotStore {
  private snapshots: SnapshotInfo[] = [];

  /**
   * Replace the entire snapshot list (e.g. after a `list` command).
   *
   * @param snapshots - The full snapshot list from the CLI.
   */
  public setAll(snapshots: SnapshotInfo[]): void {
    this.snapshots = [...snapshots];
  }

  /**
   * Return all cached snapshots, sorted newest-first by createdAt.
   */
  public getAll(): SnapshotInfo[] {
    return [...this.snapshots].sort(
      (a, b) => new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime(),
    );
  }

  /**
   * Add a snapshot to the cache.
   *
   * @param snapshot - The snapshot info to add.
   */
  public add(snapshot: SnapshotInfo): void {
    // Avoid duplicates
    const idx = this.snapshots.findIndex((s) => s.snapshot === snapshot.snapshot);
    if (idx >= 0) {
      this.snapshots[idx] = snapshot;
    } else {
      this.snapshots.push(snapshot);
    }
  }

  /**
   * Remove a snapshot from the cache by its identifier.
   *
   * @param snapshotId - The snapshot hash ID.
   * @returns `true` if found and removed, `false` otherwise.
   */
  public remove(snapshotId: string): boolean {
    const idx = this.snapshots.findIndex((s) => s.snapshot === snapshotId);
    if (idx >= 0) {
      this.snapshots.splice(idx, 1);
      return true;
    }
    return false;
  }

  /**
   * Return the number of cached snapshots.
   */
  public get count(): number {
    return this.snapshots.length;
  }
}
