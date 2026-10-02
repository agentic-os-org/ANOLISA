/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi, describe, it, expect } from 'vitest';
import { hooksCommand } from './hooksCommand.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';
import {
  type HookRegistryEntry,
  HookType,
  HooksConfigSource,
  HookEventName,
} from '@copilot-shell/core';
import type {
  MessageActionReturn,
  MultiSelectHooksActionReturn,
} from './types.js';

function makeHook(
  name: string,
  enabled: boolean,
  eventName: HookEventName = HookEventName.PreToolUse,
): HookRegistryEntry {
  return {
    config: { type: HookType.Command, command: `run-${name}`, name },
    source: HooksConfigSource.Project,
    eventName,
    enabled,
  };
}

function createHookContext(hooks: HookRegistryEntry[]) {
  const setHookEnabled = vi.fn();
  return {
    context: createMockCommandContext({
      services: {
        config: {
          getHookSystem: () => ({
            getRegistry: () => ({
              getAllHooks: () => hooks,
              setHookEnabled,
            }),
          }),
        },
      },
    }),
    setHookEnabled,
  };
}

describe('hooksCommand', () => {
  describe('disable subcommand', () => {
    it('should disable a single hook', async () => {
      const hooks = [makeHook('lint', true), makeHook('test', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable lint',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('info');
      expect(result.content).toContain('lint');
      expect(result.content).toContain('disabled');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', false);
      expect(setHookEnabled).toHaveBeenCalledTimes(1);
    });

    it('should disable multiple space-separated hooks', async () => {
      const hooks = [
        makeHook('lint', true),
        makeHook('test', true),
        makeHook('format', true),
      ];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable lint test',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('info');
      expect(result.content).toContain('2 hooks');
      expect(result.content).toContain('lint');
      expect(result.content).toContain('test');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', false);
      expect(setHookEnabled).toHaveBeenCalledWith('test', false);
      expect(setHookEnabled).toHaveBeenCalledTimes(2);
    });

    it('should disable multiple comma-separated hooks', async () => {
      const hooks = [makeHook('lint', true), makeHook('test', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable lint,test',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.content).toContain('2 hooks');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', false);
      expect(setHookEnabled).toHaveBeenCalledWith('test', false);
    });

    it('should deduplicate hook names', async () => {
      const hooks = [makeHook('lint', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable lint lint lint',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.content).toContain('lint');
      expect(result.content).toContain('disabled');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', false);
      expect(setHookEnabled).toHaveBeenCalledTimes(1);
    });

    it('should handle mixed separators', async () => {
      const hooks = [
        makeHook('a', true),
        makeHook('b', true),
        makeHook('c', true),
      ];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable a,b c',
      )) as MessageActionReturn;

      expect(result.content).toContain('3 hooks');
      expect(setHookEnabled).toHaveBeenCalledWith('a', false);
      expect(setHookEnabled).toHaveBeenCalledWith('b', false);
      expect(setHookEnabled).toHaveBeenCalledWith('c', false);
    });

    it('should report unknown hooks without toggling them', async () => {
      const hooks = [makeHook('test', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable lin test',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('info');
      expect(result.content).toContain('test');
      expect(result.content).toContain('Unknown hooks: lin');
      expect(setHookEnabled).toHaveBeenCalledWith('test', false);
      expect(setHookEnabled).not.toHaveBeenCalledWith('lin', false);
      expect(setHookEnabled).toHaveBeenCalledTimes(1);
    });

    it('should return an error when all requested hooks are unknown', async () => {
      const hooks = [makeHook('test', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable missing',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('error');
      expect(result.content).toContain('No matching hooks found: missing');
      expect(setHookEnabled).not.toHaveBeenCalled();
    });

    it('should return multi_select_hooks when no args and there are enabled hooks', async () => {
      const hooks = [
        makeHook('lint', true),
        makeHook('test', true),
        makeHook('disabled-one', false),
      ];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable',
      )) as MultiSelectHooksActionReturn;

      expect(result.type).toBe('multi_select_hooks');
      expect(result.hookNames).toContain('lint');
      expect(result.hookNames).toContain('test');
      expect(result.hookNames).not.toContain('disabled-one');
      expect(typeof result.onSelected).toBe('function');

      const msg = result.onSelected(['lint', 'test']);
      expect(msg.content).toContain('2 hooks');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', false);
      expect(setHookEnabled).toHaveBeenCalledWith('test', false);
    });

    it('should return info when no args and no enabled hooks', async () => {
      const hooks = [makeHook('lint', false)];
      const { context } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('info');
      expect(result.content).toContain('No enabled hooks');
    });

    it('should treat full args as single name when it matches a known hook with spaces', async () => {
      const hooks: HookRegistryEntry[] = [
        {
          config: { type: HookType.Command, command: 'echo test' },
          source: HooksConfigSource.Project,
          eventName: HookEventName.PreToolUse,
          enabled: true,
        },
      ];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'disable echo test',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.content).toContain('echo test');
      expect(result.content).toContain('disabled');
      expect(setHookEnabled).toHaveBeenCalledWith('echo test', false);
      expect(setHookEnabled).toHaveBeenCalledTimes(1);
    });

    it('should return error when hooks are not enabled', async () => {
      const context = createMockCommandContext({
        services: {
          config: {
            getHookSystem: () => null,
          },
        },
      });

      const result = (await hooksCommand.action!(
        context,
        'disable lint',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('error');
    });
  });

  describe('enable subcommand', () => {
    it('should enable a single hook', async () => {
      const hooks = [makeHook('lint', false)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'enable lint',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.content).toContain('lint');
      expect(result.content).toContain('enabled');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', true);
    });

    it('should enable multiple hooks', async () => {
      const hooks = [makeHook('lint', false), makeHook('test', false)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'enable lint test',
      )) as MessageActionReturn;

      expect(result.content).toContain('2 hooks');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', true);
      expect(setHookEnabled).toHaveBeenCalledWith('test', true);
    });

    it('should return multi_select_hooks when no args and there are disabled hooks', async () => {
      const hooks = [makeHook('lint', false), makeHook('test', true)];
      const { context, setHookEnabled } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'enable',
      )) as MultiSelectHooksActionReturn;

      expect(result.type).toBe('multi_select_hooks');
      expect(result.hookNames).toContain('lint');
      expect(result.hookNames).not.toContain('test');
      expect(typeof result.onSelected).toBe('function');

      const msg = result.onSelected(['lint']);
      expect(msg.content).toContain('lint');
      expect(setHookEnabled).toHaveBeenCalledWith('lint', true);
    });

    it('should return info when no args and no disabled hooks', async () => {
      const hooks = [makeHook('lint', true)];
      const { context } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'enable',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.messageType).toBe('info');
      expect(result.content).toContain('No disabled hooks');
    });
  });

  describe('list subcommand', () => {
    it('should list all hooks', async () => {
      const hooks = [
        makeHook('lint', true),
        makeHook('test', false, HookEventName.PostToolUse),
      ];
      const { context } = createHookContext(hooks);

      const result = (await hooksCommand.action!(
        context,
        'list',
      )) as MessageActionReturn;

      expect(result.type).toBe('message');
      expect(result.content).toContain('lint');
      expect(result.content).toContain('test');
      expect(result.content).toContain('Enabled');
      expect(result.content).toContain('Disabled');
    });
  });

  describe('completion', () => {
    it('should suggest enabled hooks for disable', async () => {
      const hooks = [
        makeHook('lint', true),
        makeHook('test', true),
        makeHook('disabled-one', false),
      ];
      const { context } = createHookContext(hooks);

      const completions = await hooksCommand.completion!(context, 'disable ');

      expect(completions).toContain('lint');
      expect(completions).toContain('test');
      expect(completions).not.toContain('disabled-one');
    });

    it('should filter out already typed hooks from completions', async () => {
      const hooks = [
        makeHook('lint', true),
        makeHook('test', true),
        makeHook('format', true),
      ];
      const { context } = createHookContext(hooks);

      const completions = await hooksCommand.completion!(
        context,
        'disable lint ',
      );

      expect(completions).not.toContain('lint');
      expect(completions).toContain('test');
      expect(completions).toContain('format');
    });

    it('should suggest disabled hooks for enable', async () => {
      const hooks = [makeHook('lint', false), makeHook('test', true)];
      const { context } = createHookContext(hooks);

      const completions = await hooksCommand.completion!(context, 'enable ');

      expect(completions).toContain('lint');
      expect(completions).not.toContain('test');
    });
  });
});
