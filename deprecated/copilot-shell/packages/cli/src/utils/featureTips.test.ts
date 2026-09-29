/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs/promises';
import * as os from 'node:os';
import path from 'node:path';

let testDir: string;

vi.mock('@copilot-shell/core', () => ({
  Storage: {
    getGlobalQwenDir: () => testDir,
  },
}));

// Dynamic import so the mock is active when the module loads
const { getAndMarkUnshownFeatureTips, FEATURE_TIPS } =
  await import('./featureTips.js');

// Pre-compute expected values from the actual registry
const sortedTips = [...FEATURE_TIPS].sort(
  (a, b) => (b.priority ?? 0) - (a.priority ?? 0),
);
const highestPriorityTip = sortedTips[0];
const allTipIds = FEATURE_TIPS.map((tip) => tip.id);

describe('getAndMarkUnshownFeatureTips', () => {
  beforeEach(async () => {
    testDir = await fs.mkdtemp(path.join(os.tmpdir(), 'feature-tips-test-'));
  });

  afterEach(async () => {
    await fs.rm(testDir, { recursive: true, force: true });
  });

  it('should return the highest-priority tip on first call when state file does not exist', async () => {
    const tips = await getAndMarkUnshownFeatureTips();
    expect(tips).toHaveLength(1);
    expect(tips[0].id).toBe(highestPriorityTip.id);

    // State file should have been created
    const raw = await fs.readFile(
      path.join(testDir, 'feature-tips-state.json'),
      'utf-8',
    );
    const state = JSON.parse(raw);
    expect(state.shownTipIds).toContain(highestPriorityTip.id);
  });

  it('should return empty array when all tips are already shown', async () => {
    // Pre-populate state with all tip IDs marked as shown
    await fs.writeFile(
      path.join(testDir, 'feature-tips-state.json'),
      JSON.stringify({ shownTipIds: allTipIds }),
      'utf-8',
    );

    const tips = await getAndMarkUnshownFeatureTips();
    expect(tips).toHaveLength(0);
  });

  it('should return the highest-priority unshown tip and skip already-shown ones', async () => {
    // Mark the highest-priority tip as shown
    await fs.writeFile(
      path.join(testDir, 'feature-tips-state.json'),
      JSON.stringify({ shownTipIds: [highestPriorityTip.id] }),
      'utf-8',
    );

    const tips = await getAndMarkUnshownFeatureTips();
    if (FEATURE_TIPS.length > 1) {
      expect(tips).toHaveLength(1);
      // Should be the second-highest-priority tip
      expect(tips[0].id).toBe(sortedTips[1].id);
    } else {
      // Only one tip in registry and it's already shown
      expect(tips).toHaveLength(0);
    }
  });

  it('should handle corrupted state file gracefully', async () => {
    // Write invalid JSON
    await fs.writeFile(
      path.join(testDir, 'feature-tips-state.json'),
      'not valid json!!!',
      'utf-8',
    );

    const tips = await getAndMarkUnshownFeatureTips();
    // Should fall back to empty state and return the highest-priority tip
    expect(tips).toHaveLength(1);
    expect(tips[0].id).toBe(highestPriorityTip.id);
  });

  it('should auto-create directory when state directory does not exist', async () => {
    // Remove the test dir so the write must create it
    await fs.rm(testDir, { recursive: true, force: true });

    const tips = await getAndMarkUnshownFeatureTips();
    expect(tips).toHaveLength(1);

    // Verify directory and file were created
    const stat = await fs.stat(path.join(testDir, 'feature-tips-state.json'));
    expect(stat.isFile()).toBe(true);
  });

  it('should not throw when write fails due to permission issues', async () => {
    // Make dir read-only to prevent writing
    await fs.chmod(testDir, 0o444);

    const tips = await getAndMarkUnshownFeatureTips();
    // Should still return the tip despite write failure
    expect(tips).toHaveLength(1);
    expect(tips[0].id).toBe(highestPriorityTip.id);

    // Restore permissions for cleanup
    await fs.chmod(testDir, 0o755);
  });
});
