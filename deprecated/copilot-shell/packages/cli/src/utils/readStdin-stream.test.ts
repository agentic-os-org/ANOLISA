/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { once } from 'node:events';
import { PassThrough } from 'node:stream';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { readStdin } from './readStdin.js';

const MAX_BYTES = 8 * 1024 * 1024;
let input: PassThrough;
let descriptor: PropertyDescriptor | undefined;

beforeEach(() => {
  descriptor = Object.getOwnPropertyDescriptor(process, 'stdin');
  input = new PassThrough();
  Object.defineProperty(process, 'stdin', { configurable: true, value: input });
  vi.spyOn(console, 'warn').mockImplementation(() => {});
});

afterEach(() => {
  input.destroy();
  Object.defineProperty(process, 'stdin', descriptor!);
  vi.restoreAllMocks();
});

function expectClean() {
  for (const event of ['readable', 'end', 'close', 'error']) {
    expect(input.listenerCount(event)).toBe(0);
  }
}

it('settles over-limit input when destroy emits close without end', async () => {
  let settled: string | undefined;
  const reading = readStdin();
  void reading.then((value) => {
    settled = value;
  });
  const closed = once(input, 'close');
  input.write('a'.repeat(MAX_BYTES) + 'overflow');
  await closed;
  await Promise.resolve();
  expect(settled?.length).toBe(MAX_BYTES);
  expect(console.warn).toHaveBeenCalledOnce();
  expectClean();
});

it('counts four-byte characters against the byte limit', async () => {
  const reading = readStdin();
  input.end('😀'.repeat(MAX_BYTES / 4 + 1));
  const result = await reading;
  expect(Buffer.byteLength(result, 'utf8')).toBe(MAX_BYTES);
  expect(result).toBe('😀'.repeat(MAX_BYTES / 4));
  expectClean();
});

it('does not return a partial character when only one byte remains', async () => {
  const reading = readStdin();
  input.end('a'.repeat(MAX_BYTES - 1) + '中');
  expect((await reading) === 'a'.repeat(MAX_BYTES - 1)).toBe(true);
  expectClean();
});

it('accumulates byte counts over several decoded chunks', async () => {
  const reading = readStdin();
  input.write('中'.repeat((MAX_BYTES - 2) / 3));
  await new Promise<void>((resolve) => setImmediate(resolve));
  input.end('😀tail');
  const result = await reading;
  expect(Buffer.byteLength(result, 'utf8')).toBe(MAX_BYTES - 2);
  expect(result.endsWith('中')).toBe(true);
  expectClean();
});

it('keeps exact-limit EOF input without a warning', async () => {
  const reading = readStdin();
  input.end('a'.repeat(MAX_BYTES));
  expect((await reading).length).toBe(MAX_BYTES);
  expect(console.warn).not.toHaveBeenCalled();
  expectClean();
});

it('settles a close before EOF and removes the no-input timer', async () => {
  vi.useFakeTimers();
  try {
    const reading = readStdin();
    input.destroy();
    expect(await reading).toBe('');
    expect(vi.getTimerCount()).toBe(0);
    expectClean();
  } finally {
    vi.useRealTimers();
  }
});

it('rejects stream errors without later close changing the outcome', async () => {
  const reading = readStdin();
  input.destroy(new Error('controlled input failure'));
  await expect(reading).rejects.toThrow('controlled input failure');
  expectClean();
});
