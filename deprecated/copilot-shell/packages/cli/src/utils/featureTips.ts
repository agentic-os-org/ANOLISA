/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import fs from 'node:fs/promises';
import path from 'node:path';
import { Storage } from '@copilot-shell/core';

export interface FeatureTip {
  /** Unique identifier for persistence tracking */
  id: string;
  /** Tip message text (i18n key) */
  message: string;
  /** Optional emoji prefix */
  emoji?: string;
  /** Priority — higher values shown first, default 0 */
  priority?: number;
}

export const FEATURE_TIPS: readonly FeatureTip[] = [
  {
    id: 'bash-interactive-shell',
    emoji: '\uD83D\uDC1A',
    message:
      'Use /bash to switch to an interactive Bash shell at any time. Type "exit" or press Ctrl+D to return to Copilot Shell.',
    priority: 10,
  },
  {
    id: 'dir-cd',
    emoji: '\uD83D\uDCC2',
    message:
      'Use /dir cd <path> to switch the current working directory without leaving Copilot Shell.',
    priority: 5,
  },
  {
    id: 'cosh-switch',
    emoji: '\uD83D\uDD04',
    message:
      'Use /cosh-switch or run "cosh-switch" in bash to switch between copilot-shell and cosh-ng.',
    priority: 15,
  },
  // Append new feature tips here
];

interface FeatureTipsState {
  shownTipIds: string[];
}

const STATE_FILENAME = 'feature-tips-state.json';

function getStatePath(): string {
  return path.join(Storage.getGlobalQwenDir(), STATE_FILENAME);
}

async function readState(): Promise<FeatureTipsState> {
  try {
    const raw = await fs.readFile(getStatePath(), 'utf-8');
    const parsed = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed === 'object' &&
      Array.isArray(parsed.shownTipIds)
    ) {
      return parsed as FeatureTipsState;
    }
    return { shownTipIds: [] };
  } catch {
    return { shownTipIds: [] };
  }
}

async function writeState(state: FeatureTipsState): Promise<void> {
  const filePath = getStatePath();
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  await fs.writeFile(filePath, JSON.stringify(state, null, 2), 'utf-8');
}

/**
 * Returns the single highest-priority unshown feature tip and immediately
 * marks it as shown. Returns an empty array when all tips have been shown
 * or the registry is empty. I/O errors are caught internally — worst case
 * returns an empty array.
 */
export async function getAndMarkUnshownFeatureTips(): Promise<FeatureTip[]> {
  try {
    if (FEATURE_TIPS.length === 0) {
      return [];
    }

    const state = await readState();
    const shownSet = new Set(state.shownTipIds);

    const unshown = FEATURE_TIPS.filter((tip) => !shownSet.has(tip.id));
    if (unshown.length === 0) {
      return [];
    }

    // Pick the highest-priority unshown tip
    unshown.sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0));
    const selected = unshown[0];

    // Mark as shown
    try {
      state.shownTipIds.push(selected.id);
      await writeState(state);
    } catch {
      // Write failure is non-critical — tip may re-appear next launch
    }

    return [selected];
  } catch {
    return [];
  }
}
