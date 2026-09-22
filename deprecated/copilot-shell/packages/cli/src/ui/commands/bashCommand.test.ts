/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach } from 'vitest';
import { bashCommand } from './bashCommand.js';
import { CommandKind, type CommandContext } from './types.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';

describe('bashCommand', () => {
  it('should have correct metadata', () => {
    expect(bashCommand.name).toBe('bash');
    expect(bashCommand.kind).toBe(CommandKind.BUILT_IN);
    expect(bashCommand.description).toBeTruthy();
  });

  describe('action', () => {
    let context: CommandContext;

    beforeEach(() => {
      context = createMockCommandContext({
        executionMode: 'interactive',
      });
    });

    it('should return spawn_shell with default bash', () => {
      const result = bashCommand.action!(context, '');
      expect(result).toEqual({ type: 'spawn_shell', shell: 'bash' });
    });

    it('should return spawn_shell with custom shell from args', () => {
      const result = bashCommand.action!(context, '/bin/zsh');
      expect(result).toEqual({ type: 'spawn_shell', shell: '/bin/zsh' });
    });

    it('should trim whitespace from args', () => {
      const result = bashCommand.action!(context, '  /bin/zsh  ');
      expect(result).toEqual({ type: 'spawn_shell', shell: '/bin/zsh' });
    });

    it('should return error message in non-interactive mode', () => {
      const nonInteractiveContext = createMockCommandContext({
        executionMode: 'non_interactive',
      });
      const result = bashCommand.action!(nonInteractiveContext, '');
      expect(result).toMatchObject({ type: 'message', messageType: 'error' });
    });

    it('should return error message in acp mode', () => {
      const acpContext = createMockCommandContext({
        executionMode: 'acp',
      });
      const result = bashCommand.action!(acpContext, '');
      expect(result).toMatchObject({ type: 'message', messageType: 'error' });
    });

    it('should work when executionMode is undefined (default interactive)', () => {
      const defaultContext = createMockCommandContext();
      const result = bashCommand.action!(defaultContext, '');
      expect(result).toEqual({ type: 'spawn_shell', shell: 'bash' });
    });
  });
});
