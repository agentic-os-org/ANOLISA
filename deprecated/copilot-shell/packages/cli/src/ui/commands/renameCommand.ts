/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import type { SlashCommand } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';

export const renameCommand: SlashCommand = {
  name: 'rename',
  altNames: ['name'],
  get description() {
    return t('Rename the current session');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context, args) => {
    const { config } = context.services;
    const trimmedArgs = args.trim();

    if (!config) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: t('No active session available.'),
      };
    }

    if (!trimmedArgs) {
      return;
    }

    const recordingService = config.getChatRecordingService();
    if (recordingService) {
      recordingService.recordSessionName(trimmedArgs);
    }

    return {
      type: 'message' as const,
      messageType: 'info' as const,
      content: t('Session name set to: {{name}}', { name: trimmedArgs }),
    };
  },
};
