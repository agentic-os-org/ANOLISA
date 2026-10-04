/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockSetValue = vi.hoisted(() => vi.fn());
const mockForScope = vi.hoisted(() => vi.fn());
const mockLoadSettings = vi.hoisted(() => vi.fn());

vi.mock('../../config/settings.js', () => ({
  loadSettings: mockLoadSettings,
  SettingScope: {
    User: 'USER',
    Workspace: 'WORKSPACE',
    System: 'SYSTEM',
    SystemDefaults: 'SYSTEM_DEFAULTS',
  },
}));

import { handleEnableHook } from './enable.js';
import { handleDisableHook } from './disable.js';

/**
 * Creates a fake LoadedSettings whose user scope defines hooks while the
 * workspace scope holds (or lacks) its own hooks settings. `forScope` is
 * wired to return the workspace-scope settings object.
 */
function makeSettings(
  userHooks: Record<string, unknown> | undefined,
  workspaceHooks: Record<string, unknown> | undefined,
): void {
  const workspaceSettings: Record<string, unknown> = {};
  if (workspaceHooks !== undefined) {
    workspaceSettings['hooks'] = workspaceHooks;
  }
  // Emulate the merged view the runtime consumes: event arrays concat
  // (MergeStrategy.CONCAT), other keys such as `disabled` replace
  // (workspace wins) per the settings deep-merge defaults.
  const mergedHooks: Record<string, unknown> = { ...userHooks };
  for (const [key, value] of Object.entries(workspaceHooks ?? {})) {
    if (
      Array.isArray(value) &&
      Array.isArray(mergedHooks[key]) &&
      key !== 'disabled'
    ) {
      mergedHooks[key] = [...(mergedHooks[key] as unknown[]), ...value];
    } else {
      mergedHooks[key] = value;
    }
  }
  mockForScope.mockReturnValue({ settings: workspaceSettings });
  mockLoadSettings.mockReturnValue({
    merged: { hooks: mergedHooks },
    forScope: mockForScope,
    setValue: mockSetValue,
  });
}

describe('hooks enable/disable scope handling', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockSetValue.mockReturnValue(undefined);
    mockForScope.mockReset();
    mockForScope.mockReturnValue({ settings: {} });
  });

  describe('handleDisableHook', () => {
    it('writes only the workspace hooks, without duplicating user-scope hook definitions', async () => {
      makeSettings(
        { PreToolUse: [{ matcher: '*', hooks: [{ command: 'echo user' }] }] },
        {}, // workspace has an empty hooks object
      );

      await handleDisableHook('my-hook');

      expect(mockSetValue).toHaveBeenCalledTimes(1);
      const [scope, key, value] = mockSetValue.mock.calls[0];
      expect(scope).toBe('WORKSPACE');
      expect(key).toBe('hooks');
      expect(value).toEqual({ disabled: ['my-hook'] });
      // User-scope definitions must not be copied into the workspace file:
      // event arrays merge with CONCAT, so a copy would make every user
      // hook run twice at runtime.
      expect(JSON.stringify(value)).not.toContain('PreToolUse');
    });

    it('keeps existing workspace hooks entries when disabling', async () => {
      makeSettings(
        { PreToolUse: [{ matcher: '*' }] },
        {
          disabled: ['existing-hook'],
          PostToolUse: [{ matcher: 'Write' }],
        },
      );

      await handleDisableHook('my-hook');

      const [, , value] = mockSetValue.mock.calls[0];
      expect(value).toEqual({
        disabled: ['existing-hook', 'my-hook'],
        PostToolUse: [{ matcher: 'Write' }],
      });
    });

    it('starts a workspace hooks object when none exists, without user data', async () => {
      makeSettings(
        {
          disabled: ['shared-hook'],
          PreToolUse: [{ matcher: '*' }],
        },
        undefined, // workspace has no hooks key at all
      );

      await handleDisableHook('my-hook');

      const [scope, , value] = mockSetValue.mock.calls[0];
      expect(scope).toBe('WORKSPACE');
      expect(value).toEqual({ disabled: ['my-hook'] });
      expect(JSON.stringify(value)).not.toContain('PreToolUse');
      expect(JSON.stringify(value)).not.toContain('shared-hook');
    });
  });

  describe('handleEnableHook', () => {
    it('removes the hook from the workspace disabled list without copying user definitions', async () => {
      makeSettings(
        { PreToolUse: [{ matcher: '*' }] },
        { disabled: ['my-hook'] },
      );

      await handleEnableHook('my-hook');

      expect(mockSetValue).toHaveBeenCalledTimes(1);
      const [scope, key, value] = mockSetValue.mock.calls[0];
      expect(scope).toBe('WORKSPACE');
      expect(key).toBe('hooks');
      expect(value).toEqual({ disabled: [] });
      expect(JSON.stringify(value)).not.toContain('PreToolUse');
    });

    it('keeps other workspace disabled entries when enabling one hook', async () => {
      makeSettings(
        { disabled: ['user-disabled'] },
        { disabled: ['my-hook', 'other-hook'] },
      );

      await handleEnableHook('my-hook');

      const [, , value] = mockSetValue.mock.calls[0];
      expect(value).toEqual({ disabled: ['other-hook'] });
    });

    it('does not rewrite the workspace file when the hook is disabled only in user scope', async () => {
      makeSettings(
        { disabled: ['my-hook'] }, // disabled at USER scope only
        { disabled: ['another-hook'] },
      );

      await handleEnableHook('my-hook');

      // The workspace scope cannot enable a hook that the user scope
      // disabled; writing the merged list would only leak user entries.
      expect(mockSetValue).not.toHaveBeenCalled();
    });
  });
});
