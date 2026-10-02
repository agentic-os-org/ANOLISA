/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs/promises';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  validateInboxMemoryPatchFile,
  MEMORY_INDEX_FILENAME,
} from './memoryPatchUtils.js';
import type { Config } from '../../config/config.js';

function makeConfig(projectMemoryDir: string): Config {
  return {
    storage: {
      getProjectMemoryTempDir: () => projectMemoryDir,
    },
  } as unknown as Config;
}

describe('validateInboxMemoryPatchFile', () => {
  let tmpDir: string;
  let memoryDir: string;
  let config: Config;

  beforeEach(async () => {
    tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), 'inbox-patch-test-'));
    memoryDir = path.join(tmpDir, 'memory');
    await fs.mkdir(memoryDir, { recursive: true });
    config = makeConfig(memoryDir);
  });

  afterEach(async () => {
    await fs.rm(tmpDir, { recursive: true, force: true });
  });

  const writeInboxPatch = async (content: string): Promise<string> => {
    const inboxDir = path.join(memoryDir, '.inbox', 'private');
    await fs.mkdir(inboxDir, { recursive: true });
    const patchPath = path.join(inboxDir, 'candidate.patch');
    await fs.writeFile(patchPath, content, 'utf-8');
    return patchPath;
  };

  it('returns patches and parsed on success for a valid single-file new-file patch', async () => {
    const targetPath = path.resolve(memoryDir, 'hello.md');
    const patchContent = [
      `--- /dev/null`,
      `+++ ${targetPath}`,
      `@@ -0,0 +1,1 @@`,
      `+hello`,
      ``,
    ].join('\n');
    const patchPath = await writeInboxPatch(patchContent);

    const result = await validateInboxMemoryPatchFile(
      config,
      'private',
      patchPath,
    );
    expect(result.valid).toBe(true);
    if (result.valid) {
      expect(result.patches).toHaveLength(1);
      expect(result.patches[0].targetPath).toBe(targetPath);
      expect(result.patches[0].isNewFile).toBe(true);
      expect(result.parsed).toHaveLength(1);
      expect(result.parsed[0].hunks.length).toBeGreaterThan(0);
    }
  });

  it('rejects patch files containing more than one diff (multi-file combined patch)', async () => {
    const file1 = path.resolve(memoryDir, 'a.md');
    const file2 = path.resolve(memoryDir, 'b.md');
    const patchContent = [
      `--- /dev/null`,
      `+++ ${file1}`,
      `@@ -0,0 +1,1 @@`,
      `+a`,
      `--- /dev/null`,
      `+++ ${file2}`,
      `@@ -0,0 +1,1 @@`,
      `+b`,
      ``,
    ].join('\n');
    const patchPath = await writeInboxPatch(patchContent);

    const result = await validateInboxMemoryPatchFile(
      config,
      'private',
      patchPath,
    );
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.reason).toMatch(/expected exactly one diff/i);
      expect(result.reason).toMatch(/got 2/);
    }
  });

  it('rejects patches with no hunks', async () => {
    const patchPath = await writeInboxPatch('not a real patch\n');
    const result = await validateInboxMemoryPatchFile(
      config,
      'private',
      patchPath,
    );
    expect(result.valid).toBe(false);
  });

  it('rejects patches whose target is outside the allowed private memory root', async () => {
    const outside = path.resolve(tmpDir, 'outside.md');
    const patchContent = [
      `--- /dev/null`,
      `+++ ${outside}`,
      `@@ -0,0 +1,1 @@`,
      `+pwn`,
      ``,
    ].join('\n');
    const patchPath = await writeInboxPatch(patchContent);

    const result = await validateInboxMemoryPatchFile(
      config,
      'private',
      patchPath,
    );
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.reason).toMatch(/outside private memory roots/);
    }
  });

  it('accepts memory index file under the allowed root', async () => {
    const indexPath = path.resolve(memoryDir, MEMORY_INDEX_FILENAME);
    const patchContent = [
      `--- /dev/null`,
      `+++ ${indexPath}`,
      `@@ -0,0 +1,1 @@`,
      `+index`,
      ``,
    ].join('\n');
    const patchPath = await writeInboxPatch(patchContent);
    const result = await validateInboxMemoryPatchFile(
      config,
      'private',
      patchPath,
    );
    expect(result.valid).toBe(true);
  });
});
