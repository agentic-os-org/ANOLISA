/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs/promises';
import * as os from 'node:os';
import * as path from 'node:path';
import { tryAcquireLock, isLockStale, releaseLock } from './memoryService.js';

const FAR_FUTURE_PID = 2 ** 22; // Reasonably guaranteed not to be alive on POSIX.

describe('memoryService lock management', () => {
  let tmpDir: string;
  let lockPath: string;

  beforeEach(async () => {
    tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), 'memsvc-lock-test-'));
    lockPath = path.join(tmpDir, '.extraction.lock');
  });

  afterEach(async () => {
    await fs.rm(tmpDir, { recursive: true, force: true });
  });

  describe('tryAcquireLock', () => {
    it('acquires a lock on a fresh path', async () => {
      expect(await tryAcquireLock(lockPath)).toBe(true);
      const content = await fs.readFile(lockPath, 'utf-8');
      const parsed = JSON.parse(content);
      expect(parsed.pid).toBe(process.pid);
      expect(typeof parsed.startedAt).toBe('string');
    });

    it('returns false when a live-process lock already exists', async () => {
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: process.pid,
          startedAt: new Date().toISOString(),
        }),
      );
      expect(await tryAcquireLock(lockPath)).toBe(false);
    });

    it('reclaims a stale lock whose owner PID is dead', async () => {
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: FAR_FUTURE_PID,
          startedAt: new Date().toISOString(),
        }),
      );
      expect(await tryAcquireLock(lockPath)).toBe(true);
      const parsed = JSON.parse(await fs.readFile(lockPath, 'utf-8'));
      expect(parsed.pid).toBe(process.pid);
    });

    it('reclaims an aged lock even when the owner PID is alive', async () => {
      const ancient = new Date(Date.now() - 60 * 60 * 1000).toISOString();
      await fs.writeFile(
        lockPath,
        JSON.stringify({ pid: process.pid, startedAt: ancient }),
      );
      expect(await tryAcquireLock(lockPath)).toBe(true);
      const parsed = JSON.parse(await fs.readFile(lockPath, 'utf-8'));
      expect(new Date(parsed.startedAt).getTime()).toBeGreaterThan(
        new Date(ancient).getTime(),
      );
    });

    it('reclaims a lock with malformed JSON content', async () => {
      await fs.writeFile(lockPath, 'not-json{');
      expect(await tryAcquireLock(lockPath)).toBe(true);
      const parsed = JSON.parse(await fs.readFile(lockPath, 'utf-8'));
      expect(parsed.pid).toBe(process.pid);
    });

    it('does not give up the recursion budget without making progress', async () => {
      // Live lock — single attempt, no recursion. Returns false promptly.
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: process.pid,
          startedAt: new Date().toISOString(),
        }),
      );
      const start = Date.now();
      expect(await tryAcquireLock(lockPath)).toBe(false);
      expect(Date.now() - start).toBeLessThan(1000);
    });
  });

  describe('isLockStale', () => {
    it('returns true when the lock file does not exist', async () => {
      expect(await isLockStale(lockPath)).toBe(true);
    });

    it('returns true when JSON is malformed', async () => {
      await fs.writeFile(lockPath, '{not json');
      expect(await isLockStale(lockPath)).toBe(true);
    });

    it('returns true when owner PID is dead', async () => {
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: FAR_FUTURE_PID,
          startedAt: new Date().toISOString(),
        }),
      );
      expect(await isLockStale(lockPath)).toBe(true);
    });

    it('returns true when lock age exceeds the threshold', async () => {
      const ancient = new Date(Date.now() - 60 * 60 * 1000).toISOString();
      await fs.writeFile(
        lockPath,
        JSON.stringify({ pid: process.pid, startedAt: ancient }),
      );
      expect(await isLockStale(lockPath)).toBe(true);
    });

    it('returns false for a fresh lock owned by a live PID', async () => {
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: process.pid,
          startedAt: new Date().toISOString(),
        }),
      );
      expect(await isLockStale(lockPath)).toBe(false);
    });
  });

  describe('releaseLock', () => {
    it('removes the lock file', async () => {
      await fs.writeFile(lockPath, '{}');
      await releaseLock(lockPath);
      await expect(fs.stat(lockPath)).rejects.toMatchObject({ code: 'ENOENT' });
    });

    it('is a no-op when the lock file is already absent', async () => {
      await releaseLock(lockPath); // should not throw
    });
  });

  describe('takeover does not double-acquire under serial contention', () => {
    it('only the first caller wins when both observe the same stale lock', async () => {
      await fs.writeFile(
        lockPath,
        JSON.stringify({
          pid: FAR_FUTURE_PID,
          startedAt: new Date(Date.now() - 60 * 60 * 1000).toISOString(),
        }),
      );

      // Run both attempts concurrently. The atomic-rename takeover guarantees
      // at most one of them produces a fresh lock; the other must observe
      // either EEXIST or the in-flight state and back off.
      const results = await Promise.all([
        tryAcquireLock(lockPath),
        tryAcquireLock(lockPath),
      ]);
      const winners = results.filter(Boolean).length;
      expect(winners).toBe(1);

      const parsed = JSON.parse(await fs.readFile(lockPath, 'utf-8'));
      expect(parsed.pid).toBe(process.pid);
    });
  });
});
