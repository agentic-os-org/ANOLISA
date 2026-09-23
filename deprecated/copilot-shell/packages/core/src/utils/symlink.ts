/**
 * Utility for creating/updating symlinks with optional fallback.
 */

import { promises as fs } from 'node:fs';
import path from 'node:path';

export interface UpdateSymlinkOptions {
  /** If true, copy the file instead of symlinking when symlink fails */
  fallbackCopy?: boolean;
}

/**
 * Creates or updates a symlink at `aliasPath` pointing to `targetPath`.
 * On platforms that don't support symlinks, optionally falls back to copying.
 */
export async function updateSymlink(
  aliasPath: string,
  targetPath: string,
  options?: UpdateSymlinkOptions,
): Promise<void> {
  const { fallbackCopy = false } = options ?? {};

  try {
    // Remove existing symlink/file if it exists
    await fs.unlink(aliasPath).catch(() => {
      // Ignore if it doesn't exist
    });

    // Create relative symlink
    const relativeTarget = path.relative(path.dirname(aliasPath), targetPath);
    await fs.symlink(relativeTarget, aliasPath);
  } catch {
    if (fallbackCopy) {
      try {
        await fs.copyFile(targetPath, aliasPath);
      } catch {
        // Best-effort; silently ignore
      }
    }
  }
}
