/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { ktunerCommand } from './ktunerCommand.js';
import { CommandKind } from './types.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';
import {
  maybeRunKtunerFirstRunCheck,
  isKtunerAvailable,
  hasRunCheck,
} from '../../utils/ktunerFirstRun.js';

vi.mock('../../utils/ktunerFirstRun.js', () => ({
  maybeRunKtunerFirstRunCheck: vi.fn(),
  isKtunerAvailable: vi.fn(() => false),
  hasRunCheck: vi.fn(() => false),
}));

describe('ktunerCommand', () => {
  beforeEach(() => {
    vi.mocked(maybeRunKtunerFirstRunCheck).mockClear();
    vi.mocked(isKtunerAvailable).mockReset();
    vi.mocked(isKtunerAvailable).mockReturnValue(false);
    vi.mocked(hasRunCheck).mockReset();
    vi.mocked(hasRunCheck).mockReturnValue(false);
  });

  it('has the correct name and subcommands', () => {
    expect(ktunerCommand.name).toBe('ktuner');
    expect(ktunerCommand.kind).toBe(CommandKind.BUILT_IN);
    expect(ktunerCommand.subCommands).toHaveLength(2);
    const names = ktunerCommand.subCommands!.map((sc) => sc.name);
    expect(names).toContain('enable');
    expect(names).toContain('disable');
  });

  it('bare /ktuner shows current mode', () => {
    const ctx = createMockCommandContext({
      services: {
        settings: { merged: { general: { ktunerCheck: 'disabled' } } },
      },
    });
    const result = ktunerCommand.action!(ctx, '');
    expect(result).toEqual(
      expect.objectContaining({ type: 'message', messageType: 'info' }),
    );
    expect((result as { content: string }).content).toContain('disabled');
  });

  it('bare /ktuner defaults to "ask" when setting is unset', () => {
    const ctx = createMockCommandContext({
      services: { settings: { merged: {} } },
    });
    const result = ktunerCommand.action!(ctx, '');
    expect((result as { content: string }).content).toContain('ask');
  });

  it('/ktuner enable + available: sets setting, triggers check immediately', async () => {
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    const ctx = createMockCommandContext();
    const enableCmd = ktunerCommand.subCommands!.find(
      (c) => c.name === 'enable',
    )!;

    const result = await enableCmd.action!(ctx, '');

    expect(ctx.services.settings.setValue).toHaveBeenCalledWith(
      expect.anything(),
      'general.ktunerCheck',
      'enabled',
    );
    expect(maybeRunKtunerFirstRunCheck).toHaveBeenCalledTimes(1);
    expect(maybeRunKtunerFirstRunCheck).toHaveBeenCalledWith(ctx.ui.addItem);
    expect((result as { content: string }).content).toContain('running');
  });

  it('/ktuner enable + available + already ran: tells user check already completed, does NOT re-run', async () => {
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    vi.mocked(hasRunCheck).mockReturnValue(true);
    const ctx = createMockCommandContext();
    const enableCmd = ktunerCommand.subCommands!.find(
      (c) => c.name === 'enable',
    )!;

    const result = await enableCmd.action!(ctx, '');

    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect((result as { content: string }).content).toContain('already');
  });

  it('/ktuner enable + unavailable: sets setting, does NOT trigger check, tells user to re-run after install', async () => {
    vi.mocked(isKtunerAvailable).mockReturnValue(false);
    const ctx = createMockCommandContext();
    const enableCmd = ktunerCommand.subCommands!.find(
      (c) => c.name === 'enable',
    )!;

    const result = await enableCmd.action!(ctx, '');

    expect(ctx.services.settings.setValue).toHaveBeenCalledWith(
      expect.anything(),
      'general.ktunerCheck',
      'enabled',
    );
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect((result as { content: string }).content).toContain(
      'no trusted ktuner',
    );
    expect((result as { content: string }).content).toContain(
      '/ktuner enable again',
    );
  });

  it('/ktuner disable: sets setting to disabled', async () => {
    const ctx = createMockCommandContext();
    const disableCmd = ktunerCommand.subCommands!.find(
      (c) => c.name === 'disable',
    )!;

    const result = await disableCmd.action!(ctx, '');

    expect(ctx.services.settings.setValue).toHaveBeenCalledWith(
      expect.anything(),
      'general.ktunerCheck',
      'disabled',
    );
    expect((result as { content: string }).content).toContain('disabled');
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
  });
});
