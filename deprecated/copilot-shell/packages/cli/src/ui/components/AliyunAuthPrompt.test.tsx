/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import { act } from 'react';
import { render } from 'ink-testing-library';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import chalk from 'chalk';
import { AliyunAuthPrompt } from './AliyunAuthPrompt.js';
import type { Key } from '../hooks/useKeypress.js';
import { useKeypress } from '../hooks/useKeypress.js';

vi.mock('../hooks/useKeypress.js', () => ({
  useKeypress: vi.fn(),
}));

vi.mock('@copilot-shell/core', async () => {
  const actual = await vi.importActual<typeof import('@copilot-shell/core')>(
    '@copilot-shell/core',
  );
  return {
    ...actual,
    // Force non-ECS path so the component lands in the AK/SK input step.
    getECSInstanceId: vi.fn(async () => null),
    getECSRegionId: vi.fn(async () => null),
    generateConsoleUrl: vi.fn(() => ''),
    pollForECSRamRoleAuthorization: vi.fn(async () => false),
    getECSRamRoleCredentials: vi.fn(async () => null),
  };
});

function makeKey(overrides: Partial<Key> = {}): Key {
  return {
    name: '',
    ctrl: false,
    meta: false,
    shift: false,
    paste: false,
    sequence: '',
    ...overrides,
  };
}

function latestHandler(): (key: Key) => void {
  const mock = vi.mocked(useKeypress);
  return mock.mock.calls[mock.mock.calls.length - 1]![0];
}

async function pressKey(key: Partial<Key>): Promise<void> {
  await act(() => {
    latestHandler()(makeKey(key));
  });
}

/**
 * Wait until the AK/SK input step is rendered. The detect-environment
 * useEffect resolves through the mocked `getECSInstanceId(null)` path and
 * advances state into `aksk_input`, at which point "Access Key ID:" appears.
 *
 * Polls microtasks deterministically instead of a fixed-duration sleep.
 */
async function waitForAkskInput(
  lastFrame: () => string | undefined,
  maxTicks = 50,
): Promise<void> {
  for (let i = 0; i < maxTicks; i++) {
    if (lastFrame()?.includes('Access Key ID:')) return;
    await act(async () => {
      await Promise.resolve();
    });
  }
  throw new Error(
    `AK/SK input step did not render within ${maxTicks} microtask ticks`,
  );
}

describe('AliyunAuthPrompt cursor rendering', () => {
  // chalk is a process-wide singleton; save/restore so we don't leak ANSI
  // settings into unrelated tests sharing the same Vitest worker.
  let originalChalkLevel: typeof chalk.level;

  beforeEach(() => {
    vi.clearAllMocks();
    // Force chalk to emit ANSI codes so the cursor is distinguishable from padding.
    originalChalkLevel = chalk.level;
    chalk.level = 1;
  });

  afterEach(() => {
    chalk.level = originalChalkLevel;
  });

  it('renders cursor on active accessKeyId field when empty', async () => {
    const { lastFrame } = render(
      <AliyunAuthPrompt
        isAuthenticating={false}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    await waitForAkskInput(lastFrame);

    // Default field is accessKeyId, empty → only the inverse cursor
    expect(lastFrame()).toContain('Access Key ID:');
    expect(lastFrame()).toContain(chalk.inverse(' '));
  });

  it('renders cursor at end of accessKeyId after typing', async () => {
    const { lastFrame } = render(
      <AliyunAuthPrompt
        isAuthenticating={false}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    await waitForAkskInput(lastFrame);

    for (const ch of ['L', 'T', 'A', 'I']) {
      await pressKey({ sequence: ch });
    }

    expect(lastFrame()).toContain(`LTAI${chalk.inverse(' ')}`);
  });

  it('moves cursor to model field after navigating past AK/SK', async () => {
    const { lastFrame } = render(
      <AliyunAuthPrompt
        isAuthenticating={false}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultModel="qwen3-coder-plus"
      />,
    );
    await waitForAkskInput(lastFrame);

    // accessKeyId → accessKeySecret → model via Tab
    await pressKey({ name: 'tab', sequence: '\t' });
    await pressKey({ name: 'tab', sequence: '\t' });

    expect(lastFrame()).toContain(`qwen3-coder-plus${chalk.inverse(' ')}`);
  });

  it('shows cursor only on the active field, not on inactive ones', async () => {
    const { lastFrame } = render(
      <AliyunAuthPrompt
        isAuthenticating={false}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultModel="qwen3-coder-plus"
      />,
    );
    await waitForAkskInput(lastFrame);

    // Active is accessKeyId; model is shown but inactive → only one inverse space (the cursor)
    const frame = lastFrame()!;
    const cursorOccurrences = frame.split(chalk.inverse(' ')).length - 1;
    expect(cursorOccurrences).toBe(1);
  });
});
