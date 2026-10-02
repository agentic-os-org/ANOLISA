/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi } from 'vitest';
import { statuslineCommand } from './statuslineCommand.js';
import { CommandKind } from './types.js';

describe('statuslineCommand', () => {
  it('should have the correct name and description', () => {
    expect(statuslineCommand.name).toBe('statusline');
    expect(statuslineCommand.description).toBeDefined();
    expect(statuslineCommand.kind).toBe(CommandKind.BUILT_IN);
  });

  it('should register show and clear subCommands', () => {
    expect(statuslineCommand.subCommands).toBeDefined();
    expect(statuslineCommand.subCommands).toHaveLength(2);

    const names = statuslineCommand.subCommands!.map((sc) => sc.name);
    expect(names).toContain('show');
    expect(names).toContain('clear');
  });

  it('clear subCommand should have "off" as altName', () => {
    const clearCmd = statuslineCommand.subCommands!.find(
      (sc) => sc.name === 'clear',
    );
    expect(clearCmd).toBeDefined();
    expect(clearCmd!.altNames).toContain('off');
  });

  it('subCommands should have descriptions', () => {
    for (const sub of statuslineCommand.subCommands!) {
      expect(sub.description).toBeDefined();
      expect(sub.description.length).toBeGreaterThan(0);
    }
  });

  it('subCommands should have actions', () => {
    for (const sub of statuslineCommand.subCommands!) {
      expect(sub.action).toBeDefined();
      expect(typeof sub.action).toBe('function');
    }
  });

  it('show subCommand should return current config when set', () => {
    const showCmd = statuslineCommand.subCommands!.find(
      (sc) => sc.name === 'show',
    );
    const context = {
      services: {
        settings: {
          merged: {
            ui: { statusLine: { command: 'echo hello' } },
          },
        },
      },
    };
    const result = showCmd!.action!(context as never, '');
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: expect.stringContaining('echo hello'),
    });
  });

  it('show subCommand should report no config when unset', () => {
    const showCmd = statuslineCommand.subCommands!.find(
      (sc) => sc.name === 'show',
    );
    const context = {
      services: {
        settings: {
          merged: { ui: {} },
        },
      },
    };
    const result = showCmd!.action!(context as never, '');
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: expect.stringContaining('No status line'),
    });
  });

  it('clear subCommand should clear the config', async () => {
    const clearCmd = statuslineCommand.subCommands!.find(
      (sc) => sc.name === 'clear',
    );
    const setValue = vi.fn();
    const context = {
      services: {
        settings: {
          setValue,
          merged: { ui: { statusLine: { command: 'echo hello' } } },
        },
      },
    };
    const result = await clearCmd!.action!(context as never, '');
    expect(setValue).toHaveBeenCalledWith('User', 'ui.statusLine', undefined);
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: expect.stringContaining('cleared'),
    });
  });

  it('parent action with no args should delegate to show', async () => {
    const context = {
      services: {
        settings: {
          merged: { ui: {} },
        },
      },
    };
    const result = await statuslineCommand.action!(context as never, '');
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: expect.stringContaining('No status line'),
    });
  });

  it('parent action with args should set the command', async () => {
    const setValue = vi.fn();
    const context = {
      services: {
        settings: {
          setValue,
          merged: { ui: {} },
        },
      },
    };
    const result = await statuslineCommand.action!(
      context as never,
      'echo "test"',
    );
    expect(setValue).toHaveBeenCalledWith('User', 'ui.statusLine', {
      type: 'command',
      command: 'echo "test"',
    });
    expect(result).toEqual({
      type: 'message',
      messageType: 'info',
      content: expect.stringContaining('echo "test"'),
    });
  });
});
