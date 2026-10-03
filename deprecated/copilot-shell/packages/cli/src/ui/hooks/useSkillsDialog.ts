/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState, useCallback, useEffect, useRef } from 'react';
import type {
  SkillConfig,
  SkillLevel,
  SkillManager,
} from '@copilot-shell/core';
import type { SkillDefinition } from '../types.js';

/** Tab label to SkillLevel mapping. */
const TAB_LEVELS: readonly SkillLevel[] = [
  'project',
  'custom',
  'user',
  'extension',
  'system',
] as const;

export interface UseSkillsDialogReturn {
  isSkillsDialogOpen: boolean;
  openSkillsDialog: () => void;
  closeSkillsDialog: () => void;
  /** Skills grouped by level for each tab. */
  skillsByLevel: Record<SkillLevel, SkillDefinition[]>;
  /** Toggle the disabled state of a skill. */
  toggleSkillDisabled: (skillName: string, level: SkillLevel) => Promise<void>;
  /** Whether data is currently loading. */
  isLoading: boolean;
}

export function useSkillsDialog(
  skillManager: SkillManager | undefined,
): UseSkillsDialogReturn {
  const [isSkillsDialogOpen, setIsSkillsDialogOpen] = useState(false);
  const [skillsByLevel, setSkillsByLevel] = useState<
    Record<SkillLevel, SkillDefinition[]>
  >({
    project: [],
    custom: [],
    user: [],
    extension: [],
    system: [],
  });
  const [isLoading, setIsLoading] = useState(false);
  const mountedRef = useRef(true);

  useEffect(
    () => () => {
      mountedRef.current = false;
    },
    [],
  );

  const loadSkills = useCallback(async () => {
    if (!skillManager) return;

    setIsLoading(true);
    try {
      const stateManager = skillManager.getSkillsStateManager();
      const disabledByLevel = await stateManager.getDisabledSkillsByLevel();

      // Refresh the cache once; `listSkills({ force: true })` rebuilds all
      // levels in one pass, so the subsequent per-level calls below can be
      // parallelized and served from cache without triggering additional
      // refreshes (avoids 5x redundant disk scans).
      await skillManager.listSkills({ force: true });

      // Per-level loads are independent: seenNames in SkillManager is scoped
      // to a single call, so parallel loads cannot break precedence semantics.
      const entries = await Promise.all(
        TAB_LEVELS.map(async (level) => {
          const skills: SkillConfig[] = await skillManager.listSkills({
            level,
          });
          const disabledNames = disabledByLevel[level] || [];
          return [
            level,
            skills.map((skill) => ({
              name: skill.name,
              description: skill.description,
              level: skill.level,
              disabled: disabledNames.includes(skill.name),
            })),
          ] as const;
        }),
      );

      const result: Record<SkillLevel, SkillDefinition[]> = {
        project: [],
        custom: [],
        user: [],
        extension: [],
        system: [],
      };
      for (const [level, list] of entries) {
        result[level] = list;
      }

      if (mountedRef.current) {
        setSkillsByLevel(result);
      }

      // Async cleanup of stale entries in skills-state.json (fire-and-forget)
      const existingByLevel = new Map<SkillLevel, string[]>();
      for (const level of TAB_LEVELS) {
        existingByLevel.set(
          level,
          result[level].map((s) => s.name),
        );
      }
      void stateManager.cleanupStaleEntries(existingByLevel);
    } catch (error) {
      console.warn('Failed to load skills for dialog:', error);
    } finally {
      if (mountedRef.current) {
        setIsLoading(false);
      }
    }
  }, [skillManager]);

  const openSkillsDialog = useCallback(() => {
    setIsSkillsDialogOpen(true);
    void loadSkills();
  }, [loadSkills]);

  const closeSkillsDialog = useCallback(() => {
    setIsSkillsDialogOpen(false);
  }, []);

  const toggleSkillDisabled = useCallback(
    async (skillName: string, level: SkillLevel) => {
      if (!skillManager) return;

      const stateManager = skillManager.getSkillsStateManager();
      const nowDisabled = await stateManager.toggleDisabled(skillName, level);

      // Update local state optimistically
      setSkillsByLevel((prev) => {
        const updated = { ...prev };
        updated[level] = prev[level].map((skill) =>
          skill.name === skillName
            ? { ...skill, disabled: nowDisabled }
            : skill,
        );
        return updated;
      });

      // Notify SkillManager so SkillTool refreshes the LLM tool list
      skillManager.notifySkillsChanged();
    },
    [skillManager],
  );

  return {
    isSkillsDialogOpen,
    openSkillsDialog,
    closeSkillsDialog,
    skillsByLevel,
    toggleSkillDisabled,
    isLoading,
  };
}
