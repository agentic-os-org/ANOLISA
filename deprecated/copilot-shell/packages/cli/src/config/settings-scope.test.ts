/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import {
  LoadedSettings,
  SettingScope,
  withScopedHooks,
  type Settings,
  type SettingsFile,
} from './settings.js';

/** A scope file holding `settings`, with the on-disk copy as the same object. */
function scopeFile(settings: Settings): SettingsFile {
  return {
    path: `/mock/${Math.random().toString(36).slice(2)}.json`,
    settings,
    originalSettings: structuredClone(settings),
  } as SettingsFile;
}

function loaded(parts: {
  system?: Settings;
  systemDefaults?: Settings;
  user?: Settings;
  workspace?: Settings;
}): LoadedSettings {
  return new LoadedSettings(
    scopeFile(parts.system ?? {}),
    scopeFile(parts.systemDefaults ?? {}),
    scopeFile(parts.user ?? {}),
    scopeFile(parts.workspace ?? {}),
    true,
    new Set(),
  );
}

const USER_HOOK = {
  hooks: [
    {
      type: 'command' as const,
      name: 'user-hook',
      // Already env-resolved by the loader: `settings.merged` therefore cannot
      // be written back to disk without freezing the absolute path.
      command: '/home/alice/bin/hook.sh',
    },
  ],
};

/**
 * `setValue` assigns the whole value it is given, so a value derived from
 * `settings.merged` copies every other scope's hooks into the workspace file.
 * Those hooks then appear in two scopes at once and - because the hook arrays use
 * `MergeStrategy.CONCAT` - fire twice. `withScopedHooks` is the value both
 * `/hooks disable` and `/hooks enable` persist, so it is what these tests drive.
 */
describe('hooks persistence keeps to one scope', () => {
  it('does not copy another scope\u2019s hooks into the workspace file', () => {
    const settings = loaded({
      system: { hooks: { PostToolUse: [USER_HOOK] } } as unknown as Settings,
      workspace: { hooks: { disabled: [] } } as unknown as Settings,
    });
    const workspaceHooks = (
      settings.workspace.settings as Record<string, unknown>
    )['hooks'] as Record<string, unknown>;

    const written = withScopedHooks<Record<string, unknown>>(workspaceHooks, [
      'some-hook',
    ]);
    settings.setValue(SettingScope.Workspace, 'hooks' as never, written as never);

    const onDisk = settings.workspace.settings as Record<string, unknown>;
    expect((onDisk['hooks'] as Record<string, unknown>)['PostToolUse']).toBeUndefined();
    expect(onDisk['hooks']).toMatchObject({ disabled: ['some-hook'] });
  });

  it('does not freeze an env-resolved command from another scope', () => {
    const settings = loaded({
      user: { hooks: { PreToolUse: [USER_HOOK] } } as unknown as Settings,
      workspace: {} as unknown as Settings,
    });

    settings.setValue(
      SettingScope.Workspace,
      'hooks' as never,
      withScopedHooks<Record<string, unknown>>(undefined, ['some-hook']) as never,
    );

    const serialized = JSON.stringify(settings.workspace.settings);
    expect(serialized).not.toContain('/home/alice/bin/hook.sh');
  });

  it('keeps the workspace file\u2019s own hooks while updating disabled', () => {
    const own = {
      hooks: [
        { type: 'command' as const, name: 'project-hook', command: '$HOME/p.sh' },
      ],
    };
    const settings = loaded({
      user: { hooks: { PreToolUse: [USER_HOOK] } } as unknown as Settings,
      workspace: { hooks: { ...own, disabled: ['old'] } } as unknown as Settings,
    });
    const workspaceHooks = (
      settings.workspace.settings as Record<string, unknown>
    )['hooks'] as Record<string, unknown>;

    settings.setValue(
      SettingScope.Workspace,
      'hooks' as never,
      withScopedHooks<Record<string, unknown>>(workspaceHooks, [
        'old',
        'new',
      ]) as never,
    );

    const written = (settings.workspace.settings as Record<string, unknown>)[
      'hooks'
    ] as Record<string, unknown>;
    expect(written).toMatchObject({ disabled: ['old', 'new'] });
    expect(written['hooks']).toEqual(own.hooks);
    // The project's own unexpanded form is preserved verbatim.
    expect(JSON.stringify(written)).toContain('$HOME/p.sh');
  });
});
