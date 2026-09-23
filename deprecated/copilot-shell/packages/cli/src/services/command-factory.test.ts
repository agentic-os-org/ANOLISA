/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { ChildProcess } from 'node:child_process';
import {
  createSlashCommandFromDefinition,
  _spawnImpl,
} from './command-factory.js';
import type { MessageActionReturn } from '../ui/commands/types.js';

// Use vi.spyOn on the exported wrapper object so we avoid relying on ESM
// built-in module mocking (node:child_process in jsdom env is not interceptable
// via vi.mock).
const mockSpawn = vi.fn();

// ---------------------------------------------------------------------------
// Helper: builds a fake ChildProcess whose events can be triggered manually.
// ---------------------------------------------------------------------------
function makeFakeProc() {
  const listeners: Record<string, Array<(...args: unknown[]) => void>> = {};

  function makeStream(prefix: string) {
    return {
      on(event: string, cb: (...args: unknown[]) => void) {
        const key = `${prefix}:${event}`;
        listeners[key] = [...(listeners[key] ?? []), cb];
        return this;
      },
      destroy: vi.fn(),
    };
  }

  const proc = {
    stdout: makeStream('stdout'),
    stderr: makeStream('stderr'),
    on(event: string, cb: (...args: unknown[]) => void) {
      const key = `proc:${event}`;
      listeners[key] = [...(listeners[key] ?? []), cb];
      return this;
    },
    kill: vi.fn(),
  } as unknown as ChildProcess;

  const emit = {
    stdout: (data: Buffer) =>
      (listeners['stdout:data'] ?? []).forEach((cb) => cb(data)),
    stderr: (data: Buffer) =>
      (listeners['stderr:data'] ?? []).forEach((cb) => cb(data)),
    close: (code: number | null) =>
      (listeners['proc:close'] ?? []).forEach((cb) => cb(code)),
    error: (err: Error) =>
      (listeners['proc:error'] ?? []).forEach((cb) => cb(err)),
  };

  return { proc, emit };
}

// Shortcut: build a command with a `run` field and execute its action.
async function runCmd(run: string): Promise<MessageActionReturn> {
  const cmd = createSlashCommandFromDefinition(
    '/base/check.toml',
    '/base',
    { run },
    undefined,
    '.toml',
  );
  return (await cmd.action!(null as never, '')) as MessageActionReturn;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('createSlashCommandFromDefinition - run field', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(_spawnImpl, 'fn').mockImplementation(mockSpawn as never);
    mockSpawn.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('returns info message on successful exit (code 0)', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const promise = runCmd('echo hello');
    emit.stdout(Buffer.from('hello\n'));
    emit.close(0);

    const result = await promise;

    expect(result.type).toBe('message');
    expect(result.messageType).toBe('info');
    expect(result.content).toContain('hello\n');
    expect(result.content).not.toContain('exited with code');
  });

  it('returns error message on non-zero exit code', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const promise = runCmd('exit 1');
    emit.stderr(Buffer.from('error output\n'));
    emit.close(1);

    const result = await promise;

    expect(result.messageType).toBe('error');
    expect(result.content).toContain('[exited with code 1]');
  });

  it('returns error message when spawn emits process error (e.g. ENOENT)', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const promise = runCmd('nonexistent-cmd');
    emit.error(new Error('spawn sh ENOENT'));

    const result = await promise;

    expect(result.messageType).toBe('error');
    expect(result.content).toContain(
      '[failed to start process: spawn sh ENOENT]',
    );
  });

  it('kills process and reports error after 30 s timeout', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const promise = runCmd('sleep 9999');
    vi.advanceTimersByTime(30_001);
    emit.close(null); // simulate process killed

    const result = await promise;

    expect(proc.kill).toHaveBeenCalled();
    expect(result.messageType).toBe('error');
    expect(result.content).toContain('[process killed after 30s timeout]');
  });

  it('truncates output exceeding 512 KiB and returns info on exit 0', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const promise = runCmd('yes');
    emit.stdout(Buffer.alloc(600 * 1024, 'x')); // 600 KiB
    emit.close(0);

    const result = await promise;

    expect(result.content).toContain('[output truncated at 512 KiB]');
    expect(result.messageType).toBe('info');
  });

  it('omits $ prefix by default when show_command is not set', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const cmd = createSlashCommandFromDefinition(
      '/base/greet.toml',
      '/base',
      { run: 'echo hi' }, // show_command not set → defaults to false
      undefined,
      '.toml',
    );
    const promise = cmd.action!(
      null as never,
      '',
    ) as Promise<MessageActionReturn>;
    emit.stdout(Buffer.from('hi\n'));
    emit.close(0);

    const result = await promise;

    expect(result.content).not.toMatch(/^\$ /);
    expect(result.content).toContain('hi\n');
  });

  it('includes $ <command> prefix when show_command is explicitly true', async () => {
    const { proc, emit } = makeFakeProc();
    mockSpawn.mockReturnValue(proc);

    const cmd = createSlashCommandFromDefinition(
      '/base/greet.toml',
      '/base',
      { run: 'echo hi', show_command: true },
      undefined,
      '.toml',
    );
    const promise = cmd.action!(
      null as never,
      '',
    ) as Promise<MessageActionReturn>;
    emit.stdout(Buffer.from('hi\n'));
    emit.close(0);

    const result = await promise;

    expect(result.content).toContain('$ echo hi');
    expect(result.content).toContain('hi\n');
  });
});
