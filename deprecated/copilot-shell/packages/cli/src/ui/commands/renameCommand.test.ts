/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi, describe, it, expect, beforeEach } from 'vitest';
import { renameCommand } from './renameCommand.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';

describe('renameCommand', () => {
  let mockContext: ReturnType<typeof createMockCommandContext>;
  let mockRecordSessionName: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    mockRecordSessionName = vi.fn();
    vi.clearAllMocks();

    mockContext = createMockCommandContext({
      services: {
        config: {
          getChatRecordingService: () => ({
            recordSessionName: mockRecordSessionName,
          }),
        },
      },
    });
  });

  it('should set session name when args provided', async () => {
    if (!renameCommand.action)
      throw new Error('renameCommand must have an action.');

    const result = await renameCommand.action(mockContext, 'my session name');

    expect(mockRecordSessionName).toHaveBeenCalledWith('my session name');
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: 'Session name set to: my session name',
    });
  });

  it('should return undefined when no args provided (ghost hint shown in input)', async () => {
    if (!renameCommand.action)
      throw new Error('renameCommand must have an action.');

    const result = await renameCommand.action(mockContext, '');

    expect(mockRecordSessionName).not.toHaveBeenCalled();
    expect(result).toBeUndefined();
  });

  it('should handle missing config gracefully', async () => {
    if (!renameCommand.action)
      throw new Error('renameCommand must have an action.');

    const nullConfigContext = createMockCommandContext({
      services: { config: null },
    });

    const result = await renameCommand.action(nullConfigContext, 'test');

    expect(result).toEqual({
      type: 'message',
      messageType: 'error',
      content: 'No active session available.',
    });
  });
});
