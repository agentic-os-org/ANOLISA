/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { QWEN_DIR } from '../config/storage.js';

const OLD_CONFIG_DIR = '.copilot';

/**
 * Migrates the cosh configuration directory from ~/.copilot to ~/.copilot-shell
 * if the old directory exists and the new directory does not.
 *
 * This migration is necessary to avoid conflicts with other tools (e.g. VSCode Copilot)
 * that use the ~/.copilot namespace.
 *
 * @returns A warning message string if migration was performed or failed,
 *          or null if no migration was needed.
 */
export async function migrateConfigDirIfNeeded(): Promise<string | null> {
  const homeDir = os.homedir();
  if (!homeDir) {
    return null;
  }

  const oldDir = path.join(homeDir, OLD_CONFIG_DIR);
  const newDir = path.join(homeDir, QWEN_DIR);

  const oldExists = fs.existsSync(oldDir);
  const newExists = fs.existsSync(newDir);

  // No migration needed: old dir absent, or new dir already exists
  if (!oldExists || newExists) {
    return null;
  }

  try {
    fs.cpSync(oldDir, newDir, { recursive: true });
    return (
      `Config directory migrated from ~/${OLD_CONFIG_DIR} to ~/${QWEN_DIR}. ` +
      `You may remove ~/${OLD_CONFIG_DIR} after confirming everything works.`
    );
  } catch (err) {
    return (
      `Warning: Failed to migrate config from ~/${OLD_CONFIG_DIR} to ~/${QWEN_DIR} ` +
      `(${err instanceof Error ? err.message : String(err)}). ` +
      `Falling back to ~/${OLD_CONFIG_DIR}.`
    );
  }
}
