/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { spawnSync } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Config } from '../config/config.js';
import { createMockWorkspaceContext } from '../test-utils/mockWorkspaceContext.js';
import { GrepTool } from './grep.js';

// These tests exercise the real system-grep fallback (Strategy 2), so they
// require an actual `grep` binary and a non-git temp directory.
const grepAvailable =
  spawnSync('grep', ['--version'], { windowsHide: true }).status === 0;

describe('GrepTool system grep fallback', () => {
  let tempRootDir: string;
  let grepTool: GrepTool;
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
    tempRootDir = await fs.mkdtemp(
      path.join(os.tmpdir(), 'grep-system-fallback-'),
    );
    grepTool = new GrepTool(mockConfig);
    await fs.writeFile(
      path.join(tempRootDir, 'fileA.txt'),
      'HELLO WORLD\nsecond line\n',
    );
  });

  afterEach(async () => {
    await fs.rm(tempRootDir, { recursive: true, force: true });
  });

  const search = async (pattern: string): Promise<string> => {
    const invocation = grepTool.build({ pattern, path: tempRootDir });
    const result = await invocation.execute(abortSignal);
    return String(result.llmContent);
  };

  describe.skipIf(!grepAvailable)('with a real grep binary', () => {
    it('finds a case-insensitive match (issue #5015)', async () => {
      const llmContent = await search('hello');

      expect(llmContent).toContain('Found 1 match');
      expect(llmContent).toContain('HELLO WORLD');
    });

    it('still finds exact-case matches', async () => {
      const llmContent = await search('WORLD');

      expect(llmContent).toContain('Found 1 match');
    });

    it('keeps extended regex semantics', async () => {
      // `.` is only a metacharacter under -E; a single matching line keeps
      // this assertion independent of platform EOL handling.
      const llmContent = await search('HELLO.WORLD');

      expect(llmContent).toContain('Found 1 match');
      expect(llmContent).toContain('HELLO WORLD');
    });
  });
});
