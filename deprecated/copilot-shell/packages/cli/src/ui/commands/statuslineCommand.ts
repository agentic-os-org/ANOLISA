/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { SlashCommand, MessageActionReturn } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';
import { SettingScope } from '../../config/settings.js';

type StatusLineActionReturn =
  | MessageActionReturn
  | {
      type: 'submit_prompt';
      content: Array<{ text: string }>;
    };

const showSubCommand: SlashCommand = {
  name: 'show',
  get description() {
    return t('Show current status line configuration');
  },
  kind: CommandKind.BUILT_IN,
  action: (context): StatusLineActionReturn => {
    const currentConfig = context.services.settings.merged.ui?.statusLine as
      | { command: string }
      | undefined;
    if (currentConfig) {
      return {
        type: 'message',
        messageType: 'info',
        content: t('Current status line command: {{command}}', {
          command: currentConfig.command,
        }),
      };
    }
    return {
      type: 'message',
      messageType: 'info',
      content: t('No status line command is currently set.'),
    };
  },
};

const clearSubCommand: SlashCommand = {
  name: 'clear',
  altNames: ['off'],
  get description() {
    return t('Clear status line configuration');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context): Promise<StatusLineActionReturn> => {
    await context.services.settings.setValue(
      SettingScope.User,
      'ui.statusLine',
      undefined,
    );
    return {
      type: 'message',
      messageType: 'info',
      content: t('Status line command cleared.'),
    };
  },
};

export const statuslineCommand: SlashCommand = {
  name: 'statusline',
  get description() {
    return t("Set up Copilot Shell's status line UI");
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [showSubCommand, clearSubCommand],
  action: async (context, args): Promise<StatusLineActionReturn> => {
    const trimmedArgs = args.trim();

    // No arguments: show current configuration
    if (!trimmedArgs) {
      return showSubCommand.action!(context, '') as StatusLineActionReturn;
    }

    // Set the status line command
    const statusLineConfig = {
      type: 'command',
      command: trimmedArgs,
    };

    await context.services.settings.setValue(
      SettingScope.User,
      'ui.statusLine',
      statusLineConfig,
    );
    return {
      type: 'message',
      messageType: 'info',
      content: t('Status line command set to: {{command}}', {
        command: trimmedArgs,
      }),
    };
  },
};
