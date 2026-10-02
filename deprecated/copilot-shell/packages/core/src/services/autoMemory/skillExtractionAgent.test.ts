/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs/promises';
import * as os from 'node:os';
import * as path from 'node:path';
import { createExtractionHooks } from './skillExtractionAgent.js';
import type { PostToolUsePayload } from '../../subagents/subagent-hooks.js';

const basePayload = {
  subagentId: 'sub-1',
  name: 'auto-memory-extractor',
  durationMs: 0,
  timestamp: Date.now(),
};

function makePayload(
  toolName: string,
  args: Record<string, unknown>,
  success = true,
): PostToolUsePayload {
  return { ...basePayload, toolName, args, success };
}

describe('createExtractionHooks session-read tracking', () => {
  const chatsDir = path.resolve('/tmp/qoder-chats-hook');
  const uuidA = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
  const uuidB = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';

  it('reports sessionId when read_file reads a chat session file', async () => {
    const reads: string[] = [];
    const hooks = createExtractionHooks({
      chatsDir,
      onSessionRead: (sid) => reads.push(sid),
    });
    await hooks.postToolUse?.(
      makePayload('read_file', {
        file_path: path.join(chatsDir, `${uuidA}.jsonl`),
      }),
    );
    expect(reads).toEqual([uuidA]);
  });

  it('reports multiple sessionIds from read_many_files paths', async () => {
    const reads: string[] = [];
    const hooks = createExtractionHooks({
      chatsDir,
      onSessionRead: (sid) => reads.push(sid),
    });
    await hooks.postToolUse?.(
      makePayload('read_many_files', {
        paths: [
          path.join(chatsDir, `${uuidA}.jsonl`),
          path.join(chatsDir, `${uuidB}.jsonl`),
          '/tmp/not-a-session.txt',
        ],
      }),
    );
    expect(reads.sort()).toEqual([uuidA, uuidB].sort());
  });

  it('ignores non-chat paths outside chatsDir', async () => {
    const reads: string[] = [];
    const hooks = createExtractionHooks({
      chatsDir,
      onSessionRead: (sid) => reads.push(sid),
    });
    await hooks.postToolUse?.(
      makePayload('read_file', { file_path: '/etc/passwd' }),
    );
    expect(reads).toEqual([]);
  });

  it('does not report when tool call failed', async () => {
    const reads: string[] = [];
    const hooks = createExtractionHooks({
      chatsDir,
      onSessionRead: (sid) => reads.push(sid),
    });
    await hooks.postToolUse?.(
      makePayload(
        'read_file',
        { file_path: path.join(chatsDir, `${uuidA}.jsonl`) },
        /* success */ false,
      ),
    );
    expect(reads).toEqual([]);
  });

  it('is a no-op on session tracking when no options are provided', async () => {
    const hooks = createExtractionHooks();
    // Should simply not throw, and not observe any side-effect.
    await hooks.postToolUse?.(
      makePayload('read_file', {
        file_path: path.join(chatsDir, `${uuidA}.jsonl`),
      }),
    );
  });

  it('does not report for non-read tools', async () => {
    const reads: string[] = [];
    const hooks = createExtractionHooks({
      chatsDir,
      onSessionRead: (sid) => reads.push(sid),
    });
    await hooks.postToolUse?.(
      makePayload('glob', {
        pattern: path.join(chatsDir, '*.jsonl'),
      }),
    );
    expect(reads).toEqual([]);
  });
});

describe('createExtractionHooks patch validation', () => {
  let tmpDir: string;
  let inboxDir: string;
  let patchPath: string;

  const validPatch = [
    '--- /dev/null',
    '+++ /tmp/example.md',
    '@@ -0,0 +1,2 @@',
    '+hello',
    '+world',
    '',
  ].join('\n');

  beforeEach(async () => {
    tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), 'hook-patch-test-'));
    inboxDir = path.join(tmpDir, '.inbox', 'private');
    await fs.mkdir(inboxDir, { recursive: true });
    patchPath = path.join(inboxDir, 'extraction.patch');
  });

  afterEach(async () => {
    await fs.rm(tmpDir, { recursive: true, force: true });
  });

  it('normalizes a valid patch file in place via async IO', async () => {
    // Write a patch whose hunk header has the wrong line count; normalization
    // should recompute it from the actual content.
    const wrongCount = validPatch.replace(
      '@@ -0,0 +1,2 @@',
      '@@ -0,0 +1,99 @@',
    );
    await fs.writeFile(patchPath, wrongCount, 'utf-8');

    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', { file_path: patchPath }),
    );

    expect(result).toBeUndefined();
    const finalContent = await fs.readFile(patchPath, 'utf-8');
    expect(finalContent).toContain('@@ -0,0 +1,2 @@');
    expect(finalContent).not.toContain('@@ -0,0 +1,99 @@');
  });

  it('returns additionalContent when the patch has no valid hunks', async () => {
    await fs.writeFile(patchPath, 'this is not a patch\n', 'utf-8');

    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', { file_path: patchPath }),
    );

    expect(result?.additionalContent).toMatch(/PATCH VALIDATION FAILED/);
  });

  it('ignores write_file outside the .inbox path', async () => {
    const outsidePath = path.join(tmpDir, 'random.patch');
    await fs.writeFile(outsidePath, 'irrelevant', 'utf-8');

    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', { file_path: outsidePath }),
    );
    expect(result).toBeUndefined();

    // File should be untouched (not normalized/rewritten).
    expect(await fs.readFile(outsidePath, 'utf-8')).toBe('irrelevant');
  });

  it('ignores write_file for non-.patch files even inside .inbox', async () => {
    const mdPath = path.join(inboxDir, 'note.md');
    await fs.writeFile(mdPath, 'not a patch', 'utf-8');

    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', { file_path: mdPath }),
    );
    expect(result).toBeUndefined();
  });

  it('returns undefined when the file is missing (no follow-up error)', async () => {
    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', {
        file_path: path.join(inboxDir, 'does-not-exist.patch'),
      }),
    );
    expect(result).toBeUndefined();
  });

  it('skips validation when the tool call did not succeed', async () => {
    await fs.writeFile(patchPath, 'broken', 'utf-8');
    const hooks = createExtractionHooks();
    const result = await hooks.postToolUse?.(
      makePayload('write_file', { file_path: patchPath }, /* success */ false),
    );
    expect(result).toBeUndefined();
    // File was not rewritten.
    expect(await fs.readFile(patchPath, 'utf-8')).toBe('broken');
  });
});
