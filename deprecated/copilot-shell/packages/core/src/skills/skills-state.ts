/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import { Storage } from '../config/storage.js';
import type { SkillLevel } from './types.js';

/**
 * Persistent state for skill enable/disable management.
 * Uses a per-level blacklist model: skills not in the disabled list are enabled.
 */
export interface SkillsState {
  disabledSkills: Record<SkillLevel, string[]>;
}

const DEFAULT_STATE: SkillsState = {
  disabledSkills: {
    project: [],
    custom: [],
    user: [],
    extension: [],
    system: [],
  },
};

/**
 * Manages the persistent enable/disable state for skills.
 * State is stored in ~/.copilot-shell/skills-state.json.
 *
 * Design principles:
 * - Never throws on read/write failures (graceful degradation)
 * - Missing or corrupt file defaults to all-enabled
 * - Atomic writes via temp file + rename
 */
export class SkillsStateManager {
  /**
   * Serializes all read-modify-write operations against skills-state.json
   * to prevent races when the user presses Space rapidly or when cleanup
   * runs concurrently with toggle operations.
   */
  private writeChain: Promise<unknown> = Promise.resolve();

  /**
   * Reads the current skills state from disk.
   * Returns the default state (all enabled) if the file is missing or corrupt.
   */
  async readState(): Promise<SkillsState> {
    try {
      const filePath = Storage.getSkillsStatePath();
      const content = await fs.readFile(filePath, 'utf8');
      const parsed = JSON.parse(content) as Partial<SkillsState>;

      // Validate and merge with defaults to ensure all levels exist
      if (!parsed.disabledSkills || typeof parsed.disabledSkills !== 'object') {
        return structuredClone(DEFAULT_STATE);
      }

      const state: SkillsState = structuredClone(DEFAULT_STATE);
      const levels: SkillLevel[] = [
        'project',
        'custom',
        'user',
        'extension',
        'system',
      ];

      for (const level of levels) {
        const value = parsed.disabledSkills[level];
        if (Array.isArray(value)) {
          state.disabledSkills[level] = value.filter(
            (item): item is string => typeof item === 'string',
          );
        }
      }

      return state;
    } catch {
      // File doesn't exist, is unreadable, or contains invalid JSON
      return structuredClone(DEFAULT_STATE);
    }
  }

  /**
   * Writes the skills state to disk atomically.
   * Uses temp file + rename to prevent corruption on crash.
   */
  async writeState(state: SkillsState): Promise<void> {
    try {
      const filePath = Storage.getSkillsStatePath();
      const dir = path.dirname(filePath);

      // Ensure directory exists
      await fs.mkdir(dir, { recursive: true });

      // Atomic write: write to temp file then rename
      const tmpPath = filePath + '.tmp.' + process.pid;
      await fs.writeFile(tmpPath, JSON.stringify(state, null, 2), 'utf8');
      await fs.rename(tmpPath, filePath);
    } catch (error) {
      console.warn(
        'Failed to write skills state:',
        error instanceof Error ? error.message : error,
      );
    }
  }

  /**
   * Checks whether a skill is disabled at a specific level.
   */
  async isDisabled(skillName: string, level: SkillLevel): Promise<boolean> {
    const state = await this.readState();
    return state.disabledSkills[level].includes(skillName);
  }

  /**
   * Toggles the disabled state of a skill at a specific level.
   * Operations are serialized through `writeChain` so that rapid Space
   * presses cannot trigger overlapping read-modify-write cycles.
   * @returns The new disabled status (true = now disabled, false = now enabled).
   */
  async toggleDisabled(skillName: string, level: SkillLevel): Promise<boolean> {
    const next = this.writeChain.then(async () => {
      const state = await this.readState();
      const list = state.disabledSkills[level];
      const index = list.indexOf(skillName);

      if (index >= 0) {
        // Currently disabled -> enable (remove from list)
        list.splice(index, 1);
        await this.writeState(state);
        return false;
      }

      // Currently enabled -> disable (add to list)
      list.push(skillName);
      await this.writeState(state);
      return true;
    });
    // Swallow errors on the chain so a single failure does not block
    // subsequent operations; callers still observe the rejected promise.
    this.writeChain = next.catch(() => undefined);
    return next;
  }

  /**
   * Returns the full disabled skills map by level.
   */
  async getDisabledSkillsByLevel(): Promise<Record<SkillLevel, string[]>> {
    const state = await this.readState();
    return state.disabledSkills;
  }

  /**
   * Removes stale entries from the disabled list.
   * An entry is stale if the skill no longer exists at that level.
   * Serialized through `writeChain` to stay ordered relative to toggles.
   */
  async cleanupStaleEntries(
    existingByLevel: Map<SkillLevel, string[]>,
  ): Promise<void> {
    const next = this.writeChain.then(async () => {
      const state = await this.readState();
      let changed = false;

      for (const [level, existingNames] of existingByLevel) {
        const disabledList = state.disabledSkills[level];
        const filtered = disabledList.filter((name) =>
          existingNames.includes(name),
        );

        if (filtered.length !== disabledList.length) {
          state.disabledSkills[level] = filtered;
          changed = true;
        }
      }

      if (changed) {
        await this.writeState(state);
      }
    });
    this.writeChain = next.catch(() => undefined);
    return next;
  }
}
