/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { CommandModule } from 'yargs';
import { createDebugLogger, getErrorMessage } from '@copilot-shell/core';
import {
  loadSettings,
  SettingScope,
  withScopedHooks,
} from '../../config/settings.js';

const debugLogger = createDebugLogger('HOOKS_DISABLE');

interface DisableArgs {
  hookName: string;
}

/**
 * Disable a hook by adding it to the disabled list
 */
export async function handleDisableHook(hookName: string): Promise<void> {
  const workingDir = process.cwd();
  const settings = loadSettings(workingDir);

  try {
    // Read the disabled list from the merged view (any scope may have populated
    // it) but write back only the workspace scope's own hooks object: writing the
    // merged object copies every other scope's hooks into this file, and those
    // hooks then merge with themselves and fire twice. The hooks come from
    // originalSettings - the settings copy is already env-resolved, so its
    // `$HOME/...` commands would freeze to absolute paths on disk.
    const mergedSettings = settings.merged as
      | Record<string, unknown>
      | undefined;
    const hooksSettings = (mergedSettings?.['hooks'] || {}) as Record<
      string,
      unknown
    >;
    const disabledHooks = (hooksSettings['disabled'] || []) as string[];

    // Check if hook is already disabled
    if (disabledHooks.includes(hookName)) {
      debugLogger.info(`Hook "${hookName}" is already disabled.`);
      return;
    }

    // Add hook to disabled list
    const newDisabledHooks = [...disabledHooks, hookName];
    const workspaceHooks = (
      settings.workspace.originalSettings as Record<string, unknown>
    )['hooks'] as Record<string, unknown> | undefined;
    const newHooksSettings = withScopedHooks(workspaceHooks, newDisabledHooks);

    // Save updated settings
    settings.setValue(
      SettingScope.Workspace,
      'hooks' as keyof typeof settings.merged,
      newHooksSettings as never,
    );

    debugLogger.info(`✓ Hook "${hookName}" has been disabled.`);
  } catch (error) {
    debugLogger.error(`Error disabling hook: ${getErrorMessage(error)}`);
  }
}

export const disableCommand: CommandModule = {
  command: 'disable <hook-name>',
  describe: 'Disable an active hook',
  builder: (yargs) =>
    yargs.positional('hook-name', {
      describe: 'Name of the hook to disable',
      type: 'string',
      demandOption: true,
    }),
  handler: async (argv) => {
    const args = argv as unknown as DisableArgs;
    await handleDisableHook(args.hookName);
    process.exit(0);
  },
};
