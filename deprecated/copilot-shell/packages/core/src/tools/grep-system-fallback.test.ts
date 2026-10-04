/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import path from 'node:path';
import fs from 'node:fs/promises';
import os from 'node:os';
import { GrepTool } from './grep.js';
import type { Config } from '../config/config.js';
import { createMockWorkspaceContext } from '../test-utils/mockWorkspaceContext.js';
import { isCommandAvailable } from '../utils/shell-utils.js';
import { isGitRepository } from '../utils/gitUtils.js';

// Force the system-grep strategy: the JS fallback and git grep are both
// case-insensitive, so only the system grep path can regress.
vi.mock('../utils/gitUtils.js', { spy: true });

describe('GrepTool system grep fallback', () => {
  let tempRootDir: string;
  const abortSignal = new AbortController().signal;

  const mockConfig = {
    getTargetDir: () => tempRootDir,
    getWorkspaceContext: () => createMockWorkspaceContext(tempRootDir),
    getFileExclusions: () => ({
      getGlobExcludes: () => [],
    }),
    getTruncateToolOutputThreshold: () => 25000,
    getTruncateToolOutputLines: () => 1000,
  } as unknown as Config;

  beforeEach(async () => {
    vi.mocked(isGitRepository).mockReturnValue(false);
    tempRootDir = await fs.mkdtemp(path.join(os.tmpdir(), 'grep-sys-root-'));
    await fs.writeFile(
      path.join(tempRootDir, 'notes.txt'),
      'HELLO WORLD\nsecond line',
    );
  });

  afterEach(async () => {
    await fs.rm(tempRootDir, { recursive: true, force: true });
    vi.mocked(isGitRepository).mockReset();
  });

  it.skipIf(!isCommandAvailable('grep').available)(
    'matches case-insensitively when it shells out to system grep',
    async () => {
      const grepTool = new GrepTool(mockConfig);
      const invocation = grepTool.build({ pattern: 'hello' });
      const result = await invocation.execute(abortSignal);

      expect(result.llmContent).toContain('Found 1 match');
      expect(result.llmContent).toContain('HELLO WORLD');
    },
  );
});
